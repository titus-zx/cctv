'use strict';
const net = require('node:net');
const crypto = require('node:crypto');
const { EventEmitter } = require('node:events');
const { resolveRelay } = require('./cloud');

// Protocol reference: https://github.com/PyanSofyan/V380Decoder
// Independently implemented with Node built-ins; no vendor binaries required.
function aes(data, key, decrypt = false) {
  const cipher = decrypt ? crypto.createDecipheriv('aes-128-ecb', key, null)
    : crypto.createCipheriv('aes-128-ecb', key, null);
  cipher.setAutoPadding(false);
  return Buffer.concat([cipher.update(data), cipher.final()]);
}
function encodePassword(password) {
  const raw = Buffer.from(password, 'utf8');
  if (raw.length > 47) throw new Error('Protokol login ini mendukung password maksimal 47 byte.');
  const random = Buffer.from(crypto.randomBytes(16).toString('hex').slice(0, 16));
  const padded = Buffer.alloc(48); raw.copy(padded);
  const staticKey = Buffer.alloc(16); staticKey.write('macrovideo+*#!^@');
  return Buffer.concat([random, aes(aes(padded, staticKey), random)]);
}
function mediaKey(ticket) {
  const key = Buffer.alloc(16);
  key.writeUInt32LE(ticket, 0);
  key.writeBigUInt64LE(0x618123462c14795cn, 4);
  key.writeUInt32LE(0x82800df0, 12);
  return key;
}
function decodeVideo(frame, key, version = 20) {
  const data = Buffer.from(frame.subarray(16));
  if (!key) return data;
  if (version === 21) {
    const size = data.length - data.length % 16;
    aes(data.subarray(0, size), key, true).copy(data);
  } else {
    for (let offset = 0; offset + 64 <= data.length; offset += 80) {
      aes(data.subarray(offset, offset + 64), key, true).copy(data, offset);
    }
  }
  return data;
}

