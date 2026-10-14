'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { Rclone } = require('./rclone');
const NAME = /^cam(\d+)_(\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})\.mp4$/;
// Segment files are named by the recorder: cam<deviceId>_YYYYMMDD_HHMMSS.mp4 (server local time).
function parseName(name, size) {
  const m = NAME.exec(name); if (!m) return null;
  const start = new Date(+m[2], m[3] - 1, +m[4], +m[5], +m[6], +m[7]);
  return {name, deviceId: m[1], start: start.toISOString(), size: Number(size) || 0};
}
function byDay(day) { // 'YYYY-MM-DD' -> [from, to) in local time
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day || ''); if (!m) throw new Error('Tanggal tidak valid.');
  const from = new Date(+m[1], m[2] - 1, +m[3]); return [from, new Date(+m[1], m[2] - 1, +m[3] + 1)];
}
function range(header, size) { // single "bytes=a-b" range only
  const m = /^bytes=(\d*)-(\d*)$/.exec(header || ''); if (!m || (!m[1] && !m[2])) return null;
  let start = m[1] ? Number(m[1]) : Math.max(0, size - Number(m[2])), end = m[1] && m[2] ? Number(m[2]) : size - 1;
  end = Math.min(end, size - 1); return start <= end && start < size ? {start, end} : false;
}
// Reads from wherever the recorder delivers files: an rclone remote (gdrive/onedrive) or recordings/ (local).
class FolderStore {
  constructor(dir) { this.dir = dir; }
  async list(day) {
    const [from, to] = byDay(day), out = [];
    for (const name of await fs.promises.readdir(this.dir).catch(() => [])) {
      const item = parseName(name); if (!item) continue;
      const start = new Date(item.start); if (start < from || start >= to) continue;
      const stat = await fs.promises.stat(path.join(this.dir, name)).catch(() => null);
      if (stat?.isFile()) out.push({...item, size: stat.size});
    }
    return out;
  }
  async open(name, header) {
    const file = path.join(this.dir, name), stat = await fs.promises.stat(file).catch(() => null);
    if (!stat?.isFile()) return null;
    const r = range(header, stat.size);
    if (r === false) return {status: 416, headers: {'Content-Range': `bytes */${stat.size}`}, stream: null};
    const {start, end} = r || {start: 0, end: stat.size - 1};
    return {status: r ? 206 : 200, stream: fs.createReadStream(file, {start, end}), headers: {
      'Content-Length': end - start + 1, ...(r ? {'Content-Range': `bytes ${start}-${end}/${stat.size}`} : {})}};
  }
}
class RcloneStore {
  constructor(rclone) { this.rclone = rclone; this.sizes = new Map(); }
  async list(day) {
    byDay(day); const out = [];
    for (const f of await this.rclone.list(day.replaceAll('-', ''))) {
      const item = parseName(f.name, f.size); if (!item) continue;
      this.sizes.set(f.name, f.size); out.push(item);
    }
    return out;
  }
  async open(name, header) {
    let size = this.sizes.get(name);
    if (size === undefined) { const f = await this.rclone.stat(name); if (!f) return null; this.sizes.set(name, size = f.size); }
    const r = range(header, size);
    if (r === false) return {status: 416, headers: {'Content-Range': `bytes */${size}`}, stream: null};
    const {start, end} = r || {start: 0, end: size - 1};
    const proc = this.rclone.cat(name, start, end - start + 1);
    proc.on('error', () => proc.stdout.destroy());
    const stream = proc.stdout; stream.on('close', () => proc.kill('SIGTERM')); // client seeked away or left
    return {status: r ? 206 : 200, stream, headers: {'Content-Length': end - start + 1, ...(r ? {'Content-Range': `bytes ${start}-${end}/${size}`} : {})}};
  }
}
function createStore(cfg, baseDir) {
  const rec = cfg.recording; if (!rec?.enabled) return null;
  const destination = rec.destination || 'gdrive';
  if (destination === 'gdrive' || destination === 'onedrive')
    return new RcloneStore(new Rclone({remote: rec.rclone?.[destination], bin: rec.rclone?.bin}));
  return new FolderStore(path.resolve(baseDir, rec.dir || 'recordings'));
}
module.exports = { createStore, parseName, range, NAME };
