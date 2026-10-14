'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { Readable } = require('node:stream');
const { DriveUploader } = require('./drive');
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
// Reads from wherever the recorder delivers files: the OneDrive-synced folder, recordings/ (local), or Google Drive.
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
class DriveStore {
  constructor(drive) { this.drive = drive; this.ids = new Map(); }
  async list(day) {
    const [from, to] = byDay(day);
    // Drive's modifiedTime is the upload time; pad the window and then filter by the time in the file name.
    const files = await this.drive.list({after: new Date(from - 3600e3).toISOString(), before: new Date(+to + 6 * 3600e3).toISOString()});
    const out = [];
    for (const f of files) {
      const item = parseName(f.name, f.size); if (!item) continue;
      const start = new Date(item.start); if (start < from || start >= to) continue;
      this.ids.set(f.name, f.id); out.push(item);
    }
    return out;
  }
  async open(name, header) {
    let id = this.ids.get(name);
    if (!id) { const f = await this.drive.find(name); if (!f) return null; this.ids.set(name, id = f.id); }
    const res = await this.drive.download(id, header);
    if (res.status === 404) { this.ids.delete(name); return null; }
    if (!res.ok && res.status !== 206) throw new Error(`Unduh Drive gagal (${res.status}).`);
    const headers = {}; for (const h of ['content-length', 'content-range']) if (res.headers.get(h)) headers[h] = res.headers.get(h);
    return {status: res.status, headers, stream: Readable.fromWeb(res.body)};
  }
}
function createStore(cfg, baseDir) {
  const rec = cfg.recording; if (!rec?.enabled) return null;
  const destination = rec.destination || 'gdrive';
  if (destination === 'gdrive') return new DriveStore(new DriveUploader(rec.drive || {}));
  if (destination === 'onedrive') return new FolderStore(rec.onedrive.dir);
  return new FolderStore(path.resolve(baseDir, rec.dir || 'recordings'));
}
module.exports = { createStore, parseName, range, NAME };
