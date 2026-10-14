# V380 Local Viewer

Viewer CCTV V380 di browser. Server Node.js login ke kamera melalui TCP 8800 (LAN atau relay cloud V380), merakit fragment video, mendekode AES-128-ECB berdasarkan tiket sesi, dan mengubah H.264/H.265 serta frame JPEG menjadi MJPEG melalui FFmpeg. Mode LAN tidak membutuhkan aplikasi Apple, akun cloud, atau perubahan firmware.

## Jalankan

Butuh Node.js 18+ dan FFmpeg di PATH. Tidak ada dependency npm untuk dipasang.

```sh
# Hanya bila config.json belum ada:
cp config.example.json config.json
# Isi password kamera di config.json, lalu:
npm start
```

Buka **http://localhost:3380**. Untuk mode LAN, Mac dan kamera harus berada di jaringan yang saling terhubung. Server hanya mendengarkan di 127.0.0.1; alamat lokal ini juga digunakan saat kamera diakses melalui relay cloud. Gunakan `PORT=3381 npm start` jika port utama terpakai. Untuk lokasi FFmpeg lain, gunakan `FFMPEG_PATH=/path/to/ffmpeg npm start`.

Fitur: video live 10 fps, status koneksi, snapshot JPEG, layar penuh, pengaturan kamera, kualitas HD/SD, reconnect otomatis untuk gangguan jaringan, dan dukungan RTSP/bridge. Password didukung hingga 47 byte. Password yang kosong pada formulir mempertahankan nilai tersimpan; password tidak dikembalikan melalui API. Login yang ditolak menunggu pengaturan diperbaiki atau reconnect manual, agar tidak mencoba password berulang kali.

Kamera tambahan dapat dimasukkan ke array `cameras` di `config.json`, lalu restart server. File ini berisi kredensial lokal dan diabaikan Git. Untuk mode RTSP, isi `mode: "rtsp"` serta `rtspUrl` yang lengkap. Encode karakter khusus dalam username/password URL, misalnya `#` menjadi `%23`.

## Verifikasi

```sh
npm test
curl http://localhost:3380/api/cameras
curl http://localhost:3380/snapshot/0 -o /tmp/snapshot.jpg
```

Tests mencakup cuplikan terenkripsi dari kamera, fragmentasi TCP, respons kontrol panjang, password ditolak, penghentian koneksi, password yang melewati satu blok AES, dan frame JPEG. Tiga kamera telah diuji dengan device version 31, communication version 20, H.264 1280×720; kamera Kitchen memakai JPEG 640×480. Varian H.265 dan communication version 21 diimplementasikan tetapi belum diverifikasi pada hardware lain. PTZ dan playback SD card belum disediakan; audio hanya direkam (lihat bagian rekam), belum diputar di live view.

