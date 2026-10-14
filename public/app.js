'use strict';
const grid = document.querySelector('#grid');
const dialog = document.querySelector('#settings');
const form = document.querySelector('#settings-form');
const cards = new Map();
let cameras = [], toastTimer;
function toast(message) {
  const el = document.querySelector('#toast'); el.textContent = message; el.hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => el.hidden = true, 4000);
}
async function api(url, data) {
  const response = await fetch(url, data === undefined ? {cache:'no-store'} : {
    method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});
  const result = await response.json();
  if (!response.ok) throw new Error(result.error || 'Permintaan gagal.');
  return result;
}
function openSettings(camera) {
  form.reset();
  for (const field of ['id','name','ip','deviceId','username','mode','quality','v380Port']) form.elements[field].value = camera[field];
  form.elements.password.placeholder = camera.hasPassword ? 'Kosongkan untuk mempertahankan password' : 'Masukkan password kamera';
  document.querySelector('#form-error').textContent = '';
  updateModeFields();
  dialog.showModal();
}
async function reconnect(id) {
  try { await api(`/api/cameras/${id}/reconnect`,{}); await refresh(); }
  catch(error) {toast(error.message);}
}
// G.711 A-law → float samples (ITU-T G.711 reference decoding).
const alawTable = Float32Array.from({length:256}, (_, i) => {
  const a = i ^ 0x55, seg = (a & 0x70) >> 4; let t = (a & 0x0f) << 4;
  t = seg === 0 ? t + 8 : (t + 0x108) << Math.max(0, seg - 1);
  return ((a & 0x80) ? t : -t) / 32768;
});
const sound = {context:null, id:null, abort:null, next:0};
function stopSound() {
  sound.abort?.abort(); sound.abort = null; sound.id = null;
  for (const button of document.querySelectorAll('.sound')) {
    button.textContent = '🔇'; button.setAttribute('aria-pressed','false'); button.title = 'Aktifkan suara';
  }
}
async function playSound(id, button) {
  sound.context ||= new AudioContext({sampleRate:8000});
  await sound.context.resume();
  const abort = sound.abort = new AbortController(); sound.id = id; sound.next = 0;
  button.textContent = '🔊'; button.setAttribute('aria-pressed','true'); button.title = 'Matikan suara';
  try {
    const response = await fetch(`/audio/${id}`, {signal:abort.signal});
    const reader = response.body.getReader();
    while (true) {
      const {value, done} = await reader.read(); if (done) break;
      const samples = Float32Array.from(value, byte => alawTable[byte]);
      const buffer = sound.context.createBuffer(1, samples.length, 8000); buffer.copyToChannel(samples, 0);
      const source = sound.context.createBufferSource(); source.buffer = buffer; source.connect(sound.context.destination);
      const now = sound.context.currentTime;
      // Keep ~150 ms of buffer; drop audio when latency grows beyond 1 s so sound stays in sync with video.
      if (sound.next < now + 0.02) sound.next = now + 0.15;
      if (sound.next > now + 1) continue;
      source.start(sound.next); sound.next += buffer.duration;
    }
  } catch (error) { if (error.name !== 'AbortError') toast('Audio terputus.'); }
  if (sound.abort === abort) stopSound();
}
// Push-to-talk: microphone → 8 kHz mono → G.711 A-law → 480-byte (60 ms) POSTs while the button is held.
function alawEncode(sample) {
  let pcm = Math.max(-32768, Math.min(32767, Math.round(sample * 32767))), mask = 0xd5;
  if (pcm < 0) { mask = 0x55; pcm = -pcm - 1; }
  if (pcm > 0x7fff) pcm = 0x7fff;
  let seg = 0; for (let v = pcm >> 8; v; v >>= 1) seg++;
  if (seg >= 8) return 0x7f ^ mask;
  return (((seg << 4) | (seg < 2 ? (pcm >> 4) & 0x0f : (pcm >> (seg + 3)) & 0x0f))) ^ mask;
}
const talk = {stream:null, context:null, node:null, source:null};
async function startTalk(id) {
  talk.stream ||= await navigator.mediaDevices.getUserMedia({audio:{echoCancellation:true,noiseSuppression:true,channelCount:1}});
  talk.context ||= new AudioContext();
  await talk.context.resume();
  const ratio = talk.context.sampleRate / 8000; let position = 0, pending = [];
  talk.source = talk.context.createMediaStreamSource(talk.stream);
  talk.node = talk.context.createScriptProcessor(2048, 1, 1);
  talk.node.onaudioprocess = event => {
    const input = event.inputBuffer.getChannelData(0);
    for (; position < input.length; position += ratio) { // box-filter resample to 8 kHz
      const start = Math.floor(position), end = Math.min(input.length, Math.max(start + 1, Math.floor(position + ratio)));
      let sum = 0; for (let i = start; i < end; i++) sum += input[i];
      pending.push(alawEncode(sum / (end - start)));
    }
    position -= input.length;
    while (pending.length >= 480) {
      const chunk = Uint8Array.from(pending.splice(0, 480));
      fetch(`/api/cameras/${id}/talk`, {method:'POST', headers:{'Content-Type':'application/octet-stream'}, body:chunk}).catch(() => {});
    }
  };
  talk.source.connect(talk.node); talk.node.connect(talk.context.destination); // node needs a sink to run; output is silent
}
function stopTalk() {
  talk.node?.disconnect(); talk.source?.disconnect(); talk.node = talk.source = null;
  for (const button of document.querySelectorAll('.talk')) button.classList.remove('active');
}
function createCard(camera) {
  const card = document.querySelector('#camera-template').content.firstElementChild.cloneNode(true);
  card.querySelector('.settings').onclick = () => openSettings(cameras.find(c=>c.id===camera.id));
  card.querySelector('.retry').onclick = () => reconnect(camera.id);
  card.querySelector('.sound').onclick = event => {
    const wasPlaying = sound.id === camera.id; stopSound();
    if (!wasPlaying) playSound(camera.id, event.currentTarget).catch(error => toast(error.message));
  };
  const talkButton = card.querySelector('.talk'); let talking = false;
  talkButton.onpointerdown = async event => {
    event.preventDefault(); talkButton.setPointerCapture(event.pointerId); talking = true;
    try { await startTalk(camera.id); } catch { talking = false; return toast('Mikrofon tidak dapat diakses. Izinkan mikrofon di browser.'); }
    if (talking) talkButton.classList.add('active'); else stopTalk();
  };
  talkButton.onpointerup = talkButton.onpointercancel = () => { talking = false; stopTalk(); };
  // Hold a button to move; the camera keeps moving until told to stop, so repeat while held and always send stop on release.
  const ptz = card.querySelector('.ptz'); let ptzTimer;
  const send = direction => api(`/api/cameras/${camera.id}/ptz`, {direction}).catch(error => { clearInterval(ptzTimer); toast(error.message); });
  const release = () => { if (ptzTimer) { clearInterval(ptzTimer); ptzTimer = null; send('stop'); } };
  for (const button of ptz.querySelectorAll('button')) {
    button.onpointerdown = event => { event.preventDefault(); button.setPointerCapture(event.pointerId); send(button.dataset.dir); ptzTimer = setInterval(() => send(button.dataset.dir), 700); };
    button.onpointerup = button.onpointercancel = release;
    button.onkeydown = event => { if ((event.key === 'Enter' || event.key === ' ') && !event.repeat) { event.preventDefault(); send(button.dataset.dir); } };
    button.onkeyup = event => { if (event.key === 'Enter' || event.key === ' ') send('stop'); };
  }
  const img = card.querySelector('.video');
  img.onerror = () => img.removeAttribute('src');
  card.querySelector('.fullscreen').onclick = async () => {
    try {await card.querySelector('.video-stage').requestFullscreen();} catch {toast('Layar penuh tidak tersedia di browser ini.');}
  };
  const snapshot = card.querySelector('.snapshot');
  snapshot.href = `/snapshot/${camera.id}`;
  snapshot.onclick = async event => {
    event.preventDefault();
    if (card.dataset.state !== 'live') return;
    try {
      const response = await fetch(`/snapshot/${camera.id}`);
      if (!response.ok) throw new Error('Snapshot belum tersedia.');
      const objectUrl = URL.createObjectURL(await response.blob());
      const link = document.createElement('a');link.href=objectUrl;
      link.download=`v380-${camera.deviceId}-${new Date().toISOString().replace(/[:.]/g,'-')}.jpg`;
      document.body.append(link);link.click();link.remove();setTimeout(()=>URL.revokeObjectURL(objectUrl),1000);
    } catch(error) {toast(error.message);}
  };
  grid.append(card); cards.set(camera.id,card);return card;
}
function render(camera) {
  const card = cards.get(camera.id) || createCard(camera);
  const live = camera.state === 'live';
  card.dataset.state = camera.state;
  card.querySelector('h2').textContent = camera.name;
  card.querySelector('.camera-address').textContent = camera.mode === 'cloud' ? 'Internet · Cloud V380' : `${camera.ip} · ${camera.mode.toUpperCase()}`;
  card.querySelector('.device-id').textContent = `DEVICE ID ${camera.deviceId || '—'}`;
  card.querySelector('.resolution').textContent = camera.width && camera.height ? `${camera.width} × ${camera.height}` : '—';
  card.querySelector('.status-text').textContent = live ? (camera.previewError ? 'Terhubung · tampilan bermasalah' : 'Terhubung') : camera.state==='error' ? 'Koneksi bermasalah' : 'Menghubungkan…';
  const img = card.querySelector('.video');
  if (!img.hasAttribute('src')) img.src = `/stream/${camera.id}?t=${Date.now()}`;
  img.hidden = !live;
  card.querySelector('.video-overlay').hidden = live;
  card.querySelector('.video-overlay h3').textContent = camera.state==='error' ? 'Kamera belum menampilkan video' : 'Menghubungkan kamera';
  card.querySelector('.video-overlay p').textContent = camera.message;
  card.querySelector('.retry').hidden = camera.state !== 'error';
  card.querySelector('.live-badge').hidden = !live;
  card.querySelector('.video-quality').hidden = !live;
  card.querySelector('.video-quality').textContent = camera.quality.toUpperCase();
  card.querySelector('.ptz').hidden = !live;
  card.querySelector('.talk').hidden = !(live && camera.audio);
  card.querySelector('.sound').hidden = !(live && camera.audio);
  card.querySelector('.fullscreen').disabled = !live;
  card.querySelector('.snapshot').setAttribute('aria-disabled',String(!live));
}
async function refresh() {
  try {
    cameras = await api('/api/cameras'); window.cameraList = cameras;
    cameras.forEach(render);
    document.querySelector('#total').textContent = cameras.length;
    document.querySelector('#camera-count').textContent = cameras.length;
    document.querySelector('#live').textContent = cameras.filter(c=>c.state==='live').length;
    const cloudCount = cameras.filter(c=>c.mode==='cloud').length;
    document.querySelector('#connection-mode').textContent = cloudCount === 0 ? 'Jaringan rumah' : cloudCount === cameras.length ? 'Cloud V380' : 'LAN + Cloud';
    document.querySelector('#connection-note').textContent = cloudCount ? 'Kamera cloud terhubung melalui relay V380' : 'Video melalui jaringan rumah';
    document.querySelector('#updated').textContent = `Diperbarui ${new Date().toLocaleTimeString('id-ID')}`;
    document.querySelector('#global-error').hidden = true;
  } catch {
    const el = document.querySelector('#global-error');el.textContent='Server viewer tidak terjangkau. Periksa apakah npm start masih berjalan.';el.hidden=false;
    for(const card of cards.values()){
      card.dataset.state='error';card.querySelector('.video').hidden=true;
      card.querySelector('.video-overlay').hidden=false;card.querySelector('.video-overlay h3').textContent='Server terputus';
      card.querySelector('.video-overlay p').textContent='Menunggu server viewer kembali terhubung…';
      card.querySelector('.live-badge').hidden=true;card.querySelector('.status-text').textContent='Server terputus';
    }
  }
}
function updateModeFields() {
  const cloud = form.elements.mode.value === 'cloud';
  document.querySelector('#rtsp-field').hidden = form.elements.mode.value !== 'rtsp';
  document.querySelector('#cloud-note').hidden = !cloud;
  form.elements.ip.required = !cloud;
  form.elements.ip.readOnly = cloud;
  form.elements.v380Port.readOnly = cloud;
}
form.elements.mode.onchange = updateModeFields;
form.onsubmit = async event => {
  event.preventDefault();
  const submit = form.querySelector('[type=submit]'); submit.disabled = true;
  try {
    const data = Object.fromEntries(new FormData(form));
    await api(`/api/cameras/${data.id}`,data);dialog.close();toast('Konfigurasi disimpan. Menghubungkan kamera…');await refresh();
  } catch(error) {document.querySelector('#form-error').textContent=error.message;}
  finally {submit.disabled=false;}
};
for (const id of ['close-settings','cancel-settings']) document.querySelector(`#${id}`).onclick=()=>dialog.close();
document.querySelector('#refresh').onclick=async event=>{
  const button=event.currentTarget;button.disabled=true;
  await Promise.all(cameras.map(c=>reconnect(c.id)));button.disabled=false;
};
function clock(){document.querySelector('#clock').textContent=new Date().toLocaleTimeString('id-ID',{hour:'2-digit',minute:'2-digit',second:'2-digit'});}
clock();setInterval(clock,1000);refresh();setInterval(refresh,2500);
