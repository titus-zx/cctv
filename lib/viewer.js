'use strict';
const { spawn } = require('node:child_process');
const { V380Client } = require('./v380');
const { EventEmitter } = require('node:events');
class Viewer extends EventEmitter {
  constructor(camera, recorder = null) {
    super();
    this.camera = camera; this.recorder = recorder; this.clients = new Set(); this.audioClients = new Set(); this.lastJpeg = null;
    this.state = 'idle'; this.message = 'Belum terhubung'; this.info = {};
    this.snapshotRequests = 0; this.decoderClosing = new Set(); this.connectionFailures = 0; this.reconnects = 0; this.previewError = null;
    this.recorder?.on('progress', () => {
      if (this.camera.mode === 'rtsp' && !this.closed) this.receivedFrame();
    });
    this.sampleStartedAt = Date.now(); this.sampleFrames = 0; this.sourceFps = 10;
    this.frames = 0; this.lastFrameAt = null; this.generation = 0; this.closed = false;
  }
  status() {
    const { name, ip, deviceId, username, mode, quality, v380Port } = this.camera;
    return { name, ip, deviceId, username: username || 'admin', mode: mode || 'v380', quality: quality || 'hd',
      v380Port: v380Port || 8800, state: this.state, message: this.message, ...this.info,
      recording: this.recorder ? {segments: this.recorder.segments, uploaded: this.recorder.queue.uploaded,
        pending: this.recorder.queue.pending.length, droppedFrames: this.recorder.droppedFrames, restarts: this.recorder.restarts, encoder: this.recorder.encoder, error: this.recorder.lastError || this.recorder.queue.lastError} : null,
      reconnects: this.reconnects, lastConnectionError: this.lastConnectionError || null, streamStats: this.source?.stats || this.lastStreamStats || null, previewError: this.previewError, previewActive: !!this.decoder,
      frames: this.frames, lastFrameAt: this.lastFrameAt, viewers: this.clients.size, audio: !!this.audioSeen, hasPassword: !!this.camera.password };
  }
  start() {
    if (this.closed) return;
    if (this.camera.mode !== 'rtsp' && (!/^\d{1,10}$/.test(String(this.camera.deviceId || '')) || !this.camera.password)) {
      this.state = 'error';
      this.message = 'Device ID / password belum lengkap. Isi melalui pengaturan kamera.';
      return;
    }
    const generation = ++this.generation;
    this.state = 'connecting'; this.message = 'Menghubungkan kamera…';
    this.startedAt = Date.now(); this.sampleStartedAt = this.startedAt; this.sampleFrames = 0; this.sourceFps = 10; this.firstFrameAt = null; this.audioSeen = false; this.lastFrameAt = null; this.lastJpeg = null;
    this.watchdog = setInterval(() => {
      if (this.camera.mode !== 'rtsp' && this.recorder?.process && this.recorder.lastProgressAt &&
          Date.now() - this.recorder.lastProgressAt > 20000) {
        this.recorder.restarts++;
        this.recorder.lastError = 'Recorder tidak menghasilkan video; menunggu keyframe baru.';
        this.recorder.stop();
      }
      if (this.camera.mode === 'rtsp' && !this.recorder && !this.wantsPreview()) return;
      if (Date.now() - (this.lastFrameAt || this.startedAt) > (this.camera.mode === 'cloud' ? 45000 : 18000))
        this.fail(new Error('Belum menerima gambar dari kamera.'), generation);
    }, 2000);
    if (this.camera.mode === 'rtsp') {
      this.requestPreview();
      if (this.recorder) Promise.all([...this.recorder.closing]).then(() => {
        if (!this.closed && generation === this.generation) this.recorder.start(null);
      });
      return;
    }
    const source = this.source = new V380Client(this.camera);
    source.on('status', status => { if (generation === this.generation) Object.assign(this, status); });
    source.on('info', info => { if (generation === this.generation) this.info = info; });
    source.on('failure', error => this.fail(error, generation));
    source.on('audio', pcm => { if (generation !== this.generation) return; this.audioSeen = true; this.recorder?.writeAudio(pcm); this.publishAudio(pcm.subarray(16)); });
    source.on('frame', (data, info) => {
      if (generation !== this.generation) return;
      this.receivedFrame();
      this.info.codec = info.codec;
      if (this.recorder && !this.recorder.process && !this.recorder.closing.size && info.keyframe &&
          (this.audioSeen || Date.now() - (this.firstFrameAt || this.startedAt) > 3000))
        this.recorder.start(info.codec, {audio: this.audioSeen, fps: this.camera.recordingFps || this.sourceFps});
      if (this.recorder?.process || this.recorder?.restarts > 0) this.recorder.write(data, info);
      if (!this.wantsPreview()) return;
      // JPEG cameras already provide a browser-compatible preview: no decoder needed.
      if (info.codec === 'mjpeg') {
        if (Date.now() - (this.lastPreviewAt || 0) >= 1000 / this.previewFps()) this.publish(data);
        return;
      }
      if (!this.decoder && info.keyframe && !this.decoderClosing.size && Date.now() >= (this.previewRetryAt || 0))
        this.openDecoder(info.codec, generation);
      const decoder = this.decoder;
      if (!decoder || decoder.stdin.destroyed) return;
      if (decoder.stdin.writableLength > 2 * 1024 * 1024) {
        this.previewFailed('Preview terlalu lambat; rekaman tetap berjalan.', decoder, generation); return;
      }
      decoder.stdin.write(data);
    });
    source.start();
  }
  receivedFrame() {
    this.lastFrameAt = Date.now();
    if (!this.firstFrameAt) { this.firstFrameAt = this.lastFrameAt; this.sampleStartedAt = this.lastFrameAt; }
    this.frames++; this.sampleFrames++;
    const elapsed = this.lastFrameAt - this.sampleStartedAt;
    if (elapsed >= 3000) {
      this.sourceFps = Math.max(1, Math.min(60, this.sampleFrames * 1000 / elapsed));
      this.sampleFrames = 0; this.sampleStartedAt = this.lastFrameAt;
    }
    this.state = 'live'; this.message = 'Video langsung'; this.connectionFailures = 0;
  }
  wantsPreview() { return this.clients.size > 0 || this.snapshotRequests > 0; }
  requestPreview() {
    if (this.camera.mode !== 'rtsp' || this.closed) return;
    const generation = this.generation;
    Promise.all([...this.decoderClosing]).then(() => {
      if (!this.closed && generation === this.generation && this.wantsPreview() && !this.decoder) this.openDecoder(null, generation);
    });
  }
  previewFps() { return Math.max(1, Math.min(10, Number(this.camera.previewFps) || 5)); }
  previewFailed(message, decoder, generation) {
    if (generation !== this.generation || this.decoder !== decoder) return;
    this.previewError = message; this.previewRetryAt = Date.now() + 3000;
    this.stopDecoder().then(() => {
      if (this.camera.mode === 'rtsp' && !this.closed && generation === this.generation && this.wantsPreview()) {
        this.previewRetry = setTimeout(() => {
          if (generation === this.generation && this.wantsPreview() && !this.decoder) this.openDecoder(null, generation);
        }, 3000);
      }
    });
  }
  stopDecoder() {
    const decoder = this.decoder; this.decoder = null;
    if (!decoder) return Promise.all([...this.decoderClosing]);
    let kill;
    const closed = new Promise(resolve => decoder.once('close', () => {
      clearTimeout(kill); this.decoderClosing.delete(closed); resolve();
    }));
    this.decoderClosing.add(closed);
    decoder.stdin.destroy(); decoder.kill('SIGTERM');
    kill = setTimeout(() => decoder.kill('SIGKILL'), 2000); kill.unref();
    return closed;
  }
  async snapshot() {
    this.snapshotRequests++;
    const previous = this.lastPreviewAt || 0;
    try {
      this.requestPreview();
      return await new Promise((resolve, reject) => {
        const finish = jpeg => { if ((this.lastPreviewAt || 0) > previous) { cleanup(); resolve(jpeg); } };
        const timer = setTimeout(() => { cleanup(); reject(new Error('Gambar kamera belum tersedia.')); }, 10000);
        const cleanup = () => { clearTimeout(timer); this.off('jpeg', finish); };
        this.on('jpeg', finish);
      });
    } finally { this.snapshotRequests--; if (!this.wantsPreview()) this.stopDecoder(); }
  }
  openDecoder(codec, generation) {
    if (this.decoder || this.decoderClosing.size || this.closed || generation !== this.generation) return;
    const args = ['-hide_banner', '-loglevel', 'error', '-threads', '1'];
    if (codec) args.push('-flags', 'low_delay', '-probesize', '32768', '-analyzeduration', '0', '-f', codec, '-i', 'pipe:0');
    else args.push('-rtsp_transport', 'tcp', '-rw_timeout', '8000000', '-i', this.camera.rtspUrl);
    args.push('-an', '-vf', `fps=${this.previewFps()},scale=w='min(640,iw)':h=-2`, '-threads', '1', '-c:v', 'mjpeg', '-q:v', '5', '-f', 'image2pipe', 'pipe:1');
    const decoder = this.decoder = spawn(process.env.FFMPEG_PATH || 'ffmpeg', args, {stdio: ['pipe', 'pipe', 'pipe']});
    let buffer = Buffer.alloc(0);
    decoder.stderr.on('data', () => {}); // FFmpeg errors can contain RTSP credentials.
    decoder.stdin.on('error', error => this.previewFailed(`Input preview terputus (${error.code || 'error'}).`, decoder, generation));
    decoder.on('error', () => this.previewFailed('FFmpeg preview tidak dapat dijalankan.', decoder, generation));
    decoder.on('close', () => this.previewFailed('Preview berhenti; rekaman tetap berjalan.', decoder, generation));
    decoder.stdout.on('data', chunk => {
      if (generation !== this.generation || this.decoder !== decoder) return;
      buffer = Buffer.concat([buffer, chunk]);
      while (true) {
        const start = buffer.indexOf(Buffer.from([0xff, 0xd8]));
        if (start < 0) { buffer = buffer.subarray(Math.max(0, buffer.length - 1)); break; }
        if (start > 0) buffer = buffer.subarray(start);
        const end = buffer.indexOf(Buffer.from([0xff, 0xd9]), 2);
        if (end < 0) break;
        this.publish(Buffer.from(buffer.subarray(0, end + 2))); buffer = buffer.subarray(end + 2);
      }
      if (buffer.length > 8 * 1024 * 1024) this.previewFailed('Output preview tidak valid.', decoder, generation);
    });
  }
  publish(jpeg) {
    if (!this.info.width || !this.info.height) {
      // JPEG SOF markers contain dimensions before the image scan data.
      let offset = 2;
      while (offset + 4 <= jpeg.length && jpeg[offset] === 0xff) {
        const marker = jpeg[offset + 1];
        if (marker === 0xda || marker === 0xd9) break;
        if (marker === 0xff) { offset++; continue; }
        const length = jpeg.readUInt16BE(offset + 2);
        if (length < 2 || offset + 2 + length > jpeg.length) break;
        if ([0xc0,0xc1,0xc2,0xc3,0xc5,0xc6,0xc7,0xc9,0xca,0xcb,0xcd,0xce,0xcf].includes(marker) && length >= 8) {
          this.info.height = jpeg.readUInt16BE(offset + 5);
          this.info.width = jpeg.readUInt16BE(offset + 7);
          break;
        }
        offset += length + 2;
      }
    }
    this.lastJpeg = jpeg; this.lastPreviewAt = Date.now(); this.previewError = null;
    if (!this.source) this.receivedFrame();
    this.emit('jpeg', jpeg);
    this.state = 'live'; this.message = 'Video langsung';
    for (const client of this.clients) this.send(client, jpeg);
  }
  send(res, jpeg) {
    if (res.destroyed || res.writableLength > 1024 * 1024) { this.clients.delete(res); res.destroy(); return; }
    res.write(`--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ${jpeg.length}\r\n\r\n`);
    res.write(jpeg); res.write('\r\n');
  }
  subscribe(res) {
    res.writeHead(200, {'Content-Type':'multipart/x-mixed-replace; boundary=frame', 'Cache-Control':'no-store', 'X-Content-Type-Options':'nosniff'});
    this.clients.add(res);
    this.requestPreview();
    if (this.lastJpeg) this.send(res, this.lastJpeg);
    res.on('close', () => { this.clients.delete(res); if (!this.wantsPreview()) this.stopDecoder(); });
  }
  ptz(direction) {
    if (!this.source || this.state !== 'live') throw new Error('Kamera belum live.');
    this.source.ptz(direction);
    clearTimeout(this.ptzTimer); // safety: the camera keeps moving until told to stop
    if (direction !== 'stop') this.ptzTimer = setTimeout(() => { try { this.source?.ptz('stop'); } catch {} }, 1500);
  }
  talk(alaw) {
    if (!this.source || this.state !== 'live') throw new Error('Kamera belum live.');
    this.source.talk(alaw);
  }
  publishAudio(alaw) { // raw G.711 A-law, 8 kHz mono; the browser decodes it
    for (const res of this.audioClients) {
      if (res.destroyed || res.writableLength > 256 * 1024) { this.audioClients.delete(res); res.destroy(); continue; }
      res.write(alaw);
    }
  }
  subscribeAudio(res) {
    res.writeHead(200, {'Content-Type':'application/octet-stream', 'Cache-Control':'no-store', 'X-Content-Type-Options':'nosniff'});
    this.audioClients.add(res); res.on('close', () => this.audioClients.delete(res));
  }
  fail(error, generation, retry = true) {
    if (this.closed || generation !== this.generation) return;
    this.lastConnectionError = error.message; this.lastStreamStats = this.source ? {...this.source.stats} : null;
    this.cleanup(); this.state = 'error'; this.message = error.message;
    if (error.code?.startsWith('AUTH_')) retry = false;
    if (retry) {
      this.connectionFailures++; this.reconnects++;
      const delay = Math.min(30000, 3000 * 2 ** Math.min(this.connectionFailures - 1, 4));
      this.retry = setTimeout(() => this.start(), delay);
    }
  }
  cleanup() {
    this.generation++;
    clearInterval(this.watchdog); clearTimeout(this.retry); clearTimeout(this.ptzTimer); clearTimeout(this.previewRetry);
    this.source?.stop(); this.source = null;
    this.stopDecoder(); this.recorder?.stop();
    this.lastJpeg = null;
  }
  reconnect() { this.cleanup(); this.info = {}; this.frames = 0; this.start(); }
  stop() { this.closed = true; this.cleanup(); for (const res of [...this.clients, ...this.audioClients]) res.end(); this.clients.clear(); this.audioClients.clear(); }
}
module.exports = { Viewer };
