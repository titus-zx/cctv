'use strict';
const fs = require('node:fs');
const path = require('node:path');
const { Rclone } = require('./rclone');
const { isFinalizedMp4 } = require('./recorder');
const DAY = 86400000;
function retentionSettings(recording = {}, env = process.env) {
  const value = (key, fallback) => env[key] === undefined || env[key] === '' ? fallback : env[key];
  const days = Number(value('RECORDING_RETENTION_DAYS', recording.retentionDays ?? 0));
  const checkSeconds = Number(value('RECORDING_RETENTION_CHECK_SECONDS', recording.retentionCheckSeconds ?? 3600));
  if (!Number.isFinite(days) || days < 0) throw new Error('recording.retentionDays / RECORDING_RETENTION_DAYS harus angka >= 0.');
  if (!Number.isInteger(checkSeconds) || checkSeconds < 1 || checkSeconds > 2147483)
    throw new Error('Interval retensi harus bilangan bulat 1–2147483 detik.');
  return {days, checkSeconds};
}
function recordingTime(name, deviceIds) {
  const m = /^cam(\d+)_(\d{4})(\d{2})(\d{2})_(\d{2})(\d{2})(\d{2})\.mp4$/.exec(name);
  if (!m || !deviceIds.has(m[1])) return null;
  const [year, month, day, hour, minute, second] = m.slice(2).map(Number);
  const time = new Date(year, month - 1, day, hour, minute, second);
  if (time.getFullYear() !== year || time.getMonth() !== month - 1 || time.getDate() !== day ||
      time.getHours() !== hour || time.getMinutes() !== minute || time.getSeconds() !== second) return null;
  return time.getTime();
}
class Retention {
  constructor({days, checkSeconds, dir, remote, queue, cameras, now = Date.now, log = console}) {
    Object.assign(this, {days, checkSeconds, dir, remote, queue, cameras, now, log});
    this.running = null; this.stopped = false;
    this.deletedLocal = 0; this.deletedRemote = 0; this.lastRunAt = null; this.lastError = null;
  }
  status() {
    return {days:this.days, checkSeconds:this.checkSeconds, deletedLocal:this.deletedLocal,
      deletedRemote:this.deletedRemote, lastRunAt:this.lastRunAt, error:this.lastError};
  }
  start() {
    if (this.stopped || this.timer) return;
    this.run();
    this.timer = setInterval(() => this.run(), this.checkSeconds * 1000); this.timer.unref();
  }
  run() {
    if (this.stopped || this.days === 0) return Promise.resolve();
    if (this.running) return this.running;
    this.running = this.clean().catch(error => {this.lastError=error.message;this.log.error(`Retensi rekaman: ${error.message}`);})
      .finally(() => {this.running=null;});
    return this.running;
  }
  async clean() {
    const ids = new Set(this.cameras().map(camera => String(camera.deviceId)));
    const cutoff = this.now() - this.days * DAY;
    const expired = name => {const time=recordingTime(name,ids);return time !== null && time < cutoff;};
    const pending = name => this.queue && [...this.queue.queued].some(file => path.basename(file) === name);
    this.lastRunAt = this.now(); this.lastError = null;
    let uploadedNames;
    if (this.remote) {
      const items = await this.remote.listRecordings();
      uploadedNames = new Set(items.map(item => item.name));
      for (const item of items) {
        if (this.stopped) return;
        if (!expired(item.name) || pending(item.name)) continue;
        await this.remote.removeRecording(item.name); this.deletedRemote++;
      }
    }
    let entries;
    try {entries=await fs.promises.readdir(this.dir,{withFileTypes:true});}
    catch(error) {if(error.code==='ENOENT')return;throw error;}
    for (const item of entries) {
      if (this.stopped) return;
      const file = path.join(this.dir,item.name);
      // Open segments, unuploaded files, symlinks and recovery copies are preserved.
      if (!item.isFile() || !expired(item.name) || pending(item.name) ||
          (uploadedNames && !uploadedNames.has(item.name)) || !isFinalizedMp4(file)) continue;
      await fs.promises.unlink(file); this.deletedLocal++;
    }
  }
  stop() {this.stopped=true;clearInterval(this.timer);return this.running || Promise.resolve();}
}
function createRetention(cfg, baseDir, queue, settings = retentionSettings(cfg.recording)) {
  if (settings.days === 0 || !cfg.recording) return null;
  const rec=cfg.recording, destination=rec.destination || 'gdrive';
  let remote;
  if (destination === 'gdrive' || destination === 'onedrive') remote=new Rclone({remote:rec.rclone?.[destination],bin:rec.rclone?.bin});
  else if(destination !== 'local')throw new Error('Tujuan retensi tidak valid.');
  return new Retention({...settings,dir:path.resolve(baseDir,rec.dir || 'recordings'),remote,queue,cameras:()=>cfg.cameras});
}
module.exports = {retentionSettings, recordingTime, Retention, createRetention};
