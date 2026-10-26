# V380 CCTV — Rust

Server, protokol V380, recorder, storage, discovery, dan CLI ditulis dalam Rust. Frontend ditulis dalam Rust dan dikompilasi menjadi WebAssembly; `public/bootstrap.js` hanya memuat binding hasil `wasm-bindgen`. FFmpeg menangani video, rclone menangani penyimpanan cloud. Runtime Docker tidak membutuhkan Node.js atau Python.

## Jalankan dengan Docker

Dockerfile membangun binary Linux statis dan frontend WebAssembly dari Cargo.lock. Build pertama lebih lama pada CPU Celeron; build berikutnya memakai cache Cargo.

Konfigurasi Docker yang sudah ada tetap memakai `docker-data/config/config.json`, `docker-data/recordings`, `docker-data/rclone/rclone.conf`, `docker-data/compose.env`, dan overlay penyimpanan/perangkat yang sama.

```sh
docker compose --env-file docker-data/compose.env \
  -f compose.yaml \
  -f docker-data/storage.compose.json \
  -f docker-data/hardware.compose.json \
  up -d --build
```

Sebelumnya isi `ACCESS_PIN=123456` (6 digit angka, ganti dengan PIN Anda) di `docker-data/compose.env`; compose menolak berjalan tanpa nilai ini. `docker setup` membuat PIN acak untuk instalasi baru.

Viewer tersedia di `http://localhost:3381`. Image branch ini bernama `v380-viewer:rust`. Compose memakai project/service yang sama, sehingga `up` mengganti container viewer yang sedang berjalan. File konfigurasi dan rekaman dipertahankan.

Untuk instalasi baru atau untuk CLI:

```sh
cargo run -- docker setup
cargo run -- docker up -d --build
cargo run -- docker logs -f
cargo run -- docker down
```

CLI menyiapkan folder berizin privat, menyalin konfigurasi/rclone jika belum ada, dan membuat overlay `/dev/dri/renderD128` serta grup render jika perangkat tersedia. Konfigurasi yang sudah ada tidak ditimpa.

## Thumbnail dan deteksi gerakan

Default deteksi disesuaikan untuk Celeron N4020, RAM 8 GB, dan lima kamera 1080p. Resolusi rekaman dan pemakaian VAAPI yang sudah ada tetap mengikuti konfigurasi kamera. Rekaman H.264/H.265 memakai stream-copy; model pengenalan objek tetap berjalan di CPU.

Analisis dilakukan dua tahap:

1. FFmpeg hanya mendekode keyframe, memilih paling banyak 2 frame/detik, mengecilkan menjadi grayscale 256×256, dan memeriksa komponen area yang berubah. Noise terpisah, perubahan cahaya menyeluruh, serta area di bawah `MOTION_MIN_AREA` (default 0.002 dari gambar setelah padding, sekitar 131 piksel) disaring. Komponen harus minimal 6×6 piksel dan muncul berdekatan pada dua perbandingan berurutan (tiga keyframe).
2. Jika ada kandidat, YOLOv8n COCO memeriksa **satu gambar** dari momen kandidat terkuat. Hanya manusia, kendaraan, kucing, anjing, dan mamalia COCO yang lebih besar diterima; burung tidak termasuk. Bounding box objek harus beririsan dengan area bergerak. Jika tidak ada kandidat gerakan besar, model tidak dipanggil.

Proses FFmpeg analisis dan thread AI memakai prioritas CPU rendah (`nice 10`). Pemindaian dibatasi 4 detik dan 180 keyframe, pengambilan gambar AI 3 detik, tunggu AI 8 detik, serta thumbnail 3 detik. Hanya satu inferensi boleh berjalan; worker yang melampaui waktu tunggu tetap memegang slot sampai selesai sehingga tidak menumpuk thread AI. Timeout, keyframe tidak cukup, atau slot masih sibuk menghasilkan status **Belum dianalisis**, bukan skor nol.

