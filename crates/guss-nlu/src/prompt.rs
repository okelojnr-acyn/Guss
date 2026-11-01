/// Language is a per-instance config choice (set once via `guss init`),
/// not detected per-message - see docs/ARCHITECTURE.md for why.
pub fn system_prompt(language: &str, context_snippets: &[String]) -> String {
    let context_block = if context_snippets.is_empty() {
        String::new()
    } else {
        let joined = context_snippets.join("\n---\n");
        match language {
            "id" => format!("\n\nKonteks bisnis yang diketahui:\n{joined}\n"),
            _ => format!("\n\nKnown business context:\n{joined}\n"),
        }
    };

    match language {
        "id" => format!(
            "Anda adalah asisten pembukuan untuk usaha kecil. Ubah pesan pemilik usaha \
             (teks atau hasil transkripsi suara) menjadi daftar kejadian bisnis terstruktur: \
             penjualan (sale), pengeluaran (expense), perubahan stok (stock_update), atau catatan (note). \
             Satu kalimat bisa menghasilkan lebih dari satu kejadian. Jika tidak yakin tentang suatu \
             detail, JANGAN mengada-ada - masukkan sebagai 'note' saja. Jangan pernah mengubah angka \
             yang disebutkan. Balas HANYA dengan JSON yang sesuai skema yang diberikan, tanpa teks lain.{context_block}"
        ),
        _ => format!(
            "You are a bookkeeping assistant for a small business. Convert the owner's message \
             (text or voice transcript) into a list of structured business events: sale, expense, \
             stock_update, or note. One utterance can produce more than one event. If you are not \
             confident about a detail, do NOT guess - record it as a 'note' instead. Never invent or \
             alter numbers that were not actually stated. Reply with ONLY JSON matching the given \
             schema, no other text.{context_block}"
        ),
    }
}

/// Re-prompt used when the model's previous response failed to parse
/// against the schema - feeds back its own bad output and the parse
/// error so it can self-correct, rather than retrying blind.
pub fn retry_user_prompt(original_text: &str, raw_bad_output: &str, parse_error: &str, language: &str) -> String {
    match language {
        "id" => format!(
            "Pesan asli: \"{original_text}\"\n\n\
             Jawaban Anda sebelumnya gagal diproses: {parse_error}\n\
             Jawaban sebelumnya: {raw_bad_output}\n\n\
             Balas lagi dengan HANYA JSON yang valid sesuai skema."
        ),
        _ => format!(
            "Original message: \"{original_text}\"\n\n\
             Your previous response failed to parse: {parse_error}\n\
             Your previous response was: {raw_bad_output}\n\n\
             Reply again with ONLY valid JSON matching the schema."
        ),
    }
}
