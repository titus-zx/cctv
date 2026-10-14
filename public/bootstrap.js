import init from '/pkg/cctv_ui.js';
init().catch(error => {
  console.error(error);
  const message = document.querySelector('#global-error');
  message.textContent = 'Aplikasi belum dapat dimuat. Muat ulang halaman.';
  message.hidden = false;
});