Implementasi protokol menggunakan referensi [V380Decoder](https://github.com/PyanSofyan/V380Decoder), dengan implementasi Node.js di `lib/v380.js`. Kunci video memakai tiket login 32 bit dan konstanta media; video didekode 64 byte setiap 80 byte. Jadi kunci harus diperbarui pada tiap login.

## Discovery jaringan lokal

Untuk menemukan V380 dan membaca device ID tanpa login:

```sh
python3 tools/discover.py 192.168.0.0/24 192.168.18.0/24
```

Pemindaian memeriksa TCP 8800/554 dan mengirim discovery V380 UDP 10008. Maksimal 32 pemeriksaan berjalan bersamaan, hanya pada subnet IPv4 privat /24 atau lebih kecil. Discovery UDP lintas router dapat gagal meskipun koneksi TCP kamera bekerja; dalam kasus itu device ID perlu diambil dari aplikasi V380.

## Akses dari luar rumah

Pilih **Cloud V380 · luar rumah** pada pengaturan kamera untuk menggunakan device ID, username, dan password kamera yang tersimpan. Mode `cloud` meminta alamat relay ke layanan dispatch V380 dan membuka koneksi TCP 8800 ke relay tersebut. IP rumah disimpan agar mudah kembali ke mode LAN, tetapi tidak dipakai saat mode cloud aktif. Mode ini tidak memerlukan port forwarding pada router rumah.

Permintaan discovery relay menggunakan `http://dispa1.av380.net:8001/api/v1/get_stream_server` dan berisi device ID, platform, timestamp, serta signature. Kredensial kamera dikirim dalam format autentikasi protokol V380 ke relay yang dipilih. Video dalam mode ini melewati infrastruktur V380; tidak terbatas pada jaringan rumah.

Status validasi (14 Oktober 2026): keempat kamera sudah berhasil login dan streaming melalui relay internet dari jaringan di luar rumah. Snapshot dan MJPEG masing-masing kamera sudah diverifikasi: RuangTamu, Teras1, dan Teras2 pada 1280×720; Kitchen pada 640×480. Konfigurasi saat ini memakai mode `cloud`. Pilih V380 · jaringan rumah untuk kembali memakai alamat IP lokal saat berada di rumah. Resolusi mode cloud dibaca dari gambar hasil decoder karena respons relay memiliki format berbeda dari respons LAN. Pencarian relay, fallback server, pembatalan, autentikasi, tiket sesi, dan format video juga diuji melalui 14 tes lokal.

## Rekam per menit, upload, dan playback (rclone)

Tambahkan blok `recording` di `config.json` (lihat `config.example.json`). Server memotong stream setiap `segmentSeconds` (default 60) menjadi file MP4 `cam<deviceId>_YYYYMMDD_HHMMSS.mp4` di folder `recordings/` tanpa re-encode (`-c copy`, CPU hampir nol), mengunggahnya dengan **rclone**, lalu menghapus file lokal setelah berhasil (`keepLocal: true` untuk menyimpannya). Pemotongan terjadi pada keyframe berikutnya, jadi durasi bisa sedikit lebih dari 60 detik. Upload yang gagal diulang dengan backoff; file sisa dari proses sebelumnya diunggah saat server start. Kamera tertentu dapat dikecualikan dengan `"record": false`. Status (`recording.segments/uploaded/pending/error`) tampil di `/api/cameras`.

### Tujuan: `destination`

```json
"recording": {"enabled": true, "segmentSeconds": 60, "destination": "gdrive",
  "rclone": {"gdrive": "gdrive:cctv", "onedrive": "onedrive:cctv"}}
```

- `"gdrive"` / `"onedrive"`: memakai remote rclone yang tertulis di `rclone.gdrive` / `rclone.onedrive` (format `remote:folder`). Hanya yang dipilih yang dipakai. Isi bebas, termasuk path folder lokal, mis. folder OneDrive yang disinkronkan aplikasinya.
- `"local"`: hanya simpan di `recordings/`.
- `rclone.bin` (atau env `RCLONE_PATH`) mengatur lokasi biner bila tidak ada di PATH.

Pasang dan siapkan rclone sekali: `brew install rclone`, lalu `rclone config` untuk membuat remote (`gdrive` bertipe Google Drive, `onedrive` bertipe Microsoft OneDrive). Autentikasi dan token dikelola rclone, bukan aplikasi ini. Cek dengan `rclone lsd gdrive:`.

Playback (menu **Rekaman** di sidebar) membaca dari tujuan yang sama: daftar lewat `rclone lsjson`, video lewat `rclone cat --offset/--count` sehingga seek (HTTP Range) tetap bekerja tanpa mengunduh seluruh file. Pilih kamera, tanggal, dan rentang jam; daftar diurutkan terbaru dulu dengan paging 50 rekaman. Segmen berikutnya (lebih baru) diputar otomatis. API: `GET /api/recordings?camera=<id|all>&date=YYYY-MM-DD&from=HH:MM&to=HH:MM&page=1&size=50` dan `GET /recordings/<cam…_YYYYMMDD_HHMMSS.mp4>`. Kamera MJPEG (mis. Kitchen) direkam sebagai H.264 agar dapat diputar di browser.

### Audio

Rekaman menyertakan audio bila kamera mengirimnya (G.711 A-law 8 kHz mono, didekripsi dengan kunci sesi yang sama, lalu di-encode AAC 32 kbps). Kamera tanpa mikrofon direkam tanpa track audio. Matikan dengan `recording.audio: false` atau `"audio": false` pada kamera tertentu. Live view di browser tetap tanpa suara.

### Kontrol kamera

- **PTZ:** tombol panah di pojok kanan bawah video (tahan untuk bergerak); `POST /api/cameras/<id>/ptz` dengan `{"direction":"up|down|left|right|stop"}`. Server mengirim stop otomatis bila tidak ada perintah baru selama 1,5 detik.
- **Suara live:** tombol 🔇/🔊 pada kartu kamera yang mengirim audio.
- **Push to talk (eksperimental, belum terverifikasi):** tahan tombol 🎤 untuk mengirim suara mikrofon ke kamera (G.711 A-law 8 kHz, paket 60 ms ke `POST /api/cameras/<id>/talk`). Protokol talkback V380 tidak terdokumentasi; formatnya meniru frame audio yang dikirim kamera dan belum dikonfirmasi pada hardware. Bila kamera tidak bersuara, format paket di `V380Client.talk` perlu disesuaikan dengan tangkapan trafik app V380.

## Docker

Image menyertakan Node.js 22, FFmpeg (termasuk libx264), rclone, Python untuk discovery, dan zona waktu Asia/Jakarta. Jalankan Docker Desktop dahulu, kemudian dari folder proyek:

```sh
npm run docker:up
```

Buka **http://localhost:3381** pada host atau **http://<IP-host>:3381** dari perangkat lain. Port Docker menerima koneksi pada semua alamat IPv4 host (`0.0.0.0`). Perintah ini menyiapkan data Docker pada pemakaian pertama dan menjalankan Compose dengan port yang terpisah dari `npm start` di 3380.

Konfigurasi Docker disalin sekali dari `config.json` ke `docker-data/config/config.json`. Ubah salinan ini untuk mengatur kamera atau recording di Docker; menjalankan ulang tidak menimpa perubahan. Nilai `recording.enabled` mengikuti konfigurasi sumber, termasuk jika recording sedang dinonaktifkan. Folder konfigurasi dipasang sebagai volume direktori agar penyimpanan pengaturan melalui UI (atomic rename) tetap bekerja.

Data persisten:

- `docker-data/config/`: konfigurasi kamera khusus Docker.
- `docker-data/recordings/`: rekaman dan antrean upload Docker.
- `docker-data/rclone/`: konfigurasi/token rclone Docker; helper menyalin konfigurasi rclone host jika sudah tersedia.
- `docker-data/storage.compose.json`: mount tambahan untuk folder sinkronisasi lokal, misalnya OneDrive. Helper memetakan path host ke path container tanpa mengubah konfigurasi host. Folder tersebut harus tetap tersedia di mesin yang menjalankan Docker.

`docker-data/`, konfigurasi rahasia, dan rekaman tidak masuk Git atau image Docker. UID/GID container mengikuti pengguna yang menjalankan helper agar volume dapat ditulis. Rclone memakai konfigurasi yang writable agar refresh token tetap tersimpan. Jika remote rclone belum dibuat:

```sh
node tools/docker.js run --rm viewer rclone config
```

Untuk port lain, ubah `VIEWER_PORT` di `docker-data/compose.env`. Menjalankan Docker di komputer lain memerlukan konfigurasi kamera, remote rclone, dan folder storage yang sesuai untuk komputer itu. Cloud V380 memakai koneksi keluar dari container; mode LAN memerlukan jaringan Docker yang dapat menjangkau IP kamera. Discovery UDP broadcast dari Docker Desktop dapat tidak menyeberang ke LAN; jalankan `tools/discover.py` pada host bila perlu.

```sh
npm run docker:logs
node tools/docker.js ps
node tools/docker.js exec viewer npm test
npm run docker:down
```

`docker:down` menghentikan container dan mempertahankan data pada folder host. Untuk membangun ulang setelah perubahan kode, jalankan `npm run docker:up` lagi. Pengaturan server `HOST`, `PORT`, `PUBLIC_PORT`, dan `CONFIG_PATH` dipakai oleh Compose; `npm start` tanpa variabel ini tetap memakai konfigurasi dan alamat localhost semula.

### Google Drive dengan rclone di Docker

Compose tersedia di `compose.yaml`. Persiapkan volume sekali dengan `node tools/docker.js setup`, kemudian jalankan `npm run docker:up`. Helper juga memasukkan `docker-data/compose.env` (UID/GID dan port) serta `docker-data/storage.compose.json` (mount storage tambahan). Bentuk perintah Compose langsung yang setara:

```sh
docker compose --env-file docker-data/compose.env \
  -f compose.yaml -f docker-data/storage.compose.json up -d --build
```

Cara paling mudah menghubungkan Google Drive adalah melakukan OAuth di Mac, dengan file konfigurasi yang langsung digunakan container:

```sh
rclone --config ./docker-data/rclone/rclone.conf config
```

Pilih `n` (remote baru), beri nama `gdrive`, dan pilih storage `drive` (Google Drive). Gunakan OAuth client ID dan client secret milik sendiri: buat sesuai [panduan rclone](https://rclone.org/drive/#making-your-own-client-id), aktifkan Google Drive API, dan pilih tipe aplikasi Desktop. Dokumentasi rclone saat ini merekomendasikan client ID sendiri karena client bersama akan dihentikan selama 2026.

Untuk scope, pilih `drive.file` (biasanya pilihan 3): rclone dapat upload dan playback file/folder yang dibuatnya. Jika perlu mengakses folder/file yang sebelumnya dibuat di luar rclone, pilih scope `drive` (biasanya pilihan 1). Kosongkan service-account file; pilih tidak untuk advanced config, ya untuk autentikasi browser, dan tidak untuk Shared Drive bila memakai My Drive pribadi. Login ke akun Google tujuan dan simpan remote. Masukkan client secret serta token hanya ke prompt rclone, bukan ke `config.json` kamera.

Verifikasi dari container dan buat folder tujuan:

```sh
node tools/docker.js exec viewer rclone listremotes
node tools/docker.js exec viewer rclone mkdir gdrive:v380-recordings
node tools/docker.js exec viewer rclone lsf gdrive:v380-recordings
```

Edit **hanya blok `recording`** di `docker-data/config/config.json` seperti di bawah. Pertahankan array `cameras` yang sudah berisi kredensial kamera Anda. `config.gdrive.example.json` adalah contoh lengkap dengan placeholder, bukan pengganti konfigurasi kamera yang sudah berjalan.

```json
"recording": {
  "enabled": true,
  "segmentSeconds": 60,
  "dir": "/app/recordings",
  "keepLocal": false,
  "destination": "gdrive",
  "rclone": {"gdrive": "gdrive:v380-recordings"}
}
```

Kemudian restart agar pengaturan recording dimuat:

```sh
node tools/docker.js restart viewer
npm run docker:logs
```

Rekaman diunggah setelah tiap segmen selesai; status antrean ada di `/api/cameras`. Token tersimpan persisten di `docker-data/rclone/rclone.conf` dan rclone memperbaruinya otomatis. `keepLocal: false` menghapus segmen lokal setelah upload berhasil. Selama gagal upload, segmen tetap di disk dan dicoba lagi.

Bila melakukan config dari container tanpa browser, jawab tidak pada autentikasi otomatis lalu ikuti [remote setup rclone](https://rclone.org/remote_setup/). Jalankan `rclone authorize` pada komputer dengan browser memakai client OAuth yang sama, dan masukkan hasilnya ke prompt container. Config pada host dengan path volume di atas menghindari langkah manual ini.

Jika muncul `didn't find section in config file` untuk `gdrive:cctv`, cocokkan nama remote dengan hasil:

```sh
rclone --config ./docker-data/rclone/rclone.conf listremotes
```

Bila remote Google Drive bernama `titus`, gunakan `titus:cctv` pada `recording.rclone.gdrive`, atau tambahkan alias pada file rclone yang sama:

```ini
[gdrive]
type = alias
remote = titus:
```

Container memakai `docker-data/rclone/rclone.conf` melalui `RCLONE_CONFIG`. Untuk menjalankan viewer langsung di host memakai file yang sama:

```sh
RCLONE_CONFIG="$PWD/docker-data/rclone/rclone.conf" npm start
```


### Integritas rekaman MP4

File yang masih direkam disimpan dalam direktori `.partial-*` terpisah untuk setiap proses FFmpeg. Segmen dipindahkan ke folder rekaman utama dan masuk antrean upload setelah penutupan serta pemeriksaan struktur MP4 (`moov` dan `mdat`) berhasil. File tidak lengkap yang tersisa setelah crash/restart disimpan untuk diagnosis dan tidak otomatis diupload. Server menunggu finalisasi recorder saat shutdown; metadata MP4 ditempatkan di awal file dengan `faststart` untuk playback.

Jika file lama menunjukkan `moov atom not found`, file itu belum selesai atau terpotong. Perbaikan recorder berlaku pada rekaman berikutnya; file lama tidak otomatis dapat dipulihkan. Pemulihan payload tanpa metadata dapat kehilangan audio, sebagian frame, dan informasi waktu, sehingga hasilnya harus disimpan sebagai salinan terpisah.

### Retensi rekaman

Retensi berdasarkan waktu mulai rekaman pada nama `cam<deviceId>_YYYYMMDD_HHMMSS.mp4`, mengikuti zona waktu recorder. Retensi default **nonaktif** (`0`); nilai positif adalah jumlah hari yang dipertahankan, misalnya 7 hari:

```json
"recording": {
  "enabled": true,
  "destination": "gdrive",
  "dir": "/app/recordings",
  "rclone": {"gdrive": "gdrive:cctv"},
  "retentionDays": 7,
  "retentionCheckSeconds": 3600
}
```

Tambahkan dua properti retensi pada blok recording yang sudah ada tanpa mengganti konfigurasi kamera. Cleanup berlaku pada tujuan yang dipilih (`local`, Google Drive, OneDrive, atau folder sinkronisasi rclone) dan salinan lokal yang sudah terunggah. Cleanup tetap dapat berjalan saat recording dinonaktifkan jika retensi masih diaktifkan.

Environment variable mengalahkan config; nilai kosong memakai config, sedangkan `0` secara eksplisit mematikan retensi:

```sh
RECORDING_RETENTION_DAYS=7 RECORDING_RETENTION_CHECK_SECONDS=3600 npm start
```

Untuk Docker, tambahkan ke `docker-data/compose.env`:

```dotenv
RECORDING_RETENTION_DAYS=7
RECORDING_RETENTION_CHECK_SECONDS=3600
```

Lalu jalankan `npm run docker:up` untuk memuat environment baru. Jika hanya mengubah `docker-data/config/config.json`, gunakan `node tools/docker.js restart viewer`. Cleanup pertama berjalan saat startup, lalu setiap interval (default satu jam). Perubahan nilai dapat mulai menghapus rekaman lama pada startup berikutnya.

Hanya file MP4 di folder tujuan utama dengan device ID kamera yang terdaftar dan timestamp valid yang diproses. File lebih tua dari batas umur dihapus satu per satu melalui `rclone deletefile`; aplikasi tidak memanggil purge atau membersihkan trash Google Drive. Folder lain, file nonrekaman, salinan recovery, symlink lokal, MP4 yang belum lengkap, dan upload yang masih antre tetap dipertahankan. Salinan lokal tujuan cloud yang belum ditemukan di cloud juga dipertahankan agar rekaman belum terunggah tidak hilang. Periksa hasil, jumlah file yang dihapus, dan error melalui **GET `/api/retention`**.
