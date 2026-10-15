# Membandingkan Docker Node dan Rust

Jalankan dari root repository. Persyaratan: Docker aktif, Python >= 3.12, FFmpeg host,
branch `origin/rust`, dan config kamera lokal. File private disimpan di `.benchmark/`
yang diabaikan Git. Setup mempertahankan config benchmark yang sudah ada.

```sh
python3 tools/benchmark/setup.py
docker compose --env-file .benchmark/compose.env -f compose.benchmark.yaml up -d --build node rust
```

Node: http://localhost:3381 ; Rust: http://localhost:3382.
Setiap container dibatasi 2 CPU dan RAM 1 GiB. Config masing-masing ada di
`.benchmark/config-node/config.json` dan `.benchmark/config-rust/config.json`.
Keduanya mematikan recording, upload dan retensi; preview 5 fps.
Config produksi tidak dipakai langsung oleh container benchmark.

Untuk benchmark replay, hentikan kedua viewer sebelum mengganti config:

```sh
docker compose --env-file .benchmark/compose.env -f compose.benchmark.yaml stop node rust
cp .benchmark/controlled.json .benchmark/config-node/config.json
cp .benchmark/controlled.json .benchmark/config-rust/config.json
docker compose --env-file .benchmark/compose.env -f compose.benchmark.yaml --profile fixture up -d node rust fixture
python3 tools/benchmark/run.py --label controlled --seconds 20 --rounds 3
```

Fixture mengirim 3 stream H.264 1280×720 dan 1 JPEG 640×480 pada 16 fps,
melalui protokol media V380 terenkripsi. Ketiga H.264 mengulang video uji delapan
detik; JPEG mengulang gambar yang sama. Benchmark membuka satu klien MJPEG per
kamera per versi. CPU mencakup FFmpeg di dalam container. RAM adalah working set
(usage minus inactive file cache). Latensi API memakai keep-alive pada target
100 request/detik, dua urutan AB/BA, bukan pengukuran throughput maksimum.

Kembalikan kamera asli setelah selesai:

```sh
docker compose --env-file .benchmark/compose.env -f compose.benchmark.yaml stop node rust fixture
cp .benchmark/live-node.json .benchmark/config-node/config.json
cp .benchmark/live-rust.json .benchmark/config-rust/config.json
docker compose --env-file .benchmark/compose.env -f compose.benchmark.yaml up -d node rust
```

`run.py` juga dapat dijalankan untuk kamera asli dengan label lain. Ia hanya
menguji kamera yang statusnya live pada kedua versi; periksa jumlah kamera dan
error di JSON hasil sebelum membandingkan. Jaringan cloud dan pembatasan sesi
kamera bisa membuat uji live tidak sebanding.

Hasil JSON dan log tersedia di `.benchmark/results/`. Jangan commit config kamera.
Dockerfile produksi tidak diubah. Setup menyesuaikan salinan Dockerfile untuk
ARM64 (target Rust, binary wasm-bindgen beserta checksum, dan paket GPU Intel).
Subnet fixture `172.29.199.0/24` dapat diubah di compose, setup dan camera config
jika bertabrakan dengan jaringan lokal.