Metode hemat ini memeriksa lebih sedikit momen daripada deteksi setiap frame: objek yang lewat di antara keyframe, objek jauh/kecil, atau objek di luar kandidat terkuat dapat terlewat. Ukuran piksel bukan ukuran fisik. Pengenalan dapat salah dan gerakan cahaya lokal dapat menyerupai gerakan objek. GOP kamera yang sangat panjang atau video all-I dapat mengurangi cakupan atau mencapai batas waktu. Ukur klip kamera asli di perangkat target sebelum menganggap seluruh klip akan teranalisis.

Upload tidak menunggu analisis. Antrean analisis menampung maksimal 5 klip menunggu + 1 diproses, dan maksimal 256 MiB data yang ditahan. Hard link sementara di `recordings/.analysis/` menjaga video tetap tersedia jika upload sudah menghapus file lokal aslinya; tidak menyalin isi video. Jika antrean penuh atau file terlalu besar, upload tetap lanjut, skor tetap kosong, dan daftar rekaman menampilkan **Belum dianalisis**. Klip tertahan yang belum selesai ketika shutdown dipulihkan saat startup; kelebihan batas pemulihan dilepas. Gangguan cloud tetap dapat membuat antrean upload/file asli bertambah; batas ini khusus antrean analisis.

`GET /api/cameras` menampilkan `recording.analysisPending`, `analysisSkipped`, dan `analysisFailed` (statistik global antrean selama proses hidup). Thumbnail 320 px dan skor mengikuti retensi. Skor versi detektor lama diabaikan; file lokal diajukan ulang saat pemulihan (tetap dibatasi kapasitas antrean), sedangkan klip yang hanya ada di cloud perlu diunduh untuk analisis ulang. Skor positif adalah confidence, `moving` memakai ambang 0.5.

Model sudah disertakan dalam image Docker. Untuk menjalankan tanpa Docker: `sh tools/fetch-motion-model.sh`. Override melalui `MOTION_MODEL_PATH`, `MOTION_CONFIDENCE` (0.5–1, default 0.5), dan `MOTION_MIN_AREA` (>0 sampai 1, default 0.002; dipakai pada filter area perubahan dan bounding box AI). Model YOLOv8 dari Ultralytics menggunakan lisensi AGPL-3.0: https://github.com/ultralytics/ultralytics.

Benchmark klip asli dengan build release: `cargo run --release --example motion_benchmark -- /path/to/clip.mp4 2`. Untuk lima segmen 60 detik per menit, sasaran analisis rata-rata di bawah 12 detik/klip, dengan ruang CPU untuk rekaman, preview, dan upload. Hasil pada Xeon/simulasi kernel tanpa AVX bukan sertifikasi performa N4020. Lihat [hasil dan batas benchmark profil N4020](docs/n4020-performance.md).

## Akses dengan PIN

Semua halaman, API, stream, dan rekaman hanya bisa diakses setelah memasukkan PIN 6 digit pada `/login`. PIN dibaca dari environment `ACCESS_PIN`; server menolak start jika kosong atau bukan 6 digit. Sesi berakhir setelah 5 menit tanpa aktivitas pengguna, lalu halaman diarahkan kembali ke login. Klik, gerakan pointer, keyboard, sentuhan, dan scroll memperpanjang sesi melalui `POST /api/session` (dibatasi sekitar sekali per detik). Polling otomatis, pemeriksaan sesi, dan video/audio yang terus berjalan tidak memperpanjang sesi. Cookie juga diperbarui pada aktivitas sehingga login aktif tidak terputus setelah 5 menit. Server juga memutus video/audio/download yang masih terbuka pada batas waktu idle; aktivitas memperpanjang koneksi tersebut tanpa harus menyambung ulang; frontend menghentikan playback/audio/talk, termasuk ketika tab kembali aktif. Rekaman kamera di server tetap berjalan. Lima PIN salah berturut-turut mengunci login selama 60 detik. Sesi disimpan di memori, jadi restart server mengeluarkan semua sesi. Layani lewat HTTPS (reverse proxy) bila diakses dari luar jaringan rumah, karena PIN dikirim apa adanya.

## Pengembangan lokal

