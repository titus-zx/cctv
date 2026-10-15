'use strict';
const http = require('node:http');
const fs = require('node:fs');
const path = require('node:path');
const net = require('node:net');
const { Viewer } = require('./lib/viewer');
const { createRecording } = require('./lib/recorder');
const { createStore, NAME } = require('./lib/playback');
const { createRetention, retentionSettings } = require('./lib/retention');
const { CameraEvents } = require('./lib/camera-events');
const configPath = path.resolve(process.env.CONFIG_PATH || path.join(__dirname, 'config.json'));
const cfg = JSON.parse(fs.readFileSync(configPath, 'utf8'));
const retentionConfig = retentionSettings(cfg.recording);
const recording = createRecording(cfg, __dirname);
const retention = createRetention(cfg, __dirname, recording?.queue, retentionConfig);
const store = createStore(cfg, __dirname);
const viewers = cfg.cameras.map(camera => new Viewer(camera, recording?.make(camera)));
const cameraSnapshot = () => viewers.map((viewer,id) => ({id,...viewer.status()}));
const cameraEvents = new CameraEvents(cameraSnapshot);
const port = Number(process.env.PORT || cfg.port || 3380);
const publicPort = Number(process.env.PUBLIC_PORT || port);
const allowedHosts = new Set([`localhost:${port}`, `127.0.0.1:${port}`, `localhost:${publicPort}`, `127.0.0.1:${publicPort}`]);
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
    if (!allowedHosts.has(host)) return json(res, 403, {error:'Akses hanya melalui localhost.'});
    if (req.method === 'POST' && req.headers.origin && ![...allowedHosts].some(host => req.headers.origin === `http://${host}`))
      return json(res, 403, {error:'Origin tidak diizinkan.'});
    const url = new URL(req.url, 'http://localhost');
    if (req.method === 'GET' && url.pathname === '/api/cameras')
      return json(res, 200, cameraSnapshot());
    if (req.method === 'GET' && url.pathname === '/api/cameras/events') {
      const audio = url.searchParams.get('audio');
      if (audio !== null && (!/^\d+$/.test(audio) || !viewers[Number(audio)])) return json(res,400,{error:'Kamera audio tidak valid.'});
      return cameraEvents.subscribe(res, audio === null ? null : viewers[Number(audio)]);
    }
    if (req.method === 'GET' && url.pathname === '/api/retention')
      return json(res, 200, retention ? {enabled:true,...retention.status()} : {enabled:false,...retentionConfig});
    if (req.method === 'GET' && url.pathname === '/api/recordings') {
      if (!store) return json(res, 200, {enabled: false, items: []});
      const items = await store.list(url.searchParams.get('date'));
      const names = new Map(viewers.map((v, id) => [String(v.camera.deviceId), {cameraId: id, camera: v.camera.name}]));
      const wanted = url.searchParams.get('camera');
      const minutes = key => { const m = /^(\d{2}):(\d{2})$/.exec(url.searchParams.get(key) || ''); return m ? Number(m[1]) * 60 + Number(m[2]) : null; };
      const from = minutes('from') ?? 0, to = minutes('to') ?? 24 * 60 - 1; // 'to' covers the whole minute
      const size = Math.min(200, Math.max(1, Number(url.searchParams.get('size')) || 50));
      const matches = items.map(i => ({...i, ...names.get(i.deviceId)}))
        .filter(i => wanted === null || wanted === 'all' || String(i.cameraId) === wanted)
        .filter(i => { const d = new Date(i.start), m = d.getHours() * 60 + d.getMinutes(); return m >= from && m <= to; })
        .sort((a, b) => a.start < b.start ? 1 : a.start > b.start ? -1 : 0); // newest first
      const pages = Math.max(1, Math.ceil(matches.length / size)), page = Math.min(pages, Math.max(1, Number(url.searchParams.get('page')) || 1));
      return json(res, 200, {enabled: true, total: matches.length, page, pages, size, items: matches.slice((page - 1) * size, page * size)});
    }
    const clip = url.pathname.match(/^\/recordings\/([^/]+)$/);
    if (req.method === 'GET' && clip) {
      const name = decodeURIComponent(clip[1]);
      if (!store || !NAME.test(name)) return json(res, 404, {error: 'Rekaman tidak ditemukan.'});
      const file = await store.open(name, req.headers.range);
      if (!file) return json(res, 404, {error: 'Rekaman tidak ditemukan.'});
      res.writeHead(file.status, {'Content-Type': 'video/mp4', 'Accept-Ranges': 'bytes', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff', ...file.headers});
      if (!file.stream) return res.end();
      res.on('close', () => file.stream.destroy()); return file.stream.pipe(res);
    }
    const match = url.pathname.match(/^\/(stream|audio|snapshot|api\/cameras)\/(\d+)(?:\/(reconnect|ptz|talk))?$/);
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
        if (match[3] === 'talk') { // raw G.711 A-law, 8 kHz mono, at most 480 bytes per request
          if (req.headers['content-type'] !== 'application/octet-stream') return json(res,415,{error:'Gunakan application/octet-stream.'});
          const chunks = []; let size = 0;
          for await (const chunk of req) { size += chunk.length; if (size > 480) throw new Error('Data audio terlalu besar.'); chunks.push(chunk); }
          viewer.talk(Buffer.concat(chunks)); return json(res,200,{ok:true});
        }
        if (req.headers['content-type']?.split(';')[0] !== 'application/json') return json(res,415,{error:'Gunakan application/json.'});
        if (match[3] === 'reconnect') { viewer.reconnect(); cameraEvents.publish(); return json(res,200,{ok:true}); }
        if (match[3] === 'ptz') {
          const input = await body(req);
          if (!['up','down','left','right','stop'].includes(input.direction)) throw new Error('Arah tidak valid.');
          viewer.ptz(input.direction); return json(res,200,{ok:true});
        }
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
        cameraEvents.publish();
        return json(res,200,{ok:true});
      }
      return json(res,405,{error:'Metode tidak didukung.'});
    }
    const files = {'/':'index.html','/index.html':'index.html','/app.js':'app.js','/playback.js':'playback.js','/style.css':'style.css'};
    const file = files[url.pathname];
    if (req.method !== 'GET' || !file) return json(res,404,{error:'Tidak ditemukan.'});
    const types = {'.html':'text/html; charset=utf-8','.js':'text/javascript; charset=utf-8','.css':'text/css; charset=utf-8'};
    res.writeHead(200, {'Content-Type':types[path.extname(file)],'Cache-Control':'no-store',
      'X-Content-Type-Options':'nosniff','Content-Security-Policy':"default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'"});
    fs.createReadStream(path.join(__dirname,'public',file)).pipe(res);
  } catch (error) { if (!res.headersSent) json(res,400,{error:error.message}); else res.end(); }
});
server.on('error', error => {console.error(`Server gagal: ${error.code || 'error'}`);for(const viewer of viewers)viewer.stop();process.exitCode=1;});
server.listen(port,process.env.HOST || '127.0.0.1',() => {console.log(`V380 Viewer: http://localhost:${publicPort}`);for(const viewer of viewers)viewer.start();retention?.start();});
let shuttingDown = false;
async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  cameraEvents.close();
  for (const viewer of viewers) viewer.stop();
  server.close();
  const deadline = setTimeout(() => process.exit(1), 12000); deadline.unref();
  await Promise.allSettled([...viewers.map(viewer => viewer.recorder?.stop()), retention?.stop()]);
  clearTimeout(deadline); process.exit(0);
}
process.on('SIGINT',shutdown);process.on('SIGTERM',shutdown);
