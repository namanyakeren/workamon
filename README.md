# WORKA MON

Versi 0.2.0. Pemantau server rumah untuk Raspberry Pi dan mesin Linux kecil. Satu halaman
untuk melihat CPU, RAM, penyimpanan host, dan container Docker, lengkap dengan
riwayat SQLite, log aplikasi, serta aksi start/stop/restart container.

Tampilan dibuat sederhana: server-rendered, tanpa framework frontend, tema
terang sebagai default, dan tombol tema gelap di kanan atas (atau `Shift + D`).

## Fitur

- Ringkasan CPU, RAM, swap, disk, load average, suhu, dan uptime host.
- Daftar container Docker: status, healthcheck, image, port, CPU, memori, jaringan.
- Aksi start, stop, restart, dan hapus container langsung dari browser.
- Buat container baru dari image lokal, lengkap dengan command, environment,
  port, volume, restart policy, dan opsi langsung dijalankan.
- Menu Images untuk melihat image, inspect detail, menarik image baru dari
  registry, menghapus image (termasuk force delete), dan prune image dangling.
- Detail container: informasi inspect, mount, network, log 150 baris terakhir,
  dan grafik riwayat 24 jam.
- Riwayat metrik live, 1 jam, 6 jam, 24 jam, dan 7 hari. Tab live diperbarui
  otomatis setiap 5 detik tanpa menunggu sampel database.
- Antarmuka dua bahasa (Bahasa Indonesia dan English) dengan tombol ID/EN.
- Log aplikasi (INFO/WARN/ERROR) yang dapat difilter, dicari, dan dipaginasi.
- Pembersihan objek Docker: container berhenti, image dangling, build cache,
  network, dan volume tidak terpakai.
- Database SQLite dengan batas ukuran otomatis. Data tertua dihapus saat file
  menyentuh batas (default 10 MB), tanpa perlu cron eksternal.
- Login satu pengguna. Kredensial diambil dari environment.
- Tetap berjalan meskipun Docker tidak tersedia; halaman container menampilkan
  status offline.

## Menjalankan dengan Docker Compose

Prasyarat: Docker Engine dan Docker Compose v2 di host.

```bash
cp .env.example .env
# edit .env, minimal ganti WORKAMON_PASSWORD
docker compose up -d --build
```

Buka `http://IP-SERVER:8080`.

Untuk memakai port host lain, ubah `WORKAMON_PORT` di `.env` (misalnya
`WORKAMON_PORT=9090`), lalu jalankan ulang `docker compose up -d`. Bisa juga
langsung dari shell:

```bash
WORKAMON_PORT=9090 docker compose up -d --build
```

Kredensial default adalah `admin` / `workamon`. **Ganti password sebelum
membuka aplikasi ke jaringan.**

Cek GID socket Docker agar container bisa mengakses daemon:

```bash
stat -c '%g' /var/run/docker.sock
# isi hasilnya ke DOCKER_GID di .env
```

Container berjalan sebagai UID/GID `1000:1000`, membaca root host secara
read-only di `/host`, dan memakai named volume `workamon-data` untuk database.
Jika memakai bind mount untuk `/data`, pastikan folder tersebut dapat ditulis
oleh UID 1000.

Perintah operasional:

```bash
docker compose logs -f workamon
docker compose restart workamon
docker compose down            # data tetap tersimpan di volume
docker compose down -v         # hapus database juga
```

## Menjalankan tanpa Docker

Prasyarat: Rust 1.85+ (edition 2024).

```bash
cargo run --release
```

Saat dijalankan langsung di host, `WORKAMON_HOST_ROOT` cukup `/` dan socket
Docker default (`/var/run/docker.sock`) dipakai otomatis. Bila ingin mematikan
integrasi Docker:

```bash
WORKAMON_DOCKER=0 cargo run --release
```

Buka `http://127.0.0.1:8080`.

## Konfigurasi

Semua konfigurasi lewat environment variable.

| Variabel | Default | Keterangan |
| --- | --- | --- |
| `WORKAMON_ADDR` | `0.0.0.0:8080` | Alamat bind HTTP. |
| `WORKAMON_USER` | `admin` | Username login. |
| `WORKAMON_PASSWORD` | `workamon` | Password login. Wajib diganti. |
| `WORKAMON_DB` | `data/workamon.db` | Lokasi file SQLite. |
| `WORKAMON_DB_MAX_MB` | `10` | Batas ukuran database (1–512 MB). |
| `WORKAMON_SAMPLE_SECS` | `30` | Interval sampel sistem (5–3600 detik). |
| `WORKAMON_CONTAINER_SAMPLE_SECS` | `300` | Interval riwayat container. |
| `WORKAMON_SESSION_HOURS` | `168` | Masa berlaku sesi login. |
| `WORKAMON_LOG_RETENTION_DAYS` | `14` | Retensi log aplikasi. |
| `WORKAMON_HOST_ROOT` | `/` | Root host untuk disk dan suhu. Di container: `/host`. |
| `WORKAMON_DOCKER` | `1` | Set `0` untuk mematikan integrasi Docker. |
| `RUST_LOG` | `workamon=info,tower_http=warn,warn` | Filter log tracing. |
| `TZ` | sistem | Zona waktu tampilan, misal `Asia/Jakarta`. |

## Penyimpanan dan retensi

Database memakai mode WAL. Tiga tabel menyimpan data:

- `metrics` — sampel sistem.
- `container_metrics` — riwayat container yang berjalan.
- `app_logs` — log aplikasi dan audit aksi.

Setiap dua menit aplikasi membersihkan log yang melewati masa retensi dan
memeriksa ukuran file. Jika ukuran database (termasuk WAL) melewati batas,
baris tertua dihapus bertahap, WAL di-checkpoint, lalu `VACUUM` dijalankan bila
masih perlu. Halaman **Pemeliharaan** menyediakan pangkas manual, `VACUUM`,
hapus log lama, serta kosongkan log/metrik.

Sesi login disimpan di memori, jadi restart aplikasi akan mengeluarkan semua
pengguna. Riwayat tetap tersimpan di SQLite.

## Keamanan

- Aplikasi ini dirancang untuk jaringan rumah tepercaya. Tidak ada HTTPS
  bawaan; gunakan reverse proxy (Caddy, Nginx, Traefik) bila diakses dari luar.
- Cookie sesi memakai `HttpOnly` dan `SameSite=Strict`.
- Password dibandingkan dengan perbandingan waktu konstan.
- Container Docker diberi akses ke socket Docker. Akses ke socket setara dengan
  akses root di host. Batasi port aplikasi dan jangan mengeksposnya ke internet
  tanpa autentikasi tambahan.
- Mount `/:/host:ro` hanya untuk membaca statistik disk dan suhu host.

## Struktur proyek

```
src/
  main.rs      entrypoint, startup, graceful shutdown
  config.rs    konfigurasi environment
  db.rs        skema SQLite, query, retensi ukuran
  docker.rs    integrasi Bollard
  monitor.rs   loop sampling dan pemeliharaan
  web.rs       router dan handler HTTP
  state.rs     AppState, sesi, middleware auth
  i18n.rs      kamus Bahasa Indonesia / English
  render.rs    MiniJinja dan grafik SVG
  logging.rs   layer tracing ke SQLite
  util.rs      format, disk, suhu, ANSI
templates/     halaman MiniJinja
assets/        CSS, JS, favicon, i18n.json
```

## Lisensi

MIT. Lihat [LICENSE](LICENSE).
