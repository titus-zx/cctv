'use strict';
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const { Viewer } = require('./lib/viewer');
const { createRecording } = require('./lib/recorder');
const configPath = path.join(__dirname, 'config.json');
const cfg = JSON.parse(fs.readFileSync(configPath, 'utf8'));
const recording = createRecording(cfg, __dirname);
const viewers = cfg.cameras.map(camera => new Viewer(camera, recording?.make(camera)));
const port = Number(process.env.PORT || cfg.port || 3380);
const json = (res, code, data) => {res.writeHead(code, {'Content-Type':'application/json; charset=utf-8','Cache-Control':'no-store'});res.end(JSON.stringify(data));};
async function body(req) {
  let text = '';
  for await (const chunk of req) { text += chunk; if (text.length > 16384) throw new Error('Data terlalu besar.'); }
  try { return JSON.parse(text); } catch { throw new Error('JSON tidak valid.'); }
}
function saveConfig() {
  const temporary = `${configPath}.tmp`;
  fs.writeFileSync(temporary, JSON.stringify(cfg, null, 2) + '\n', {mode:0o600});
  fs.renameSync(temporary, configPath);
}
const server = http.createServer(async (req, res) => {
  try {
    const host = req.headers.host || '';
    if (![`localhost:${port}`,`127.0.0.1:${port}`].includes(host)) return json(res, 403, {error:'Akses hanya melalui localhost.'});
    if (req.method === 'POST' && req.headers.origin && ![`http://localhost:${port}`,`http://127.0.0.1:${port}`].includes(req.headers.origin))
      return json(res, 403, {error:'Origin tidak diizinkan.'});
    const url = new URL(req.url, 'http://localhost');
    if (req.method === 'GET' && url.pathname === '/api/cameras')
      return json(res, 200, viewers.map((viewer,id) => ({id,...viewer.status()})));
    const match = url.pathname.match(/^\/(stream|audio|snapshot|api\/cameras)\/(\d+)(?:\/(reconnect))?$/);
    if (match) {
      const id = Number(match[2]), viewer = viewers[id];
      if (!viewer) return json(res,404,{error:'Kamera tidak ditemukan.'});
      if (match[1] === 'stream' && req.method === 'GET') return viewer.subscribe(res);
      if (match[1] === 'audio' && req.method === 'GET') return viewer.subscribeAudio(res);
      if (match[1] === 'snapshot' && req.method === 'GET') {
        if (!viewer.lastJpeg || viewer.state !== 'live') return json(res,503,{error:'Gambar kamera belum tersedia.'});
        res.writeHead(200, {'Content-Type':'image/jpeg','Cache-Control':'no-store',
          'Content-Disposition':`attachment; filename="v380-${viewer.camera.deviceId}-${Date.now()}.jpg"`});
        return res.end(viewer.lastJpeg);
      }
      if (match[1] === 'api/cameras' && req.method === 'POST') {
        if (req.headers['content-type']?.split(';')[0] !== 'application/json') return json(res,415,{error:'Gunakan application/json.'});
        if (match[3] === 'reconnect') { viewer.reconnect(); return json(res,200,{ok:true}); }
        const input = await body(req);
        if (typeof input.ip !== 'string' || (input.ip !== '' && !net.isIP(input.ip)) || (input.mode !== 'cloud' && !input.ip)) throw new Error('Alamat IP tidak valid.');
        if (!/^\d{1,10}$/.test(String(input.deviceId)) || Number(input.deviceId) < 1 || Number(input.deviceId) > 0xffffffff) throw new Error('Device ID tidak valid.');
        if (!['v380','cloud','rtsp'].includes(input.mode)) throw new Error('Mode tidak valid.');
        if (!['sd','hd'].includes(input.quality)) throw new Error('Kualitas tidak valid.');
        const name = String(input.name || 'Kamera V380').slice(0,80);
        const username = String(input.username || 'admin');
        if (Buffer.byteLength(username) > 31) throw new Error('Username terlalu panjang.');
        const password = input.password === '' || input.password === undefined ? viewer.camera.password : input.password;
        if (typeof password !== 'string' || Buffer.byteLength(password) > 47) throw new Error('Password harus maksimal 47 byte.');
        const v380Port = Number(input.v380Port || 8800);
        if (!Number.isInteger(v380Port) || v380Port < 1 || v380Port > 65535) throw new Error('Port tidak valid.');
        const updated = {...viewer.camera, name, ip:input.ip, deviceId:String(input.deviceId), username, password, mode:input.mode,quality:input.quality,v380Port};
        if (input.mode === 'rtsp') {
          const parsed = new URL(input.rtspUrl);
          if (parsed.protocol !== 'rtsp:') throw new Error('URL harus diawali rtsp://.');
          updated.rtspUrl = parsed.href;
        } else delete updated.rtspUrl;
        const previous = cfg.cameras[id]; cfg.cameras[id] = updated;
        try {saveConfig();} catch (error) {cfg.cameras[id] = previous; throw error;}
        viewer.camera = updated; if (viewer.recorder) viewer.recorder.camera = updated; viewer.reconnect();
        return json(res,200,{ok:true});
      }
      return json(res,405,{error:'Metode tidak didukung.'});
    }
    const files = {'/':'index.html','/index.html':'index.html','/app.js':'app.js','/style.css':'style.css'};
    const file = files[url.pathname];
    if (req.method !== 'GET' || !file) return json(res,404,{error:'Tidak ditemukan.'});
    const types = {'.html':'text/html; charset=utf-8','.js':'text/javascript; charset=utf-8','.css':'text/css; charset=utf-8'};
    res.writeHead(200, {'Content-Type':types[path.extname(file)],'Cache-Control':'no-store',
      'X-Content-Type-Options':'nosniff','Content-Security-Policy':"default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'"});
    fs.createReadStream(path.join(__dirname,'public',file)).pipe(res);
  } catch (error) { if (!res.headersSent) json(res,400,{error:error.message}); else res.end(); }
});
server.on('error', error => {console.error(`Server gagal: ${error.code || 'error'}`);for(const viewer of viewers)viewer.stop();process.exitCode=1;});
server.listen(port,'127.0.0.1',() => {console.log(`V380 Viewer: http://localhost:${port}`);for(const viewer of viewers)viewer.start();});
function shutdown() {for(const viewer of viewers)viewer.stop();server.close(()=>process.exit(0));setTimeout(()=>process.exit(0),2000).unref();}
process.on('SIGINT',shutdown);process.on('SIGTERM',shutdown);
