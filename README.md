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

Setiap segmen yang selesai direkam dianalisis sebelum diunggah. Backend Rust menjalankan YOLOv8n COCO ONNX melalui tract (tanpa Python), mengambil 2 frame/detik pada input 640×640. Hanya manusia, kendaraan, kucing, anjing, dan mamalia COCO yang lebih besar dipertimbangkan; burung/serangga tidak termasuk. Deteksi harus cocok pada tiga frame berturut-turut, dengan perpindahan pusat minimal 5% sisi terpendek bounding box (batas bawah 3 piksel) pada masing-masing pasangan frame. Ini menekan pemicu satu frame dan objek diam, tetapi gerakan sangat pelan atau hanya anggota tubuh dapat terlewat. Skor adalah confidence objek bergerak, bukan scene-change; `moving` memakai ambang 0.5.

Model sudah disertakan dalam image Docker. Untuk menjalankan tanpa Docker, jalankan `sh tools/fetch-motion-model.sh` dari root repo. Override melalui `MOTION_MODEL_PATH`, `MOTION_CONFIDENCE` (default 0.5; 0.5 sampai 1), dan `MOTION_MIN_AREA` (default 0.002; luas bounding box relatif terhadap input persegi setelah padding). Ukuran piksel bukan ukuran fisik: kucing jauh bisa terlewat, dan salah klasifikasi serangga tetap mungkin. Model YOLOv8 dari Ultralytics menggunakan lisensi AGPL-3.0; lihat https://github.com/ultralytics/ultralytics dan ketentuan lisensinya.

Thumbnail 320 px diambil dari momen gerakan terkonfirmasi dengan confidence tertinggi; jika tidak ada, dari tengah segmen. Skor dan versi detektor disimpan di `recordings/.index.json`; skor scene-change lama diabaikan. Klip lama dianalisis ulang ketika kembali masuk antrean dan file lokal masih ada; klip yang hanya tersimpan di cloud perlu diunduh/diproses ulang untuk memperoleh skor baru. Kegagalan model/decode dicatat ke stderr dan tidak diberi label seolah-olah sudah dianalisis. Thumbnail dan indeks tetap mengikuti retensi. Analisis CPU lebih berat daripada metode keyframe lama; pantau antrean upload pada perangkat kecil.

## Akses dengan PIN

Semua halaman, API, stream, dan rekaman hanya bisa diakses setelah memasukkan PIN 6 digit pada `/login`. PIN dibaca dari environment `ACCESS_PIN`; server menolak start jika kosong atau bukan 6 digit. Sesi berlaku tepat 5 menit sejak login (tidak diperpanjang oleh aktivitas), setelah itu halaman diarahkan kembali ke login. Server juga memutus video/audio/download yang masih terbuka pada deadline sesi; frontend menghentikan playback/audio/talk, termasuk ketika tab kembali aktif. Rekaman kamera di server tetap berjalan. Lima PIN salah berturut-turut mengunci login selama 60 detik. Sesi disimpan di memori, jadi restart server mengeluarkan semua sesi. Layani lewat HTTPS (reverse proxy) bila diakses dari luar jaringan rumah, karena PIN dikirim apa adanya.

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
