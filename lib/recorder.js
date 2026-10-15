'use strict';
const { spawn } = require('node:child_process');
const { EventEmitter } = require('node:events');
const { mjpegEncoding } = require('./video');
const fs = require('node:fs');
const path = require('node:path');
const { Rclone } = require('./rclone');
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
// Check top-level MP4 atoms without loading video data into memory.
function isFinalizedMp4(file) {
  let fd;
  try {
    fd = fs.openSync(file, 'r');
    const size = fs.fstatSync(fd).size, header = Buffer.alloc(16);
    let offset = 0, moov = false, mdat = false;
    while (offset + 8 <= size) {
      if (fs.readSync(fd, header, 0, Math.min(16, size - offset), offset) < 8) return false;
      let length = header.readUInt32BE(0), minimum = 8;
      const type = header.toString('ascii', 4, 8);
      if (length === 1) {
        if (offset + 16 > size) return false;
        const large = header.readBigUInt64BE(8);
        if (large > BigInt(Number.MAX_SAFE_INTEGER)) return false;
        length = Number(large); minimum = 16;
      } else if (length === 0) length = size - offset;
      if (length < minimum || offset + length > size) return false;
      if (type === 'moov' && length > minimum) moov = true;
      if (type === 'mdat' && length > minimum) mdat = true;
      offset += length;
    }
    return offset === size && moov && mdat;
  } catch { return false; }
  finally { if (fd !== undefined) fs.closeSync(fd); }
}
function audioPipe(proc) { return proc.stdio[3] && typeof proc.stdio[3].end === 'function'; }
// Cuts the camera's stream into fixed-length MP4 files (stream copy, no re-encoding) and hands them to the queue.
class Recorder extends EventEmitter {
  constructor(camera, { dir, segmentSeconds = 60, queue, audio = true }) {
    super();
    this.droppedFrames = 0; this.softwareOnly = false; this.backpressure = false; this.lastProgressAt = null;
    Object.assign(this, { camera, dir, segmentSeconds, queue, audio: audio && camera.audio !== false }); this.process = null; this.segments = 0; this.restarts = 0; this.closing = new Set(); this.lastError = null;
  }
  start(codec, {audio = false, fps = 10} = {}) { // codec: 'h264' | 'hevc' for raw pipe input; null => pull RTSP directly
    if (this.process || this.closing.size) return;
    clearTimeout(this.retry); this.stopping = false;
    fps = Math.max(1, Math.min(60, Number(this.camera.recordingFps || fps) || 10));
    const encoding = codec === 'mjpeg' ? mjpegEncoding(this.camera, fps, this.softwareOnly) : {encoder: 'copy', device: [], output: ['-c:v', 'copy']};
    this.encoder = encoding.encoder; this.backpressure = false;
    this.lastProgressAt = Date.now();
    audio = audio && this.audio && !!codec; // G.711 A-law 8 kHz mono arrives on fd 3
    fs.mkdirSync(this.dir, {recursive: true, mode: 0o700});
    const input = codec
      ? ['-use_wallclock_as_timestamps', '1', '-fflags', '+genpts', ...(codec === 'mjpeg' ? ['-probesize', '32768', '-analyzeduration', '0', '-framerate', String(fps)] : []), '-f', codec, '-i', 'pipe:0',
        ...(audio ? ['-use_wallclock_as_timestamps', '1', '-thread_queue_size', '512', '-f', 'alaw', '-ar', '8000', '-ac', '1', '-i', 'pipe:3'] : [])]
      : ['-rtsp_transport', 'tcp', '-rw_timeout', '8000000', '-i', this.camera.rtspUrl];
    const prefix = `cam${String(this.camera.deviceId || 'rtsp').replace(/\D/g, '') || 'rtsp'}`;
    const args = ['-hide_banner', '-loglevel', 'error', ...encoding.device, ...input, ...(audio ? ['-map', '0:v', '-map', '1:a', '-c:a', 'aac', '-b:a', '32k'] : ['-an']), ...encoding.output, ...(codec === 'mjpeg' ? ['-fps_mode', 'vfr'] : []), '-progress', 'pipe:4', '-f', 'segment',
      '-segment_time', String(this.segmentSeconds), '-segment_format', 'mp4', '-segment_format_options', 'movflags=+faststart', '-reset_timestamps', '1',
      '-strftime', '1', '-segment_list', 'pipe:1', '-segment_list_type', 'flat', `${prefix}_%Y%m%d_%H%M%S.mp4`];
    // A separate staging directory per FFmpeg process isolates open/incomplete segments.
    const staging = fs.mkdtempSync(path.join(this.dir, '.partial-'));
    const proc = this.process = spawn(process.env.FFMPEG_PATH || 'ffmpeg', args, {cwd: staging, stdio: ['pipe', 'pipe', 'pipe', audio ? 'pipe' : 'ignore', 'pipe']});
    let progress = '';
    proc.stdio[4].on('data', chunk => {
      progress += chunk; let nl;
      while ((nl = progress.indexOf('\n')) >= 0) {
        const line = progress.slice(0, nl); progress = progress.slice(nl + 1);
        if (/^out_time_us=\d+$/.test(line)) { this.lastProgressAt = Date.now(); this.emit('progress'); }
      }
    });
    proc.stderr.on('data', () => {}); // may contain RTSP credentials
    proc.stdin.on('error', () => {}); proc.stdio[3]?.on('error', () => {});
    let text = '';
    proc.stdout.on('data', chunk => { // one finished segment name per line
      text += chunk; let nl;
      while ((nl = text.indexOf('\n')) >= 0) {
        const name = text.slice(0, nl).trim(); text = text.slice(nl + 1);
        if (/^cam[\w]+_\d{8}_\d{6}\.mp4$/.test(name)) {
          const file = path.join(staging, name), finished = path.join(this.dir, name);
          if (!isFinalizedMp4(file)) { this.lastError = `MP4 belum selesai: ${name}`; continue; }
          try {
            // Hard-link + unlink refuses to overwrite another segment with the same timestamp.
            fs.linkSync(file, finished); fs.unlinkSync(file);
            this.segments++; this.lastError = null; this.queue.add(finished);
          } catch (error) { this.lastError = `Finalisasi rekaman gagal (${error.code || 'error'}): ${name}`; }
        }
      }
    });
    proc.on('error', () => { this.lastError = 'FFmpeg recorder tidak dapat dijalankan.'; });
    const closed = new Promise(resolve => proc.once('close', code => {
      if (code !== 0 && this.process === proc) {
        this.restarts++;
        this.lastError = `Recorder berhenti (${code ?? 'signal'}); menunggu keyframe.`;
        if (encoding.encoder === 'h264_vaapi') { this.softwareOnly = true; this.lastError += ' Fallback CPU.'; }
      }
      if (this.process === proc) this.process = null;
      this.closing.delete(closed);
      if (!codec && code !== 0 && this.process === null && !this.stopping) {
        this.retry = setTimeout(() => this.start(null), 3000);
        this.retry.unref();
      }
      try { fs.rmdirSync(staging); } catch {} // retain interrupted segments for diagnosis/recovery
      resolve();
    }));
    this.closing.add(closed);
  }
  overflow() {
    this.restarts++; this.backpressure = true;
    this.lastError = 'Buffer recorder penuh; segmen ditutup dan menunggu keyframe berikutnya.';
    this.stop();
  }
  write(data) {
    const stdin = this.process?.stdin;
    if (!stdin || stdin.destroyed) { this.droppedFrames++; return false; }
    if (stdin.writableLength + data.length > 8 * 1024 * 1024) {
      this.droppedFrames++; this.overflow(); return false;
    }
    stdin.write(data); return true;
  }
  writeAudio(data) {
    const pipe = this.process?.stdio[3];
    if (!pipe || pipe.destroyed) return;
    if (pipe.writableLength + data.length > 1024 * 1024) { this.overflow(); return; }
    pipe.write(data.subarray(16)); // drop the 16-byte packet header
  }
  stop() { // closing stdin lets FFmpeg finalize the partial last segment, which is then uploaded too
    this.stopping = true; clearTimeout(this.retry);
    const proc = this.process; this.process = null;
    if (proc) {
      proc.stdin.end(); if (audioPipe(proc)) proc.stdio[3].end();
      const terminate = setTimeout(() => proc.kill('SIGTERM'), 5000);
      const kill = setTimeout(() => proc.kill('SIGKILL'), 10000);
      terminate.unref(); kill.unref();
      proc.once('close', () => { clearTimeout(terminate); clearTimeout(kill); });
    }
    return Promise.all([...this.closing]);
  }
}
function createRecording(cfg, baseDir) {
  const rec = cfg.recording;
  if (!rec?.enabled) return null;
  const dir = path.resolve(baseDir, rec.dir || 'recordings');
  // `destination` picks exactly one target. gdrive/onedrive go through rclone remotes; local keeps files in `dir`.
  const destination = rec.destination || 'gdrive';
  let uploader;
  if (destination === 'gdrive' || destination === 'onedrive') uploader = new Rclone({remote: rec.rclone?.[destination], bin: rec.rclone?.bin});
  else if (destination === 'local') uploader = {upload: async () => {}};
  else throw new Error('recording.destination harus "gdrive", "onedrive", atau "local".');
  const queue = new UploadQueue(uploader, {keepLocal: rec.keepLocal || destination === 'local'});
  // Files left from a previous run (crash, offline) go first.
  try { for (const f of fs.readdirSync(dir)) if (/^cam[\w]+_\d{8}_\d{6}\.mp4$/.test(f)) {
    const file = path.join(dir, f);
    if (isFinalizedMp4(file)) queue.add(file);
    else console.error(`Rekaman tidak lengkap, tidak diupload: ${f}`);
  } } catch {}
  const seconds = Number(rec.segmentSeconds || 60);
  return {queue, make: camera => camera.record === false ? null : new Recorder(camera, {dir, segmentSeconds: seconds, queue, audio: rec.audio !== false})};
}
module.exports = { Recorder, UploadQueue, createRecording, isFinalizedMp4 };
