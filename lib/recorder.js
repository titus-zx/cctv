'use strict';
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
const { DriveUploader } = require('./drive');
const { FolderUploader } = require('./folder');
// Upload queue shared by all cameras: one file at a time, retried with backoff, deleted after success.
class UploadQueue {
  constructor(uploader, { keepLocal = false, log = console } = {}) {
    Object.assign(this, { uploader, keepLocal, log }); this.pending = []; this.queued = new Set(); this.running = false;
    this.uploaded = 0; this.lastError = null;
  }
  add(file) {
    if (this.queued.has(file)) return;
    this.queued.add(file); this.pending.push({file, attempts: 0}); this.run();
  }
  async run() {
    if (this.running) return; this.running = true;
    while (this.pending.length) {
      const job = this.pending.shift();
      try {
        await this.uploader.upload(job.file); this.uploaded++; this.lastError = null; this.queued.delete(job.file);
        if (!this.keepLocal) await fs.promises.unlink(job.file).catch(() => {});
      } catch (error) {
        this.lastError = error.message;
        if (error.code === 'ENOENT') { this.queued.delete(job.file); continue; }
        this.log.error(`Upload ${path.basename(job.file)} gagal: ${error.message}`);
        job.attempts++;
        await new Promise(resolve => setTimeout(resolve, Math.min(60000, 2000 * 2 ** job.attempts)).unref?.());
        this.pending.push(job); // keep retrying; segments stay on disk until uploaded
      }
    }
    this.running = false;
  }
}
// Cuts the camera's stream into fixed-length MP4 files (stream copy, no re-encoding) and hands them to the queue.
class Recorder {
  constructor(camera, { dir, segmentSeconds = 60, queue }) {
    Object.assign(this, { camera, dir, segmentSeconds, queue }); this.process = null; this.segments = 0; this.restarts = 0;
  }
  start(codec) { // codec: 'h264' | 'hevc' for raw pipe input; null => pull RTSP directly
    if (this.process) return;
    fs.mkdirSync(this.dir, {recursive: true, mode: 0o700});
    const input = codec
      ? ['-use_wallclock_as_timestamps', '1', '-fflags', '+genpts', '-f', codec, '-i', 'pipe:0']
      : ['-rtsp_transport', 'tcp', '-rw_timeout', '8000000', '-i', this.camera.rtspUrl];
    const prefix = `cam${String(this.camera.deviceId || 'rtsp').replace(/\D/g, '') || 'rtsp'}`;
    const args = ['-hide_banner', '-loglevel', 'error', ...input, '-an', '-c:v', 'copy', '-f', 'segment',
      '-segment_time', String(this.segmentSeconds), '-segment_format', 'mp4', '-reset_timestamps', '1',
      '-strftime', '1', '-segment_list', 'pipe:1', '-segment_list_type', 'flat', `${prefix}_%Y%m%d_%H%M%S.mp4`];
    const proc = this.process = spawn(process.env.FFMPEG_PATH || 'ffmpeg', args, {cwd: this.dir, stdio: ['pipe', 'pipe', 'pipe']});
    proc.stderr.on('data', () => {}); // may contain RTSP credentials
    proc.stdin.on('error', () => {});
    let text = '';
    proc.stdout.on('data', chunk => { // one finished segment name per line
      text += chunk; let nl;
      while ((nl = text.indexOf('\n')) >= 0) {
        const name = text.slice(0, nl).trim(); text = text.slice(nl + 1);
        if (/^[\w.-]+\.mp4$/.test(name)) { this.segments++; this.queue.add(path.join(this.dir, name)); }
      }
    });
    proc.on('error', () => { if (this.process === proc) this.process = null; });
    proc.on('close', () => { if (this.process === proc) this.process = null; });
  }
  write(data) {
    const stdin = this.process?.stdin;
    if (!stdin || stdin.destroyed || stdin.writableLength > 8 * 1024 * 1024) return;
    stdin.write(data);
  }
  stop() { // closing stdin lets FFmpeg finalize the partial last segment, which is then uploaded too
    const proc = this.process; this.process = null; if (!proc) return;
    proc.stdin.end(); setTimeout(() => proc.kill('SIGTERM'), 5000).unref();
  }
}
function createRecording(cfg, baseDir) {
  const rec = cfg.recording;
  if (!rec?.enabled) return null;
  const dir = path.resolve(baseDir, rec.dir || 'recordings');
  // `destination` picks exactly one target; the other's settings may stay in the config, unused.
  const destination = rec.destination || 'gdrive';
  let uploader;
  if (destination === 'gdrive') uploader = new DriveUploader(rec.drive || {});
  else if (destination === 'onedrive') uploader = new FolderUploader(rec.onedrive || {});
  else if (destination === 'local') uploader = {upload: async () => {}};
  else throw new Error('recording.destination harus "gdrive", "onedrive", atau "local".');
  const queue = new UploadQueue(uploader, {keepLocal: rec.keepLocal || destination === 'local'});
  // Files left from a previous run (crash, offline) go first.
  try { for (const f of fs.readdirSync(dir)) if (/^cam[\w]+_\d{8}_\d{6}\.mp4$/.test(f)) queue.add(path.join(dir, f)); } catch {}
  const seconds = Number(rec.segmentSeconds || 60);
  return {queue, make: camera => camera.record === false ? null : new Recorder(camera, {dir, segmentSeconds: seconds, queue})};
}
module.exports = { Recorder, UploadQueue, createRecording };
