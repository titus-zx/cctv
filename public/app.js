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
function createCard(camera) {
  const card = document.querySelector('#camera-template').content.firstElementChild.cloneNode(true);
  card.querySelector('.settings').onclick = () => openSettings(cameras.find(c=>c.id===camera.id));
  card.querySelector('.retry').onclick = () => reconnect(camera.id);
  card.querySelector('.sound').onclick = event => {
    const wasPlaying = sound.id === camera.id; stopSound();
    if (!wasPlaying) playSound(camera.id, event.currentTarget).catch(error => toast(error.message));
  };
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
  card.querySelector('.status-text').textContent = live ? 'Terhubung' : camera.state==='error' ? 'Koneksi bermasalah' : 'Menghubungkan…';
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
  card.querySelector('.sound').hidden = !(live && camera.audio);
  card.querySelector('.fullscreen').disabled = !live;
  card.querySelector('.snapshot').setAttribute('aria-disabled',String(!live));
}
async function refresh() {
  try {
    cameras = await api('/api/cameras');
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
