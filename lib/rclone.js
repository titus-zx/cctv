'use strict';
const { spawn } = require('node:child_process');
const fs = require('node:fs');
const path = require('node:path');
// Thin wrapper over the rclone CLI. `remote` is any rclone path ("gdrive:cctv", "onedrive:cctv", or a local dir),
// so rclone owns credentials, tokens, retries and provider quirks. Arguments are passed as an array, never through a shell.
class Rclone {
  constructor({ remote, bin }) {
    if (!remote || typeof remote !== 'string') throw new Error('Remote rclone belum diisi (mis. "gdrive:cctv").');
    this.remote = remote; this.bin = bin || process.env.RCLONE_PATH || 'rclone';
  }
  target(name) { return `${this.remote.replace(/\/+$/, '')}/${name}`; }
  spawn(args) { return spawn(this.bin, args, {stdio: ['ignore', 'pipe', 'pipe']}); }
  run(args) {
    return new Promise((resolve, reject) => {
      const proc = this.spawn(args); let out = '', err = '';
      proc.stdout.on('data', c => out += c); proc.stderr.on('data', c => { if (err.length < 4000) err += c; });
      proc.on('error', e => reject(new Error(e.code === 'ENOENT' ? 'rclone tidak ditemukan. Pasang rclone atau set RCLONE_PATH.' : e.message)));
      proc.on('close', code => code === 0 ? resolve(out)
        : reject(new Error(`rclone gagal (${code}): ${err.trim().split('\n').pop() || 'tanpa pesan'}`)));
    });
  }
  async upload(file) {
    await fs.promises.access(file); // ENOENT lets the queue drop files that were already removed
    await this.run(['copyto', '--no-traverse', '--', file, this.target(path.basename(file))]);
  }
  async list(day) { // day: 'YYYYMMDD'
    const out = await this.run(['lsjson', '--files-only', '--no-mimetype', '--no-modtime', '--include', `*_${day}_*.mp4`, this.remote]);
    return JSON.parse(out || '[]').map(f => ({name: f.Name, size: f.Size}));
  }
  async stat(name) {
    const out = await this.run(['lsjson', '--files-only', '--no-mimetype', '--no-modtime', this.target(name)]).catch(() => '[]');
    const f = JSON.parse(out || '[]')[0]; return f ? {name: f.Name, size: f.Size} : null;
  }
  async listRecordings() {
    const out = await this.run(['lsjson', '--files-only', '--no-mimetype', '--no-modtime', '--include', 'cam*_????????_??????.mp4', this.remote]);
    // lsjson without --recursive lists the destination folder itself only.
    return JSON.parse(out || '[]').filter(file => !file.IsDir && file.Path === file.Name)
      .map(file => ({name:file.Name, size:file.Size}));
  }
  async removeRecording(name) {
    if (!/^cam\d+_\d{8}_\d{6}\.mp4$/.test(name)) throw new Error('Nama rekaman retensi tidak valid.');
    await this.run(['deletefile', '--', this.target(name)]);
  }
  cat(name, offset, count) { return this.spawn(['cat', '--offset', String(offset), '--count', String(count), this.target(name)]); }
}
module.exports = { Rclone };
