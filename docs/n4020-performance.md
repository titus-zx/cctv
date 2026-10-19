# Profil N4020 untuk lima kamera 1080p

Profil default ini mengutamakan rekaman dan upload pada Intel Celeron N4020 (2 core, tanpa AVX/FMA), RAM 8 GB. Pada gambar perangkat pengguna tersedia sekitar 3,6 GiB RAM; penggunaan CPU saat itu sekitar 23%. Angka satu tangkapan layar bukan pengukuran beban puncak.

## Perubahan

- Keyframe dipindai sebagai grayscale 256×256 dengan filter ukuran komponen dan konfirmasi dua perubahan berurutan.
- Maksimal satu gambar kandidat terkuat diverifikasi model YOLOv8n per klip. Klip tenang tidak memanggil model. Objek terdeteksi harus beririsan dengan area bergerak.
- Decoder analisis, thumbnail, dan thread inferensi memiliki prioritas rendah. Slot AI tunggal tetap ditahan worker yang belum selesai meskipun pemanggil sudah timeout.
- Upload memakai antrean terpisah. Analisis hanya menahan maksimal lima klip menunggu + satu diproses, total maksimal 256 MiB, menggunakan hard link tanpa menyalin isi.
- Klip yang melewati anggaran, tidak memiliki cukup keyframe, atau dilewati karena antrean penuh tetap berstatus **Belum dianalisis**. File yang masih tertahan ketika proses berhenti dipulihkan saat startup.
- Resolusi rekaman, stream-copy H.264/H.265, VAAPI yang sudah dikonfigurasi, serta sesi idle lima menit tetap dipertahankan. URL thumbnail memakai versi detektor agar cache gambar lama tidak dipakai setelah perubahan algoritme.

## Benchmark release

Pengujian dilakukan pada Xeon Platinum 8573C dalam container dengan batas RAM 8 GiB dan kuota dua CPU, **bukan pada N4020 fisik**. Analisis dibatasi satu core dengan affinity, probe gabungan dua core. Versi tract 0.22.5 pada driver pengukuran memakai salinan terpisah dengan pilihan `generic()` untuk menonaktifkan kernel AVX/FMA. Ini menguji jalur komputasi yang tersedia pada N4020, tetapi IPC, cache, dan frekuensi tetap milik Xeon. Repo/dependensi produksi tidak memakai patch pengukuran itu.

Klip sintetis H.264 berukuran 1920×1080, 15 fps, durasi 60 detik. Klip aktif berisi gambar anjing bergerak, dengan GOP dua detik, dan menghasilkan confidence objek 0,874. Klip diam menggunakan warna abu-abu dan GOP sama. Angka berikut mencakup pemindaian, inferensi jika diperlukan, dan pembuatan thumbnail JPEG.

| Skenario | Waktu proses | RSS puncak proses + anak |
|---|---:|---:|
| Satu klip aktif 60 detik, model pertama / berikutnya | 2,50 / 2,32 detik | sekitar 103 MiB |
| Satu klip diam 60 detik, dua pengulangan | 0,45 / 0,33 detik | sekitar 62 MiB |
| Lima copy rekaman + lima preview + analisis lima klip aktif | lima analisis 14,87 detik; probe keseluruhan 15,55 detik | sekitar 613 MiB |

Dalam probe gabungan, semua sepuluh proses FFmpeg dan kelima analisis selesai sukses. Waktu tiap analisis dengan thumbnail: 3,916; 2,475; 2,514; 3,196; 2,772 detik. Total CPU seluruh keluarga proses sekitar 21,47 detik selama 15,55 detik pengukuran. RSS dijumlahkan per keluarga proses dan disampel setiap 50 ms; puncak singkat mungkin terlewat.

Probe kamera memakai lima stream-copy ke null dan lima preview 640 px/5 fps dari video sintetis 1080p15. Probe ini tidak mengukur koneksi kamera asli, audio, tulis disk, jaringan, rclone/cloud, seluruh aplikasi desktop, atau ketahanan berjam-jam. Angka di atas memakai build GNU libc; pemeriksaan target musl dilakukan terpisah. Karena itu, hasil ini memberikan margin pengujian dan **bukan sertifikasi throughput N4020**.

## Batas dan verifikasi pada perangkat

Lima kamera yang menghasilkan segmen 60 detik membutuhkan rata-rata penyelesaian analisis di bawah 12 detik per klip. Upload kini terpisah sehingga tidak tertahan oleh analisis. Antrean upload masih bisa bertambah jika cloud atau jaringan lambat; batas 256 MiB khusus data sementara analisis.

Metode hemat tidak memeriksa semua momen. Objek cepat di antara keyframe, objek jauh/kecil, atau objek di luar kandidat terkuat dapat terlewat; perubahan cahaya lokal dan salah klasifikasi juga masih mungkin. Jangan menganggap status belum dianalisis sebagai tidak ada gerakan. Kamera dengan GOP panjang, video all-I, atau beban desktop tinggi dapat lebih sering mencapai batas waktu.

Untuk mengukur klip asli pada perangkat target, jalankan dari root repo dengan Rust release:

```sh
cargo run --release --example motion_benchmark -- /path/to/clip.mp4 2
```

Contoh ini mengukur analisis beserta thumbnail, tidak mengubah rekaman/index aplikasi. Pantau `recording.analysisPending`, `analysisSkipped`, `analysisFailed`, dan antrean upload melalui `/api/cameras` selama kelima kamera aktif. Statistik antrean bersifat global dan dimulai ulang saat server restart.

## Validasi perilaku

Suite mencakup penyaringan objek kecil/noise/cahaya menyeluruh, area bergerak persisten, penolakan objek diam di luar area gerak, batas jumlah/byte antrean, pemulihan hard link setelah file asli terhapus, dan upload yang tetap selesai ketika decoder analisis sengaja digantung. Decoder yang menggantung dihentikan pada batas empat detik dan skor tetap kosong. Gambar anjing diam dan bergerak juga diuji; hanya kasus bergerak menghasilkan skor positif.

Suite workspace lulus dengan satu uji durasi MJPEG lama dikecualikan karena sebelumnya sudah direproduksi gagal pada commit awal. Helper subprocess untuk uji decoder menggantung sengaja ditandai ignored ketika dipanggil langsung, lalu dijalankan oleh uji induknya. Pemeriksaan frontend WebAssembly dan target musl lulus. Build/link release musl untuk server dan contoh benchmark berhasil; binary server dapat dijalankan dan benchmark binary musl berhasil menganalisis klip aktif 60 detik.
