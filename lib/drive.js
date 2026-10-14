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
module.exports = { DriveUploader };
