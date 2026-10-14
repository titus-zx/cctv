'use strict';
const { spawn } = require('node:child_process');
const { V380Client } = require('./v380');
class Viewer {
  constructor(camera) {
    this.camera = camera; this.clients = new Set(); this.lastJpeg = null;
    this.state = 'idle'; this.message = 'Belum terhubung'; this.info = {};
    this.frames = 0; this.lastFrameAt = null; this.generation = 0; this.closed = false;
  }
  status() {
    const { name, ip, deviceId, username, mode, quality, v380Port } = this.camera;
    return { name, ip, deviceId, username: username || 'admin', mode: mode || 'v380', quality: quality || 'hd',
      v380Port: v380Port || 8800, state: this.state, message: this.message, ...this.info,
      frames: this.frames, lastFrameAt: this.lastFrameAt, viewers: this.clients.size, hasPassword: !!this.camera.password };
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
    this.startedAt = Date.now(); this.lastFrameAt = null; this.lastJpeg = null;
    this.watchdog = setInterval(() => {
      if (Date.now() - (this.lastFrameAt || this.startedAt) > (this.camera.mode === 'cloud' ? 45000 : 18000))
        this.fail(new Error('Belum menerima gambar dari kamera.'), generation);
    }, 2000);
    if (this.camera.mode === 'rtsp') { this.openDecoder(null, generation); return; }
    const source = this.source = new V380Client(this.camera);
    source.on('status', status => { if (generation === this.generation) Object.assign(this, status); });
    source.on('info', info => { this.info = info; });
    source.on('failure', error => this.fail(error, generation));
    source.on('frame', (data, info) => {
      if (generation !== this.generation) return;
      if (!this.decoder) {
        if (!info.keyframe) return;
        this.info.codec = info.codec; this.openDecoder(info.codec, generation);
      }
      if (this.decoder.stdin.destroyed) return;
      if (this.decoder.stdin.writableLength > 4 * 1024 * 1024)
        return this.fail(new Error('Decoder terlalu lambat. Menghubungkan ulang…'), generation);
      this.decoder.stdin.write(data);
    });
    source.start();
  }
  openDecoder(codec, generation) {
    const args = ['-hide_banner', '-loglevel', 'error'];
    if (codec) args.push('-flags', 'low_delay', '-probesize', '32768', '-analyzeduration', '0', '-f', codec, '-i', 'pipe:0');
    else args.push('-rtsp_transport', 'tcp', '-rw_timeout', '8000000', '-i', this.camera.rtspUrl);
    args.push('-an', '-vf', 'fps=10', '-c:v', 'mjpeg', '-q:v', '5', '-f', 'image2pipe', 'pipe:1');
    const decoder = this.decoder = spawn(process.env.FFMPEG_PATH || 'ffmpeg', args, {stdio: ['pipe', 'pipe', 'pipe']});
    let buffer = Buffer.alloc(0);
    decoder.stderr.on('data', () => {}); // FFmpeg errors can contain RTSP credentials.
    decoder.stdin.on('error', error => this.fail(new Error(`Input decoder terputus (${error.code || 'error'}).`), generation));
    decoder.on('error', () => this.fail(new Error('FFmpeg tidak dapat dijalankan. Pastikan ffmpeg terpasang.'), generation, false));
    decoder.on('close', () => this.fail(new Error('Decoder berhenti. Menghubungkan ulang…'), generation));
    decoder.stdout.on('data', chunk => {
      if (generation !== this.generation) return;
      buffer = Buffer.concat([buffer, chunk]);
      while (true) {
        const start = buffer.indexOf(Buffer.from([0xff, 0xd8]));
        if (start < 0) { buffer = buffer.subarray(Math.max(0, buffer.length - 1)); break; }
        if (start > 0) buffer = buffer.subarray(start);
        const end = buffer.indexOf(Buffer.from([0xff, 0xd9]), 2);
        if (end < 0) break;
        this.publish(Buffer.from(buffer.subarray(0, end + 2))); buffer = buffer.subarray(end + 2);
      }
      if (buffer.length > 8 * 1024 * 1024) this.fail(new Error('Output decoder tidak valid.'), generation);
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
    this.lastJpeg = jpeg; this.lastFrameAt = Date.now(); this.frames++;
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
    if (this.lastJpeg) this.send(res, this.lastJpeg);
    res.on('close', () => this.clients.delete(res));
  }
  fail(error, generation, retry = true) {
    if (this.closed || generation !== this.generation) return;
    this.cleanup(); this.state = 'error'; this.message = error.message;
    if (error.code?.startsWith('AUTH_')) retry = false;
    if (retry) this.retry = setTimeout(() => this.start(), 5000);
  }
  cleanup() {
    this.generation++;
    clearInterval(this.watchdog); clearTimeout(this.retry);
    this.source?.stop(); this.source = null;
    this.decoder?.kill('SIGTERM'); this.decoder = null;
    this.lastJpeg = null;
  }
  reconnect() { this.cleanup(); this.info = {}; this.frames = 0; this.start(); }
  stop() { this.closed = true; this.cleanup(); for (const res of this.clients) res.end(); this.clients.clear(); }
}
module.exports = { Viewer };
