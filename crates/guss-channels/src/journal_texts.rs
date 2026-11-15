//! Replies for the journal: history, voids and approvals (en/id).

fn id(language: &str) -> bool {
    language == "id"
}

pub fn history_title(language: &str) -> &'static str {
    if id(language) {
        "Catatan terakhir (terbaru di atas):"
    } else {
        "Latest entries (newest first):"
    }
}

pub fn history_empty(language: &str) -> &'static str {
    if id(language) {
        "Belum ada catatan."
    } else {
        "Nothing recorded yet."
    }
}

pub fn voided_suffix(by: &str, reason: &str, language: &str) -> String {
    if id(language) {
        format!(" [DIBATALKAN oleh {by}: {reason}]")
    } else {
        format!(" [VOIDED by {by}: {reason}]")
    }
}

pub fn no_reason(language: &str) -> &'static str {
    if id(language) {
        "tanpa alasan"
    } else {
        "no reason given"
    }
}

pub fn no_such_entry(number: i64, language: &str) -> String {
    if id(language) {
        format!("Tidak ada catatan #{number}. Lihat /history.")
    } else {
        format!("There's no entry #{number}. See /history.")
    }
}

pub fn already_voided(number: i64, by: &str, language: &str) -> String {
    if id(language) {
        format!("#{number} sudah dibatalkan oleh {by}.")
    } else {
        format!("#{number} was already voided by {by}.")
    }
}

pub fn voided(line: &str, language: &str) -> String {
    if id(language) {
        format!("Dibatalkan: {line}\nTidak dihitung lagi, tapi tetap ada di /history.")
    } else {
        format!("Voided: {line}\nIt no longer counts, and stays visible in /history.")
    }
}

pub fn void_requested(number: i64, language: &str) -> String {
    if id(language) {
        format!(
            "Permintaan membatalkan #{number} sudah dikirim. Pemilik atau manajer yang memutuskan; \
             sampai saat itu catatan ini tetap dihitung."
        )
    } else {
        format!(
            "Asked for #{number} to be voided. The owner or a manager decides; until then it \
             still counts."
        )
    }
}

pub fn void_request_notice(
    who: &str,
    request: i64,
    line: &str,
    reason: &str,
    language: &str,
) -> String {
    if id(language) {
        format!(
            "{who} minta membatalkan catatan ini (permintaan {request}):\n{line}\nAlasan: {reason}\n\n\
             Balas /approve {request} atau /reject {request}."
        )
    } else {
        format!(
            "{who} asks to void this entry (request {request}):\n{line}\nReason: {reason}\n\n\
             Reply /approve {request} or /reject {request}."
        )
    }
}

pub fn not_allowed(language: &str) -> &'static str {
    if id(language) {
        "Maaf, hanya pemilik atau manajer yang bisa melakukan itu."
    } else {
        "Sorry, only the owner or a manager can do that."
    }
}

pub fn nothing_pending(language: &str) -> &'static str {
    if id(language) {
        "Tidak ada permintaan pembatalan yang menunggu."
    } else {
        "No void requests are waiting."
    }
}

pub fn pending_line(request: i64, who: &str, reason: &str, entry: &str, language: &str) -> String {
    if id(language) {
        format!(
            "Permintaan {request} dari {who} ({reason}):\n{entry}\n/approve {request} · /reject {request}"
        )
    } else {
        format!(
            "Request {request} from {who} ({reason}):\n{entry}\n/approve {request} · /reject {request}"
        )
    }
}

pub fn no_such_request(number: i64, language: &str) -> String {
    if id(language) {
        format!("Tidak ada permintaan {number} yang menunggu. Lihat /pending.")
    } else {
        format!("There's no waiting request {number}. See /pending.")
    }
}

pub fn decided(request: i64, entry: i64, approved: bool, language: &str) -> String {
    match (approved, id(language)) {
        (true, true) => format!("Disetujui: #{entry} dibatalkan (permintaan {request})."),
        (false, true) => format!("Ditolak: #{entry} tetap dihitung (permintaan {request})."),
        (true, false) => format!("Approved: #{entry} is voided (request {request})."),
        (false, false) => format!("Rejected: #{entry} still counts (request {request})."),
    }
}

pub fn decision_notice(entry: i64, approved: bool, by: &str, language: &str) -> String {
    match (approved, id(language)) {
        (true, true) => format!("{by} menyetujui: #{entry} sudah dibatalkan."),
        (false, true) => format!("{by} menolak membatalkan #{entry}; catatan itu tetap dihitung."),
        (true, false) => format!("{by} approved: #{entry} is voided."),
        (false, false) => format!("{by} said no to voiding #{entry}; it still counts."),
    }
}

pub fn longer_reports_not_allowed(language: &str) -> &'static str {
    if id(language) {
        "Laporan mingguan dan bulanan hanya untuk pemilik dan manajer. /report menampilkan hari ini."
    } else {
        "Week and month reports are for the owner and managers. /report shows today."
    }
}

pub fn not_in_friday(why: &str, language: &str) -> String {
    if id(language) {
        format!(" [belum di Friday: {why}]")
    } else {
        format!(" [not in Friday: {why}]")
    }
}

fn qty(n: f64) -> String {
    if n.fract().abs() < 1e-9 {
        format!("{n:.0}")
    } else {
        format!("{n:.2}")
    }
}

pub fn stock_left(
    item: &str,
    left: f64,
    unit: Option<&str>,
    known: bool,
    language: &str,
) -> String {
    let unit = unit.map(|u| format!(" {u}")).unwrap_or_default();
    match (known, id(language)) {
        (false, true) => format!("Belum ada stok {item} yang tercatat."),
        (false, false) => format!("No stock of {item} has been recorded yet."),
        (true, true) => format!("Sisa {item}: {}{unit}.", qty(left)),
        (true, false) => format!("{item}: {}{unit} left.", qty(left)),
    }
}

fn period(window: guss_core::store::TimeWindow, language: &str) -> &'static str {
    use guss_core::store::TimeWindow::*;
    match (window, id(language)) {
        (Day, true) => "hari ini",
        (Week, true) => "7 hari terakhir",
        (Month, true) => "30 hari terakhir",
        (Day, false) => "today",
        (Week, false) => "in the last 7 days",
        (Month, false) => "in the last 30 days",
    }
}

pub(crate) fn totals_about(
    about: &str,
    window: guss_core::store::TimeWindow,
    t: &crate::orchestrator::AboutTotals,
    language: &str,
) -> String {
    let when = period(window, language);
    if t.entries == 0 {
        return if id(language) {
            format!("Tidak ada catatan untuk {about} {when}.")
        } else {
            format!("Nothing recorded for {about} {when}.")
        };
    }
    let list = |v: &[guss_core::money::CurrencyAmount]| {
        v.iter()
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(" + ")
    };
    let mut parts = Vec::new();
    if !t.money_in.is_empty() {
        parts.push(if id(language) {
            format!("terjual {} seharga {}", qty(t.sold), list(&t.money_in))
        } else {
            format!("sold {} for {}", qty(t.sold), list(&t.money_in))
        });
    }
    if !t.money_out.is_empty() {
        parts.push(if id(language) {
            format!("dibayar {}", list(&t.money_out))
        } else {
            format!("paid {}", list(&t.money_out))
        });
    }
    if id(language) {
        format!(
            "{about} {when}: {} ({} catatan).",
            parts.join(", "),
            t.entries
        )
    } else {
        format!(
            "{about} {when}: {} ({} entries).",
            parts.join(", "),
            t.entries
        )
    }
}
