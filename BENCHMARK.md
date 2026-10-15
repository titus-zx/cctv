# Benchmark Docker Node.js vs Rust

Pengukuran 15 Oktober 2026 pada Docker Desktop Linux ARM64 native (10 CPU tersedia,
RAM VM 7,75 GiB). Node tersedia di http://localhost:3381 dan Rust di
http://localhost:3382. Masing-masing dibatasi 2 CPU dan RAM 1 GiB.

## Hasil replay terkontrol

| Metrik | Node.js | Rust |
| --- | ---: | ---: |
| CPU saat streaming (% satu core) | 37.02 | 33.77 |
| RAM saat streaming (MiB) | 151.48 | 58.39 |
| Total preview FPS | 18.42 | 18.32 |
| API p50 (ms) | 0.94 | 0.84 |
| API p95 (ms) | 3.19 | 1.82 |
| CPU tanpa penonton (% satu core) | 9.22 | 6.32 |
| RAM tanpa penonton (MiB) | 20.34 | 1.35 |

Rust menggunakan CPU 8.8% lebih rendah
serta RAM 61.5% lebih rendah pada beban ini.
FPS berbeda kurang dari 1%; Rust lebih hemat resource tanpa peningkatan FPS berarti.
API tidak mengalami error (600 request per versi). Angka CPU 100% berarti satu core,
sehingga 37% berarti sekitar 0,37 core, bukan 37% dari seluruh CPU host.

## Metode dan batas interpretasi

- Tiga pengukuran streaming, masing-masing 20 detik setelah warmup 8 detik;
  pengukuran tanpa penonton 20 detik. Kedua versi berjalan bersamaan.
- Input identik melalui fixture TCP V380 terenkripsi: tiga H.264 1280×720 16 fps
  (video sintetis berulang delapan detik) dan satu JPEG 640×480 16 fps
  (gambar tetap berulang). Tidak ada audio pada replay.
- Satu klien MJPEG per kamera per versi. Preview diset 5 fps, maksimum lebar
  640 piksel; FFmpeg 5.1.9 Debian ARM64 dan satu thread decoder/encoder pada keduanya.
  Total yang diterima sekitar 18,4 fps: tiga H.264 sekitar 4,8 fps dan JPEG sekitar
  4 fps. Target konfigurasi bukan jaminan FPS output persis 5.
- CPU mencakup proses aplikasi dan anak FFmpeg. RAM working set = usage dikurangi
  inactive file cache, bukan hanya heap bahasa. Heap Node belum tentu sudah mencapai
  kondisi stabil dalam pengujian singkat ini.
- API `/api/cameras`: HTTP keep-alive, target 100 request/detik, dua batch masing-masing
  300 request per versi dalam urutan AB lalu BA, saat preview aktif. Tabel merata-ratakan
  p50/p95 tiap batch; ini bukan pengukuran throughput maksimum atau latensi SSE.
- Semua kamera replay live tanpa error preview. Fixture pernah memicu satu koneksi
  ulang awal pada masing-masing kamera/kedua versi; ini terjadi sebelum sampel streaming.
- Tidak menguji rendering browser/WASM, recording, upload, retensi, kualitas gambar
  secara objektif, jumlah klien maksimum atau kestabilan berjam-jam. Hasil berlaku
  untuk implementasi dan workload ini; bukan kesimpulan universal Rust vs Node.

## Sumber dan perubahan

- `HEAD`: `0906808d3c7c29ba01404b0ed46ab3c5c9b7da26`.
- `origin/rust`: `8c1eac341d5c78bc5185a18c8f92bd29ccf19ea5`.
- Node menyertakan perbaikan satu baris: inisialisasi `audioListeners` agar paket audio
  kamera asli tidak menjatuhkan koneksi. Semua 47 test Node lulus, termasuk test audio SSE.
- Branch aktif tetap `main`; Rust diekspor ke direktori private `.benchmark/rust`.
  Salinan Dockerfile disesuaikan agar ARM64 native, tanpa mengubah Dockerfile produksi.
- `config.json`, `docker-data/config/config.json`, dan kedua config benchmark:
  `recording.enabled=false`, semua `camera.record=false`, `recording.retentionDays=0`.
  Tidak ada mount kredensial rclone pada container pembanding.
- Hasil mentah: `.benchmark/results/controlled.json` (diabaikan Git); salinan metrik
  tanpa metadata kamera: [results-controlled.json](tools/benchmark/results-controlled.json).
  Cara menjalankan ulang: [tools/benchmark/README.md](tools/benchmark/README.md).

## Verifikasi kamera asli

RuangTamu, Teras1 dan Teras2 berhasil streaming pada kedua versi. Kitchen menolak
login dengan kode 1002 pada keduanya dan dikecualikan dari pengukuran live.
Kedua container tetap berjalan dengan config kamera asli; fixture sudah dihentikan.
Rekaman/upload tetap mati. Dua putaran streaming masing-masing 10 detik:

| Metrik | Node.js | Rust |
| --- | ---: | ---: |
| CPU (% satu core) | 17.43 | 16.03 |
| RAM (MiB) | 113.35 | 59.14 |
| Total preview FPS | 13.67 | 14.02 |
| API p95 (ms) | 1.82 | 1.18 |

Tidak ada error pada 600 request API per versi. Uji live ini lebih singkat dan
bergantung pada koneksi cloud serta perilaku kamera; gunakan replay identik sebagai
perbandingan utama. Audio asli tetap mengikuti config kamera.
Metrik tersimpan di [results-live.json](tools/benchmark/results-live.json).

## Pengukuran ulang kamera asli (15 Oktober 2026, container berjalan ±1 jam)

Tiga kamera live (RuangTamu, Teras1, Teras2; Kitchen menolak login 1002 pada keduanya),
satu klien MJPEG per kamera per versi, tiga putaran 30 detik, recording/upload/retensi mati.
Container tanpa restart dan tanpa OOM.

| Metrik | Node.js | Rust |
| --- | ---: | ---: |
| CPU (% satu core) | 17.64 | 16.00 |
| RAM (MiB) | 130.45 | 71.98 |
| Total preview FPS | 13.34 | 13.63 |
| API p50 (ms) | 0.84 | 0.74 |
| API p95 (ms) | 1.98 | 4.12 |
| Error API | 0 | 0 |

Hanya selisih RAM (Rust ±45% lebih rendah) yang konsisten pada semua pengukuran. CPU, FPS dan
latensi API berada dalam derau: CPU Node bergeser 21% → 14,6% antar putaran, dan p95 API
berubah arah dibanding replay (satu batch Rust 6,6 ms). FPS dibatasi kamera dan relay cloud.
Image Docker: Node 1,04 GB, Rust 782 MB. Metrik: [results-live-20261015.json](tools/benchmark/results-live-20261015.json).
