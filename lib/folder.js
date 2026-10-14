'use strict';
const fs = require('node:fs');
const path = require('node:path');
// Copies finished segments into a local folder (e.g. one synced by the OneDrive app).
// Written as .part then renamed, so the sync client never picks up a half-written file.
class FolderUploader {
  constructor({ dir }) {
    if (!dir || !path.isAbsolute(dir)) throw new Error('Folder sinkronisasi harus berupa path absolut.');
    this.dir = dir;
  }
  async upload(file) {
    await fs.promises.mkdir(this.dir, {recursive: true});
    const target = path.join(this.dir, path.basename(file)), part = `${target}.part`;
    try { await fs.promises.copyFile(file, part); await fs.promises.rename(part, target); }
    catch (error) { await fs.promises.unlink(part).catch(() => {}); throw error; }
    return target;
  }
}
module.exports = { FolderUploader };
