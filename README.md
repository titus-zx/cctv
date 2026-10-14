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

Tests mencakup cuplikan terenkripsi dari kamera, fragmentasi TCP, respons kontrol panjang, password ditolak, penghentian koneksi, password yang melewati satu blok AES, dan frame JPEG. Tiga kamera telah diuji dengan device version 31, communication version 20, H.264 1280×720; kamera Kitchen memakai JPEG 640×480. Varian H.265 dan communication version 21 diimplementasikan tetapi belum diverifikasi pada hardware lain. Audio, PTZ, playback SD card, dan recording belum disediakan.

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
