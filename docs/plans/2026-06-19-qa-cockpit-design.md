# QA Cockpit — Desain v1

**Tanggal:** 2026-06-19
**Status:** Desain disepakati, siap masuk perencanaan implementasi
**Pengguna:** 1 orang (dipakai sendiri — personal QA cockpit)
**Platform:** macOS (Tauri — Rust + web UI)
**AI:** Gemma lokal via LM Studio (OpenAI-compatible API di `http://localhost:1234/v1`)

---

## 1. Inti / Jantung Aplikasi

Sebuah **desktop cockpit pribadi buat QA engineer** yang:

1. Merekam aktivitas kerja di background (app + judul window aktif).
2. Tiap hari merangkum jadi "lo ngerjain apa, berapa jam, ticket Jira mana" (pakai Gemma).
3. **Fitur pembeda utama — Detektor Keadilan Poin:** karena cuma app ini yang tahu
   waktu kerja asli, dia bisa nge-flag ticket yang under/over-pointed.

> Filosofi: app ini "selesai untuk v1" bukan saat semua fitur ada, tapi saat sudah
> dipakai 1–2 minggu beneran dan kerasa membantu. Baru lanjut ke fitur berikutnya.

---

## 2. Arsitektur (3 lapisan)

### Lapisan 1 — Perekam (background) — "mata"
- Proses Rust kecil, nyala diam-diam.
- Tiap ~5 detik mencatat: app aktif + judul window + timestamp.
- Butuh akses macOS (alasan utama pakai Tauri/Rust).
- Data langsung disimpan lokal, tidak dikirim ke mana pun.

### Lapisan 2 — Otak (pengolah)
- Sync **Jira** (API key) → ticket + story point.
- Sync **GitHub** → PR / commit.
- Panggil **Gemma** (LM Studio) untuk rangkuman & analisa naratif.
- Cocokin blok aktivitas ↔ ticket, hitung waktu asli per ticket, hitung keadilan poin.

### Lapisan 3 — Tampilan (web UI di Tauri) — "wajah"
- Dashboard: ringkasan harian, tabel ticket + status keadilan, timeline, PR, catatan.
- Semua data dari penyimpanan lokal.

**Prinsip inti:** SEMUA data disimpan lokal (SQLite) di Mac. Tidak ada upload. Privasi penuh
(wajib, karena app merekam aktivitas).

---

## 3. Alur Data Sehari-hari

**Sepanjang hari (otomatis):**
- Perekam mencatat tiap ~5 detik, tapi langsung **digabung jadi "sesi/blok"**:
  window sama berturut-turut → satu blok (`VS Code (login_test.dart) — 14:03–14:41`).
- **Idle detection:** tidak ada aktivitas mouse/keyboard sekian menit → ditandai idle,
  tidak dihitung sebagai waktu kerja.

**Sekali sehari — sync (otomatis pagi / tombol refresh):**
- Tarik Jira (ticket + story point) & GitHub (PR), simpan lokal.
- Dipisah dari perekaman supaya tidak menembak API terus-menerus.

**Saat buka dashboard / sore:**
- Cocokin blok aktivitas ↔ ticket.
- Hitung waktu per ticket vs poin.
- Gemma: rangkum naratif + terjemahkan flag keadilan jadi kalimat + saran.

### Skema SQLite (perkiraan)
- `activity_blocks` — app, judul, mulai, selesai, idle?
- `jira_tickets` — cache ticket + story point
- `pull_requests` — cache PR
- `ticket_time` — hasil hitungan waktu asli per ticket
- `notes` — catatan manual
- `ai_summaries` — cache hasil Gemma (regen hanya jika ada data baru)

**Caching AI:** Gemma lokal lambat → hasil di-cache, hanya di-generate ulang saat data berubah.

---

## 4. Pencocokan Window ↔ Ticket & Detektor Keadilan Poin

### Langkah 1 — Cocokin aktivitas ke ticket (dari petunjuk terkuat → terlemah)
1. **Nomor tiket di judul window** (paling akurat) — mis. `JIRA-1234 - Login bug`.
2. **Nama branch git** yang mengandung nomor tiket.
3. **Kecocokan kata** judul window ↔ judul ticket (dibantu AI untuk yang tanpa nomor).

**Koreksi manual (WAJIB):** tiap blok punya dropdown — kalau tebakan salah, pindahkan ke
ticket yang benar (sekali klik). Tanpa ini, angka keadilan tidak bisa dipercaya.

