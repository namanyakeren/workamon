# WORKA MON

Versi 0.3.7. Panel server rumah untuk Raspberry Pi dan mesin Linux kecil. Satu
halaman untuk melihat CPU, RAM, penyimpanan host, container Docker, service
systemd, paket terpasang, jaringan, dan Cloudflare Tunnel. Workamon juga bisa
memasang aplikasi dari katalog paket Debian, mengelola service, dan membuka
terminal host langsung dari browser tanpa SSH.

Tampilan dibuat sederhana: server-rendered, tanpa framework frontend, tema
terang sebagai default, dan tombol tema gelap di kanan atas (atau `Shift + D`).

## Fitur

- Ringkasan CPU, RAM, swap, disk, load average, suhu, dan uptime host.
- Daftar container Docker: status, healthcheck, image, port, CPU, memori, jaringan.
- Aksi start, stop, restart, dan hapus container langsung dari browser. Status
  container diperbarui segera setelah aksi berhasil, tanpa refresh manual.
- Buat container baru dari image lokal, lengkap dengan command, environment,
  port, volume, restart policy, dan opsi langsung dijalankan.
- Menu Images untuk melihat image, inspect detail, menarik image baru dari
  registry, menghapus image (termasuk force delete), dan prune image dangling.
- Detail container: informasi inspect, mount, network, log 150 baris terakhir,
  dan grafik riwayat 24 jam.
- Halaman Apps & Services: daftar service systemd dan container Docker dalam
  satu tabel, lengkap dengan status, CPU, memori, jaringan, dan I/O disk per
  item. Service bisa di-start, stop, restart, enable, dan disable. Unit template
  systemd seperti `autovt@.service` disaring agar tidak mengganggu discovery.
- Katalog instalasi aplikasi Debian/Raspberry Pi OS (nginx, Caddy, Cockpit,
  Samba, Mosquitto, Node Exporter, Fail2ban, UFW, Avahi, CUPS, Netdata,
  WireGuard) plus paket kustom. Daftar paket terpasang bisa dicari dan dihapus.
- Terminal host live berbasis xterm.js dan WebSocket. Shell berjalan di namespace
  host melalui `nsenter`, tanpa perlu SSH. Environment terminal membangun
  `HOME`, `USER`, dan `PATH` dari user host sehingga tool user seperti
  `opencode` dapat digunakan. User dapat dipilih dengan `WORKAMON_TERMINAL_USER`.
- Monitoring jaringan live: interface, alamat IP, kecepatan RX/TX, total
  trafik, gateway default, DNS, grafik trafik, dan status Cloudflare Tunnel
  (`cloudflared`) baik sebagai service systemd maupun container Docker.
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

Compose memakai `restart: always`, sehingga container Workamon akan dijalankan
kembali oleh Docker setelah daemon Docker aktif pasca-reboot host. Pastikan
Docker Engine juga diaktifkan saat boot (contoh pada systemd: `sudo systemctl
enable --now docker`). Setelah reboot, verifikasi dengan:

