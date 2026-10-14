'use strict';
const fs = require('node:fs');
const path = require('node:path');
// Google Drive upload via OAuth2 refresh token (files are owned by the user's own Drive account).
class DriveUploader {
  constructor({ clientId, clientSecret, refreshToken, folderId }, request = fetch) {
    if (!clientId || !clientSecret || !refreshToken) throw new Error('Konfigurasi Google Drive belum lengkap (clientId, clientSecret, refreshToken).');
    Object.assign(this, { clientId, clientSecret, refreshToken, folderId, request });
    this.token = null; this.expiresAt = 0;
  }
  async accessToken() {
    if (this.token && Date.now() < this.expiresAt - 60000) return this.token;
    const res = await this.request('https://oauth2.googleapis.com/token', {
      method: 'POST', redirect: 'error', headers: {'Content-Type': 'application/x-www-form-urlencoded'},
      body: new URLSearchParams({client_id: this.clientId, client_secret: this.clientSecret,
        refresh_token: this.refreshToken, grant_type: 'refresh_token'}),
    });
    const data = await res.json().catch(() => ({}));
    if (!res.ok || !data.access_token) throw new Error(`Token Google ditolak (${data.error || res.status}).`);
    this.token = data.access_token; this.expiresAt = Date.now() + (data.expires_in || 3600) * 1000;
    return this.token;
  }
  async upload(file) {
    const body = await fs.promises.readFile(file);
    const metadata = {name: path.basename(file), ...(this.folderId ? {parents: [this.folderId]} : {})};
    const token = await this.accessToken();
    const start = await this.request('https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&fields=id', {
      method: 'POST', redirect: 'error',
      headers: {Authorization: `Bearer ${token}`, 'Content-Type': 'application/json; charset=UTF-8',
        'X-Upload-Content-Type': 'video/mp4', 'X-Upload-Content-Length': String(body.length)},
      body: JSON.stringify(metadata),
    });
    if (start.status === 401) this.token = null;
    const location = start.headers.get('location');
    if (!start.ok || !location) throw new Error(`Drive menolak upload (${start.status}).`);
    const done = await this.request(location, {method: 'PUT', redirect: 'error', headers: {'Content-Type': 'video/mp4'}, body});
    if (!done.ok) throw new Error(`Upload Drive gagal (${done.status}).`);
    return (await done.json()).id;
  }
}
// Playback side: list a folder's MP4s and stream one back (Range is passed through so <video> can seek).
DriveUploader.prototype.list = async function ({ after, before } = {}) {
  const files = []; let pageToken;
  for (let page = 0; page < 10; page++) {
    const q = [`trashed=false`, `mimeType='video/mp4'`, this.folderId && `'${this.folderId}' in parents`,
      after && `modifiedTime >= '${after}'`, before && `modifiedTime < '${before}'`].filter(Boolean).join(' and ');
    const url = new URL('https://www.googleapis.com/drive/v3/files');
    url.search = new URLSearchParams({q, fields: 'nextPageToken,files(id,name,size)', pageSize: '1000', orderBy: 'name desc',
      ...(pageToken ? {pageToken} : {})});
    const res = await this.request(url, {redirect: 'error', headers: {Authorization: `Bearer ${await this.accessToken()}`}});
    if (!res.ok) throw new Error(`Daftar Drive gagal (${res.status}).`);
    const data = await res.json(); files.push(...data.files);
    if (!(pageToken = data.nextPageToken)) break;
  }
  return files;
};
DriveUploader.prototype.find = async function (name) {
  const q = [`name='${name}'`, 'trashed=false', this.folderId && `'${this.folderId}' in parents`].filter(Boolean).join(' and ');
  const url = new URL('https://www.googleapis.com/drive/v3/files');
  url.search = new URLSearchParams({q, fields: 'files(id,size)', pageSize: '1'});
  const res = await this.request(url, {redirect: 'error', headers: {Authorization: `Bearer ${await this.accessToken()}`}});
  if (!res.ok) throw new Error(`Pencarian Drive gagal (${res.status}).`);
  return (await res.json()).files[0] || null;
};
DriveUploader.prototype.download = async function (id, range) {
  return this.request(`https://www.googleapis.com/drive/v3/files/${id}?alt=media`,
    {headers: {Authorization: `Bearer ${await this.accessToken()}`, ...(range ? {Range: range} : {})}});
};
module.exports = { DriveUploader };