class SocketReader {
  constructor(socket) {
    this.socket = socket; this.buffer = Buffer.alloc(0); this.pending = null; this.error = null;
    socket.on('data', chunk => {
      if (this.buffer.length + chunk.length > 8 * 1024 * 1024) return socket.destroy(new Error('Buffer kamera terlalu besar.'));
      this.buffer = Buffer.concat([this.buffer, chunk]); this.flush();
    });
    socket.on('error', error => { this.error = error; this.flush(); });
    socket.on('close', () => { this.error ||= new Error('Koneksi kamera terputus.'); this.flush(); });
    socket.on('timeout', () => socket.destroy(new Error('Kamera tidak mengirim data selama 8 detik.')));
  }
  flush() {
    const p = this.pending;
    if (!p) return;
    if (this.error) { this.pending = null; p.reject(this.error); }
    else if (this.buffer.length >= p.size) {
      const data = this.buffer.subarray(0, p.size); this.buffer = this.buffer.subarray(p.size);
      this.pending = null; p.resolve(data);
    }
  }
  read(size) {
    if (this.pending) throw new Error('Concurrent socket read');
    return new Promise((resolve, reject) => { this.pending = { size, resolve, reject }; this.flush(); });
  }
  async connect(host, port) {
    this.socket.setTimeout(8000);
    await new Promise((resolve, reject) => {
      const cleanup = () => { this.socket.off('connect', connected); this.socket.off('error', error); this.socket.off('close', closed); };
      const error = e => { cleanup(); reject(e); };
      const closed = () => error(new Error('Koneksi kamera dihentikan.'));
      const connected = () => { cleanup(); resolve(); };
      this.socket.once('connect', connected); this.socket.once('error', error); this.socket.once('close', closed);
      this.socket.connect(port, host);
    });
    this.socket.setNoDelay(true);
  }
}
class V380Client extends EventEmitter {
  constructor(camera, options = {}) {
    super(); this.stats = {packets:0, bytes:0, videoPackets:0, audioPackets:0, frames:0, incompleteFrames:0, ignoredPackets:0, lastPacketAt:null};
    this.camera = camera; this.sockets = new Set(); this.stopped = false;
    this.cloud = camera.mode === 'cloud'; this.resolveRelay = options.resolveRelay || resolveRelay;
    this.abortController = new AbortController(); this.endpoint = null;
  }
  async socket() {
    const socket = new net.Socket(); this.sockets.add(socket);
    socket.once('close', () => this.sockets.delete(socket));
    const reader = new SocketReader(socket);
    await reader.connect(this.endpoint?.host || this.camera.ip, this.endpoint?.port || this.camera.v380Port || 8800);
    if (this.stopped) { socket.destroy(); throw new Error('Dihentikan'); }
    return reader;
  }
  async login() {
    if (this.cloud && !this.endpoint) {
      this.emit('status', {state:'connecting', message:'Mencari server relay cloud V380…'});
      this.endpoint = await this.resolveRelay(this.camera.deviceId, {signal:this.abortController.signal});
      if (this.stopped) throw new Error('Dihentikan');
    }
    const reader = await this.socket();
    try {
      const cmd = Buffer.alloc(this.cloud ? 520 : 256);
      cmd.writeUInt32LE(1167, 0); cmd.writeUInt32LE(this.cloud ? 1022 : 1361, 4); cmd[8] = 31;
      cmd.writeUInt32LE(this.cloud ? 1 : 0, 9);
      cmd.writeUInt32LE(Number(this.camera.deviceId), 13);
      const stamp = new Date().toISOString().slice(0, 19).replace('T', ' ');
      if (this.cloud) {
        cmd.write(`${this.camera.deviceId}.nvdvr.net`, 17, 50);
        cmd.writeUInt32LE(8800, 67);
        cmd.write(this.camera.username || 'admin', 71, 32);
        encodePassword(this.camera.password).copy(cmd, 103);
      } else {
        cmd.write(stamp, 17, 32); cmd.write(this.camera.username || 'admin', 49, 32);
        encodePassword(this.camera.password).copy(cmd, 81);
      }
      reader.socket.write(cmd);
      const reply = await reader.read(256);
      if (reply.readUInt32LE(0) !== 1168) throw new Error('Respons login V380 tidak dikenal.');
      const result = reply.readInt32LE(4);
      if (result !== 1001) {
        const message = {1011: 'Username kamera ditolak.', 1012: 'Password kamera ditolak.', 1018: 'Device ID kamera ditolak.'}[result];
        const error = new Error(message || `Login kamera gagal (${result}).`); error.code = `AUTH_${result}`; throw error;
      }
      return { ticket: reply.readUInt32LE(13), session: reply.readUInt32LE(17), version: reply[12] };
    } finally { reader.socket.destroy(); }
  }
  async start() {
    try {
      this.emit('status', { state: 'connecting', message: 'Login ke kamera…' });
      const auth = await this.login();
      this.emit('status', { state: 'connecting', message: 'Login berhasil. Membuka video…' });
      const reader = await this.socket();
      const cmd = Buffer.alloc(256);
      cmd.writeUInt32LE(301, 0);
      if (this.cloud) {
        cmd.writeUInt32LE(1022, 4); cmd.write(`${this.camera.deviceId}.nvdvr.net`, 8, 50);
        cmd.writeUInt32LE(8800, 58); cmd.writeUInt32LE(Number(this.camera.deviceId), 62);
        cmd.writeUInt32LE(auth.ticket, 66); cmd.writeUInt32LE(auth.session, 70);
        cmd.writeUInt32LE(this.camera.quality === 'sd' ? 0 : 1, 74);
        cmd[78] = 20; cmd.writeUInt32LE(1, 79);
      } else {
        cmd.writeUInt32LE(Number(this.camera.deviceId), 4);
        cmd.writeUInt16LE(20, 12); cmd.writeUInt32LE(auth.ticket, 14);
        cmd.writeUInt32LE(4096, 22); cmd.writeUInt32LE(this.camera.quality === 'sd' ? 0 : 1, 26);
      }
      reader.socket.write(cmd);
      const reply = await reader.read(32);
      if (reply.readUInt32LE(0) !== 401 || reply.readInt32LE(4) < 0) throw new Error('Kamera menolak permintaan stream.');
      // Relay replies do not contain the LAN resolution/version fields.
      // For cloud streams, determine resolution from the decoded JPEG output.
      const version = this.cloud ? null : reply.readUInt16LE(8);
      const width = this.cloud ? 0 : reply.readUInt32LE(10), height = this.cloud ? 0 : reply.readUInt32LE(14);
      const start = Buffer.alloc(256); start.writeUInt32LE(303, 0); start.writeUInt16LE(0x3001, 4);
      reader.socket.write(start); this.control = reader.socket;
      const key = this.key = auth.version > 30 ? mediaKey(auth.ticket) : null;
      this.emit('info', {width, height, deviceVersion: auth.version, communicationVersion: version,
        transport: this.cloud ? 'cloud' : 'lan'});
      let parts = [], next = 0, total = 0, frameType = 0, bytes = 0, skipped = 0, streamCodec = 'h264';
      while (!this.stopped) {
        // Some firmware sends a longer control reply. Resynchronize byte by byte.
        const first = await reader.read(1);
        if (first[0] !== 0x7f) {
          if (++skipped > 65536) throw new Error('Format stream V380 tidak dikenal.');
          continue;
        }
        skipped = 0;
        const h = Buffer.concat([first, await reader.read(11)]);
        const type = h[1], count = h.readUInt16LE(3), index = h.readUInt16LE(5), length = h.readUInt16LE(7);
        if (!count || index >= count || !length || length > 20000) throw new Error('Header fragment kamera tidak valid.');
        const data = await reader.read(length);
        this.stats.packets++; this.stats.bytes += length; this.stats.lastPacketAt = Date.now();
        if (type === 26) { this.stats.audioPackets++; if (data.length > 16) this.emit('audio', decodeVideo(data, key, 21)); continue; }
        if (![0, 1, 9, 0x28, 0x29].includes(type)) { this.stats.ignoredPackets++; continue; }
        this.stats.videoPackets++;
        if (index === 0) { if (parts.length && next !== total) this.stats.incompleteFrames++; parts = []; bytes = 0; next = 0; total = count; frameType = type; }
        if (index !== next || count !== total || type !== frameType) { this.stats.incompleteFrames++; parts = []; next = -1; continue; }
        parts.push(data); bytes += length; next++;
        if (bytes > 4 * 1024 * 1024) throw new Error('Frame kamera terlalu besar.');
        if (index !== count - 1) continue;
        const full = Buffer.concat(parts); parts = []; bytes = 0;
        if (full.length < 20) continue;
        const payload = decodeVideo(full, key, version);
        if (type === 9) {
          if (payload[0] !== 0xff || payload[1] !== 0xd8) throw new Error('Frame JPEG kamera tidak valid.');
          this.stats.frames++;
          this.emit('frame', payload, { keyframe: true, codec: 'mjpeg' });
          continue;
        }
        let offset = payload.indexOf(Buffer.from([0, 0, 1]));
        if (offset < 0 || offset > 16) throw new Error('Video belum dapat didekode: format enkripsi kamera berbeda.');
        if (offset > 0 && payload[offset - 1] === 0) offset--;
        const video = payload.subarray(offset);
        const nalOffset = video[2] === 1 ? 3 : 4;
        const nal = video[nalOffset];
        // Parameter-set NAL 0x40/0x42/0x44 identifies HEVC; H264 SPS usually 0x27/0x67.
        if ([0x40, 0x42, 0x44].includes(nal)) streamCodec = 'hevc';
        else if ([0x27, 0x67].includes(nal)) streamCodec = 'h264';
        const codec = streamCodec;
        this.stats.frames++;
        this.emit('frame', video, { keyframe: type === 0 || type === 0x28, codec });
      }
    } catch (error) { if (!this.stopped) this.emit('failure', error); }
    finally { this.stop(); }
  }
  // EXPERIMENTAL push-to-talk: the talkback protocol is undocumented, so this mirrors the layout of the audio
  // frames the camera sends us (0x7f header, type 26, 16-byte prefix + 480 bytes A-law, AES-ECB with the media key).
  talk(alaw) {
    if (!this.control || this.control.destroyed) throw new Error('Kamera belum terhubung.');
    if (!alaw.length || alaw.length > 480) throw new Error('Ukuran audio tidak valid.');
    const payload = Buffer.alloc(480, 0xd5); alaw.copy(payload);
    const prefix = Buffer.alloc(16); this.talkSeq = (this.talkSeq || 0) + 1;
    prefix.writeInt32LE(-510 * this.talkSeq, 0); prefix.writeUInt16LE(26, 4); prefix.writeUInt16LE(16, 6);
    prefix.writeUInt32LE((Date.now() & 0xffffffff) >>> 0, 8); prefix.writeUInt32LE(417, 12);
    let body = Buffer.concat([prefix, payload]);
    if (this.key) body = Buffer.concat([prefix, aes(payload, this.key)]);
    const header = Buffer.alloc(12);
    header[0] = 0x7f; header[1] = 26; header[2] = this.talkSeq & 0xff; header.writeUInt16LE(1, 3);
    header.writeUInt16LE(0, 5); header.writeUInt16LE(body.length, 7); header[9] = 2;
    this.control.write(Buffer.concat([header, body]));
  }
  // Pan/tilt: command 170 with 1000 = idle, x 1002/1001 = right/left (verified on the cameras; the reference project has them the other way round), y 1003/1004 = up/down. Send idle to stop.
  ptz(direction) {
    const x = {right: 1002, left: 1001}[direction] || 1000, y = {up: 1003, down: 1004}[direction] || 1000;
    if (direction !== 'stop' && x === 1000 && y === 1000) throw new Error('Arah tidak valid.');
    if (!this.control || this.control.destroyed) throw new Error('Kamera belum terhubung.');
    const cmd = Buffer.alloc(256);
    cmd.writeUInt32LE(170, 0); cmd.writeUInt16LE(1000, 4); cmd.writeUInt16LE(1000, 6);
    cmd.writeUInt16LE(x, 8); cmd.writeUInt16LE(y, 10); cmd.writeUInt16LE(1, 14);
    this.control.write(cmd);
  }
  stop() { this.stopped = true; this.abortController.abort(); for (const socket of this.sockets) socket.destroy(); }
}
module.exports = { V380Client, mediaKey, decodeVideo };