### Langkah 2 — Hitung waktu asli per ticket
- Jumlahkan durasi semua blok terkait (idle dibuang). Hasil: `JIRA-1234 → 6j20m`.

### Langkah 3 — Detektor Keadilan Poin
**Patokan tetap (bukan dari histori): 1 jam kerja = 2 poin (1 poin = 30 menit).**

- **Poin yang harusnya didapat** = jam kerja asli × 2.
- Bandingkan dengan poin yang di-assign di Jira:
  - `JIRA-1234`: Jira 3 poin, kerja 6 jam (harusnya 12) → 🔴 under-pointed (kurang 9).
  - `JIRA-1250`: Jira 8 poin, kerja 2 jam (harusnya 4) → 🟡 over-pointed (lebih 4).
  - selisih kecil → ✅ adil.
- Gemma menerjemahkan: *"Minggu ini lo kerja setara 40 poin, di Jira ke-assign 25 —
  ada 15 poin kerjaan yang tidak terhitung."*

**Catatan v1:** 2 poin/jam berlaku rata untuk semua jenis kerjaan (coding = meeting =
review). Pembedaan per-jenis ditunda ke versi berikutnya.

---

## 5. Peran Gemma & Dashboard

### Di mana Gemma dipakai (dan TIDAK)
- **TIDAK** (pakai kode biasa, instan): semua hitungan — total waktu, poin ×2, selisih.
- **YA** (butuh bahasa manusia):
  - Rangkuman naratif harian.
  - Terjemahan flag poin → kalimat + saran.
  - Pencocokan ambigu (blok tanpa nomor tiket).
- Panggilan AI: di-cache + jalan di background ("lagi menyusun ringkasan…") supaya
  dashboard tidak nge-hang.

### Layout dashboard (1 layar utama, beberapa panel)
- **Header:** tanggal + poin hari ini (`harusnya 14 / ke-assign 9`) + jam kerja bersih.
- **Ringkasan AI:** paragraf naratif (Gemma).
- **Tabel ticket:** judul, jam asli, poin-harusnya, poin-Jira, status 🟢🟡🔴 + dropdown koreksi.
- **Timeline aktivitas:** blok-blok hari ini.
- **Panel PR:** PR + status.
- **Panel catatan:** note manual bebas.
- **Toggle perekam on/off** yang jelas terlihat (kontrol & kepercayaan pengguna).

---

## 6. Lingkup (Scope)

### 🎯 v1 — "Cockpit Keadilan Poin"
- Perekam background B1 (app + judul window) + idle detection
- Sesi/blok aktivitas → SQLite lokal
- Sync Jira (ticket + story point) & GitHub (PR)
- Pencocokan otomatis + koreksi manual
- Hitung jam asli → poin (×2) → flag keadilan 🟢🟡🔴
- Dashboard lengkap (header, ringkasan AI, tabel, timeline, PR, catatan)
- Toggle perekam

### 📦 Lanjutan (setelah v1 terbukti dipakai)
- **v2 — Test Case Management:** simpan/atur test case + generate via Gemma dari ticket.
- **v3 — PR Risk Analyzer:** Gemma baca diff PR → highlight file berisiko (bisa colong
  logika dari project lama `~/Documents/Important/PR Analyst`).
- **v4 — Polesan:** laporan mingguan/sprint, export.

### Sengaja DIBUANG (YAGNI)
Screenshot/OCR, multi-user, cloud sync, mobile.

---

## 7. Keputusan Teknis yang Sudah Dikunci
| Topik | Keputusan |
|---|---|
| Platform | macOS, Tauri (Rust + web UI) |
| Perekaman | B1 — app + judul window (bukan screenshot/OCR) |
| Poin | Tetap: 1 jam = 2 poin, rata semua jenis kerjaan (v1) |
| Penyimpanan | SQLite lokal, tanpa cloud |
| AI | Gemma lokal (LM Studio, OpenAI-compatible) |
| Privasi | Semua lokal; toggle perekam; idle dibuang |

---

## 8. Pertanyaan Terbuka (untuk tahap perencanaan)
- Detail koneksi Jira: instance URL + format API key/token.
- Akun GitHub mana yang dipakai (user punya beberapa akun gh).
- Cara akурat baca judul window aktif di macOS dari Rust (crate/Accessibility API +
  permission yang dibutuhkan).
- Interval & strategi idle threshold (default usulan: idle > 3 menit).
- Model Gemma persisnya (kemungkinan Gemma 3 4B) + batas konteks untuk rangkuman.
