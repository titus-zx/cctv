'use strict';
(() => {
  const $ = selector => document.querySelector(selector);
  const video = $('#pb-video'), list = $('#pb-list'), camera = $('#pb-camera'), date = $('#pb-date');
  let items = [], current = -1, loaded = false, page = 1, pages = 1;
  const SIZE = 50;
  const pad = n => String(n).padStart(2, '0');
  const today = () => { const d = new Date(); return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`; };
  const time = iso => new Date(iso).toLocaleTimeString('id-ID', {hour: '2-digit', minute: '2-digit', second: '2-digit'});
  const size = bytes => bytes >= 1048576 ? `${(bytes / 1048576).toFixed(1)} MB` : `${Math.max(1, Math.round(bytes / 1024))} KB`;
  function show(view) {
    for (const link of document.querySelectorAll('[data-view]')) link.classList.toggle('active', link.dataset.view === view);
    $('#live-view').hidden = view !== 'live'; $('#playback-view').hidden = view !== 'playback';
    $('#crumb').textContent = view === 'live' ? 'Live view' : 'Rekaman';
    if (view === 'playback') { fillCameras(); if (!loaded) load(); } else video.pause();
  }
  function fillCameras() {
    if (camera.options.length > 1 || !window.cameraList?.length) return;
    camera.innerHTML = '<option value="all">Semua kamera</option>' + window.cameraList.map(c => `<option value="${c.id}"></option>`).join('');
    window.cameraList.forEach((c, i) => camera.options[i + 1].textContent = c.name);
  }
  function play(index, auto = true) {
    const item = items[index]; if (!item) return;
    current = index; video.src = `/recordings/${encodeURIComponent(item.name)}`;
    $('#pb-now').textContent = `${item.camera || `Device ${item.deviceId}`} · ${new Date(item.start).toLocaleString('id-ID')}`;
    if (auto) video.play().catch(() => {});
    for (const [i, li] of [...list.children].entries()) li.classList.toggle('active', i === index);
    list.children[index]?.scrollIntoView({block: 'nearest'});
  }
  async function load(keepPage = false) {
  if (keepPage !== true) page = 1;
    fillCameras(); $('#pb-count').textContent = 'Memuat…'; list.textContent = '';
    try {
      const query = new URLSearchParams({camera: camera.value || 'all', date: date.value, from: $('#pb-from').value, to: $('#pb-to').value, page, size: SIZE});
      const response = await fetch(`/api/recordings?${query}`, {cache: 'no-store'}), data = await response.json();
      if (!response.ok) throw new Error(data.error || 'Gagal memuat rekaman.');
      loaded = true; items = data.items; current = -1; page = data.page; pages = data.pages;
      $('#pb-count').textContent = data.enabled ? `${data.total} rekaman` : 'Perekaman belum diaktifkan di config.json';
      $('#pb-page').textContent = `Halaman ${page} / ${pages}`; $('#pb-prev').disabled = page <= 1; $('#pb-next').disabled = page >= pages;
      items.forEach((item, i) => {
        const li = document.createElement('li'), button = document.createElement('button');
        button.innerHTML = '<b></b><span></span><small></small>';
        button.children[0].textContent = time(item.start); button.children[1].textContent = item.camera || `Device ${item.deviceId}`;
        button.children[2].textContent = size(item.size); button.onclick = () => play(i);
        li.append(button); list.append(li);
      });
      if (!items.length && data.enabled) list.innerHTML = '<li class="pb-empty">Tidak ada rekaman pada tanggal dan jam ini.</li>';
    } catch (error) { $('#pb-count').textContent = error.message; }
  }
  video.onended = () => { if (current > 0) play(current - 1); }; // list is newest first, so the next clip in time is the one above
  video.onerror = () => { if (current >= 0) $('#pb-now').textContent = 'Rekaman tidak dapat diputar (belum selesai disalin atau terhapus).'; };
  camera.onchange = date.onchange = $('#pb-from').onchange = $('#pb-to').onchange = () => load(); $('#pb-reload').onclick = () => load(true);
  $('#pb-prev').onclick = () => { page--; load(true); }; $('#pb-next').onclick = () => { page++; load(true); };
  date.value = today();
  const route = () => show(location.hash === '#playback' ? 'playback' : 'live');
  window.addEventListener('hashchange', route); route();
})();