Gunakan Rust 1.99, FFmpeg, dan rclone jika memakai penyimpanan cloud. Frontend menggunakan `wasm-bindgen-cli` 0.2.105, sama dengan versi crate yang dikunci.

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.105 --locked
cargo build --release -p cctv-ui --target wasm32-unknown-unknown
wasm-bindgen --target web --out-dir public/pkg \
  target/wasm32-unknown-unknown/release/cctv_ui.wasm
cp config.example.json config.json
# Isi password kamera dan tujuan rekaman pada config.json.
ACCESS_PIN=123456 cargo run -- serve
```

Server lokal mendengarkan `127.0.0.1:3380`. `CONFIG_PATH`, `HOST`, `PORT`, `PUBLIC_PORT`, `PUBLIC_DIR`, `FFMPEG_PATH`, `RCLONE_PATH`, dan `RCLONE_CONFIG` dapat diatur melalui environment.

## Fitur dan konfigurasi

- V380 LAN, cloud V380, dan RTSP; pemilihan HD/SD menggunakan stream kamera.
- Live view, snapshot JPEG, fullscreen, pengaturan kamera, reconnect, PTZ dengan stop otomatis, audio G.711 A-law, dan push-to-talk eksperimental.
- Playback MP4 dari lokal, Google Drive, atau OneDrive; filter kamera/tanggal/jam, pagination, HTTP byte ranges, dan pemutaran klip berikutnya.
- Rekaman H.264/HEVC memakai stream copy tanpa encoding video ulang. Audio V380 dimux ke AAC 32 kbit/s.
- Kamera MJPEG dikonversi ke H.264 pada resolusi asli. Encoder otomatis menguji Intel VA-API dan memakai CPU jika GPU tidak tersedia atau gagal. CPU memakai `ultrafast`, CRF 22, satu thread; GPU memakai QP 22. Skala CRF dan QP berbeda.
- Preview H.264/HEVC hanya didekode saat ada penonton/snapshot, maksimal 640 piksel dan default 5 fps. Preview MJPEG memakai JPEG kamera langsung. Membuka playback melepas stream preview live.
- Koneksi kamera, preview, dan recorder ditangani terpisah. Antrean input memiliki batas byte; recorder yang penuh menutup segmen dan pulih pada keyframe berikutnya. Shutdown menutup input, memfinalisasi segmen, membatasi waktu tunggu, dan mereap proses FFmpeg.
- File MP4 dibuka di folder `.partial-*` yang terpisah. Hanya file dengan atom MP4 lengkap yang masuk antrean upload. Upload gagal diulang; file lokal dipertahankan sampai berhasil. File yang belum lengkap dipertahankan untuk pemulihan.

Lihat `config.example.json`. Field per kamera: `name`, `ip`, `deviceId`, `username`, `password`, `v380Port` (default 8800), `mode` (`v380`/`cloud`/`rtsp`), `quality` (`hd`/`sd`), `rtspUrl`, `record`, `audio`, `previewFps` (1–10), `recordingEncoder` (`auto`/`cpu`), `recordingCrf` (0–51), dan `recordingFps` (1–60).

MJPEG memakai estimasi frame rate dari kedatangan frame selama tiga detik pertama, timestamp waktu kedatangan, dan output VFR. Jitter jaringan tetap dapat memengaruhi timing. Rekaman tidak menambah detail yang tidak tersedia pada sumber kamera.

Blok `recording`: `enabled`, `destination` (`local`/`gdrive`/`onedrive`), `dir`, `segmentSeconds` (default 60), `audio`, `keepLocal`, serta `rclone.gdrive`/`rclone.onedrive`. Keyframe menentukan batas segmen; durasinya dapat sedikit melewati interval. Rclone membaca konfigurasi autentikasinya sendiri.

Cloud V380 memakai dispatch `http://dispa1.av380.net:8001/api/v1/get_stream_server` dan relay TCP port 8800. Mode ini dipilih secara eksplisit; kegagalan LAN tidak otomatis memindahkan kredensial ke cloud. Dukungan H.265/communication version 21 mengikuti protokol sebelumnya dan memerlukan validasi pada perangkat yang memakainya.

