# Guss

[![CI](https://github.com/okelojnr-acyn/Guss/actions/workflows/ci.yml/badge.svg)](https://github.com/okelojnr-acyn/Guss/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org)

Asisten bisnis mandiri yang Anda ajak bicara, bukan dasbor yang harus Anda
cek terus-menerus.

Ceritakan apa yang terjadi di usaha Anda hari ini - dengan mengetik atau
dengan suara, lewat CLI, Telegram, atau WhatsApp - dan Guss akan mencatat
penjualan, pengeluaran, dan perubahan stok. Minta laporan kapan saja, atau
biarkan Guss mengirimkannya secara otomatis setiap akhir hari, setiap
minggu, dan setiap bulan: apa yang terjual, stok apa yang menipis, produk
mana yang tren-nya naik atau turun, laba/rugi, dan perkiraan sederhana
untuk pendapatan berulang.

Baca dalam [Bahasa Inggris](README.md).

## Mengapa ini dibuat

Kebanyakan "AI" untuk usaha kecil hanyalah chatbot yang ditempelkan ke
spreadsheet, atau dasbor SaaS yang mengunci data Anda di balik biaya
berlangganan. Guss bukan keduanya: ini adalah program Rust yang Anda
jalankan sendiri, menggunakan API key dan kredensial milik *Anda*, dengan
data tersimpan di file SQLite di komputer Anda sendiri. Tidak ada data
yang dikirim ke pihak lain. Tidak ada yang di-hosting untuk Anda. Clone
repositorinya, jawab beberapa pertanyaan setup, dan itu sudah jadi milik
Anda.

## Apa yang sebenarnya dilakukan Guss

- **Mencatat kejadian bisnis dari bahasa alami.** "Terjual 3 karung beras
  seharga 500 masing-masing" menjadi data penjualan terstruktur - nama
  barang, jumlah, harga, total - bukan sekadar catatan samar. Berlaku
  sama untuk pengeluaran, restock, dan catatan bebas yang ingin Anda
  simpan.
- **Konfirmasi sebelum benar-benar disimpan.** Setiap catatan langsung
  ditampilkan kembali, dan Anda punya waktu 5 menit untuk membalas
  "tidak" (atau "no") untuk membatalkannya. LLM yang mengekstrak angka
  dari ucapan kadang bisa salah - Guss dirancang agar kesalahan selalu
  bisa dikoreksi, tidak langsung tertanam permanen di pembukuan Anda.
- **Melaporkan kapan saja atau sesuai jadwal.** Ketik "report" (atau
  "report week", "report month") kapan saja, atau atur Guss untuk
  otomatis mengirim laporan setiap akhir hari, setiap Senin, dan setiap
  tanggal 1 - pendapatan, pengeluaran, produk paling laris/paling lambat,
  stok menipis, dan perkiraan sederhana pendapatan berulang untuk
  pemasukan bergaya langganan.
- **Bisa membalas dengan suara.** Balasan bisa diucapkan, bukan hanya
  dicetak - secara lokal dan gratis lewat mesin TTS
  [Piper](https://github.com/rhasspy/piper), atau lewat ElevenLabs jika
  Anda punya API key-nya.
- **Berfungsi penuh dalam Bahasa Indonesia atau Inggris** - prompt,
  balasan, dan laporan, bukan cuma dokumentasinya.

## Pilih kanal Anda

| Kanal | Usaha setup | Catatan |
|---|---|---|
| **CLI** | Tidak ada | Tinggal ketik di terminal. Cara terbaik untuk mencoba Guss pertama kali. |
| **Telegram** | ~2 menit | Bot token gratis, mendukung teks dan voice note. |
| **WhatsApp** | ~30 menit | Meta Cloud API resmi - jalur yang sesuai ToS, bukan bridge tidak resmi yang berisiko membuat nomor Anda diblokir. |

## Pilih LLM Anda

Satu implementasi provider melayani ketiganya - cukup ganti URL-nya:

| Provider | Biaya | Setup |
|---|---|---|
| **[Ollama](docs/setup-ollama.md)** | Gratis, jalan di komputer sendiri | Tanpa API key, berfungsi offline |
| **[Groq](docs/setup-groq.md)** | Gratis (tier gratis), sangat cepat | Perlu API key gratis |
| **OpenAI** | Berbayar | Perlu API key |

## Mulai cepat

```bash
git clone https://github.com/okelojnr-acyn/Guss.git
cd Guss
cargo run -p guss -- init > .env   # jawab beberapa pertanyaan
cargo run -p guss                   # menjalankan kanal CLI secara default
```

Lalu tinggal ketik apa yang terjadi:

```
> terjual 3 karung beras seharga 500 masing-masing
Terjual: 3 beras seharga 500.00 USD = 1500.00 USD

Balas "tidak" dalam 5 menit untuk membatalkan.
> report
Ini laporan untuk My Shop, mencakup 24 jam terakhir.

Pendapatan sebesar 1500.00 USD dan pengeluaran sebesar 0.00 USD, dengan laba bersih 1500.00 USD.
Produk paling banyak terjual: beras (3 terjual).
```

Ingin pakai Telegram atau WhatsApp ketimbang CLI? Lihat
[docs/setup-telegram.md](docs/setup-telegram.md) atau
[docs/setup-whatsapp-cloud-api.md](docs/setup-whatsapp-cloud-api.md),
lalu atur `GUSS_CHANNEL__KIND` di file `.env` Anda.

Ingin fitur suara? Lihat
[docs/setup-elevenlabs-piper.md](docs/setup-elevenlabs-piper.md) untuk
text-to-speech lokal gratis (dan speech-to-text, yang perlu toolchain
C/C++ untuk di-build - lihat dokumen itu untuk alasan dan caranya).

### Docker

```bash
docker compose up --build
```

Tambahkan `--profile ollama` untuk juga menjalankan Ollama lokal dalam
compose stack yang sama, atau `--profile postgres` untuk memakai Postgres
alih-alih file SQLite default (build dengan `GUSS_FEATURES=postgres` dan
atur `GUSS_STORE__URL` sesuai - lihat
[docker-compose.yml](docker-compose.yml) dan
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)).

## Cara ini dibangun

Guss adalah Cargo workspace berisi beberapa crate yang masing-masing
fokus pada satu hal - model domain, penyimpanan, ekstraksi LLM,
insight/laporan, suara, kanal, penjadwalan - dan masing-masing diuji
secara independen. Lihat
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) untuk desain lengkapnya,
termasuk keputusan-keputusan reliabilitas (konfirmasi-sebelum-disimpan,
dedup webhook, concurrency terbatas, ekstraksi structured-output) yang
muncul dari riset bagaimana sistem serupa biasanya gagal di produksi.

## Status

Ini adalah Fase 1 yang nyata dan berfungsi: setiap kanal, pipeline
ekstraksi LLM, insight/laporan, scheduler, dan kedua backend penyimpanan
sudah diimplementasikan dan diuji otomatis (jalankan `cargo test
--workspace --no-default-features` untuk suite sqlite default; CI juga
menjalankan suite Postgres dan Whisper di bawah ini).

- **Penyimpanan Postgres** sudah terhubung lewat `--features postgres`
  (`PostgresStore`, dipilih saat runtime dari skema `GUSS_STORE__URL` -
  lihat [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)), dengan test suite
  sendiri yang berjalan di CI melawan Postgres service container nyata.
- **Speech-to-text Whisper lokal** diaktifkan lewat fitur Cargo
  `local-whisper` (perlu toolchain C/C++ + CMake untuk build - lihat
  [docs/setup-elevenlabs-piper.md](docs/setup-elevenlabs-piper.md)) dan
  sudah diverifikasi end-to-end di CI melawan sampel suara sintetis nyata
  (lihat job `whisper-e2e` di
  [.github/workflows/ci.yml](.github/workflows/ci.yml)).

## Berkontribusi

Lihat [CONTRIBUTING.md](CONTRIBUTING.md) (Bahasa Inggris) untuk setup
dev dan pengecekan yang dijalankan CI pada setiap PR, serta
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Menemukan masalah keamanan?
Lihat [SECURITY.md](SECURITY.md) - jangan buka issue publik untuk itu.

## Lisensi

[MIT](LICENSE)