```bash
docker compose ps
docker compose logs --tail=100 workamon
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

Untuk fitur host (systemd, paket, terminal), container dijalankan sebagai root
dengan `pid: host` dan `privileged: true`. Root host dipasang read-only di
`/host` untuk pembacaan statistik, sementara perintah host dijalankan lewat
`nsenter` ke namespace host. Named volume `workamon-data` menyimpan database.

Terminal tetap dijalankan dari proses host dengan hak root, tetapi environment
user dapat diarahkan ke user login host. Jika auto-detection tidak memilih user
yang benar, isi `.env` seperti berikut lalu recreate container:

```dotenv
WORKAMON_TERMINAL_USER=nama-user-host
```

Jika dikosongkan, Workamon mencari user non-root yang memiliki instalasi
`opencode` di lokasi umum seperti `~/.local/bin`, `~/.opencode/bin`,
`~/.npm-global/bin`, atau `~/.bun/bin`.

Jika Anda hanya ingin memantau Docker dan tidak membutuhkan kontrol host, set
`WORKAMON_HOST_EXEC=0` lalu hapus `privileged: true`, `pid: host`, dan
`user: "0:0"` dari `docker-compose.yml` (atau gunakan user `1000:1000`). Dengan
begitu halaman Apps, paket, dan terminal host akan menampilkan status bahwa
akses host dimatikan.

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
Docker default (`/var/run/docker.sock`) dipakai otomatis. Akses host untuk
systemd, apt, dan terminal dimatikan secara default. Untuk mengaktifkannya,
jalankan sebagai root:

```bash
sudo WORKAMON_HOST_EXEC=1 cargo run --release
```

Bila ingin mematikan integrasi Docker:

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
| `WORKAMON_HOST_ROOT` | `/` | Root host untuk disk, suhu, dan `/proc/net/dev`. Di container: `/host`. |
| `WORKAMON_HOST_EXEC` | `0` | Set `1` untuk mengaktifkan systemd, apt, dan terminal host. Compose default: `1`. |
| `WORKAMON_TERMINAL_USER` | kosong | User host yang dipakai untuk `HOME`, `USER`, dan path tool terminal. Jika kosong, user dengan instalasi `opencode` akan dicari otomatis. |
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
- Mode host menjalankan container sebagai root dengan `privileged: true` dan
  `pid: host`. Terminal dan aksi systemd/apt memiliki akses root penuh ke host.
  Gunakan password kuat, HTTPS/reverse proxy, dan jangan membuka port Workamon
  langsung ke internet. Jika tidak membutuhkan fitur ini, matikan
  `WORKAMON_HOST_EXEC` dan hapus konfigurasi privileged dari Compose.
- Mount `/:/host:ro` dipakai untuk membaca statistik disk, suhu, dan jaringan
  host. Perintah host berjalan melalui `nsenter` ke namespace host.
- WebSocket terminal memeriksa header `Origin` dan hanya tersedia setelah login.

## Troubleshooting

### Container tidak muncul setelah reboot

Pastikan Docker aktif saat boot dan Compose dijalankan dari folder proyek:

```bash
sudo systemctl enable --now docker
docker compose up -d
docker compose ps
```

Policy `restart: always` akan menangani restart container setelah Docker kembali
aktif. Jika image atau konfigurasi berubah, gunakan `docker compose up -d --build`.

### Service systemd tidak tampil

Pastikan deployment memakai `privileged: true`, `pid: host`, `user: "0:0"`, dan
`WORKAMON_HOST_EXEC=1`. Workamon hanya mengirim unit service konkret ke
`systemctl show`; unit template seperti `autovt@.service` sengaja tidak
ditampilkan karena bukan invocation unit yang valid untuk pengambilan properti.

### `opencode: command not found` di terminal

Tentukan user host secara eksplisit:

```dotenv
WORKAMON_TERMINAL_USER=nama-user-host
```

Lalu jalankan ulang container:

```bash
docker compose up -d --force-recreate
```

Pastikan executable dapat dijalankan oleh user tersebut dan berada di salah
satu direktori `PATH` user, misalnya `~/.local/bin/opencode` atau
`~/.opencode/bin/opencode`.

### `Gagal menjalankan shell host: No such file or directory (os error 2)`

Pada versi 0.3.6 dan sebelumnya, terminal mengisi `HOME` dengan home user host
(misalnya `/home/pi`). `portable-pty` melakukan `chdir` ke nilai tersebut
sebelum `exec`, sedangkan path itu tidak ada di dalam mount namespace
container, sehingga `nsenter` tidak pernah dijalankan. Versi 0.3.7 mematok
working directory ke `/` sehingga masalah ini tidak lagi muncul.

Setelah memperbarui kode, image container harus dibangun ulang agar binary
baru terpakai:

```bash
docker compose build --no-cache
docker compose up -d --force-recreate
```

Jika error tetap muncul, pastikan `nsenter` tersedia di image:

```bash
docker compose exec workamon which nsenter
```

## Struktur proyek

```
src/
  main.rs      entrypoint, startup, graceful shutdown
  config.rs    konfigurasi environment
  db.rs        skema SQLite, query, retensi ukuran
  docker.rs    integrasi Bollard
  host.rs      nsenter, systemd, apt, jaringan, statistik service
  monitor.rs   loop sampling dan pemeliharaan
  web.rs       router dan handler HTTP, terminal WebSocket
  state.rs     AppState, sesi, middleware auth
  i18n.rs      kamus Bahasa Indonesia / English
  render.rs    MiniJinja dan grafik SVG
  logging.rs   layer tracing ke SQLite
  util.rs      format, disk, suhu, ANSI
templates/     halaman MiniJinja
assets/        CSS, JS, font, xterm.js, favicon, i18n.json
```

## Lisensi

MIT. Lihat [LICENSE](LICENSE).