## Retensi dan diagnostik

`recording.retentionDays` (default 0, nonaktif) dan `retentionCheckSeconds` (default 3600) dapat dioverride oleh `RECORDING_RETENTION_DAYS` dan `RECORDING_RETENTION_CHECK_SECONDS`. Environment kosong mempertahankan konfigurasi; nilai 0 mematikan retensi.

Retensi hanya menghapus rekaman dengan nama/tanggal valid dari kamera terkonfigurasi. Folder aktif, symlink, file rusak, file yang masih diunggah, dan file lokal yang belum ditemukan di cloud dipertahankan. Penghapusan cloud hanya menargetkan satu file di folder tujuan, tanpa rekursi.

Endpoint tetap kompatibel:

| Endpoint | Fungsi |
| --- | --- |
| `GET /api/cameras` | Status kamera, `hasPassword`, recorder, preview, reconnect, statistik paket |
| `POST /api/cameras/{id}` | Simpan konfigurasi dan reconnect; password kosong dipertahankan |
| `POST /api/cameras/{id}/reconnect` | Reconnect manual |
| `POST /api/cameras/{id}/ptz` | `{ "direction": "up/down/left/right/stop" }` |
| `POST /api/cameras/{id}/talk` | G.711 A-law, 1–480 byte, `application/octet-stream` |
| `GET /stream/{id}` | Multipart MJPEG |
| `GET /audio/{id}` | Stream G.711 A-law |
| `GET /snapshot/{id}` | JPEG baru, timeout 10 detik |
| `GET /api/recordings` | Filter `date`, `camera`, `from`, `to`, `page`, `size` |
| `GET /recordings/{name}` | MP4 dengan byte ranges |
| `GET /api/retention` | Status retensi |

Password dan URL RTSP berkredensial tidak dikembalikan oleh API. Stderr FFmpeg tidak dipublikasikan karena dapat berisi kredensial. Konfigurasi ditulis atomik dengan izin privat. Request POST lintas origin ditolak. Viewer belum menyediakan login pengguna; gunakan akses jaringan yang sesuai untuk viewer pribadi.

## Discovery LAN

```sh
cargo run -- discover 192.168.0.0/24
```

Discovery memakai UDP V380 dan scan TCP port 8800/554, maksimum 32 koneksi sekaligus, hanya pada subnet IPv4 privat `/24` atau lebih kecil yang disebutkan.

## Pengujian

```sh
cargo test -- --test-threads=1
cargo test -p cctv-ui
cargo check -p cctv-ui --target wasm32-unknown-unknown
cargo fmt --all -- --check
```

Tes mencakup cuplikan terenkripsi kamera, autentikasi, fragmentasi TCP, pembatalan, JPEG/HEVC, PTZ/talkback, batas buffer, finalisasi MP4, AAC, durasi MJPEG, keamanan API, byte ranges, retensi, dan pemulihan upload. Tes video memakai FFmpeg; hasil pengujian protokol memakai server TCP lokal, bukan kredensial kamera sebenarnya.

Untuk smoke test browser dengan kamera sintetis dan MP4 lokal:

```sh
cargo run --example browser_fixture
# http://127.0.0.1:3390
```

### Diagnostik analisis dan healthcheck

Healthcheck Docker memakai `GET /healthz` (HTTP 204), yang dapat diakses tanpa sesi dan tidak mengembalikan data kamera. API kamera dan rekaman tetap memerlukan login.

Pemindaian mencoba keyframe terlebih dahulu. Jika kurang dari tiga sampel, decoder mencoba frame biasa dengan interval minimal 0,5 detik dalam anggaran total empat detik yang sama. Klip yang tetap terlalu pendek mendapatkan thumbnail jika frame tersedia, tetapi status gerak tetap **Belum dianalisis**. Kegagalan decoder mencatat exit status dan maksimal 4 KiB bagian akhir stderr FFmpeg; batas waktu dan jumlah frame tetap berlaku.
