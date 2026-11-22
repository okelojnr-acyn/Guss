//! Replies for the journal: history, voids and approvals (en/id/sw).

/// The reply language: English, Indonesian or Swahili (anything else
/// reads as English).
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Lang {
    En,
    Id,
    Sw,
}

pub(crate) fn lang(language: &str) -> Lang {
    match language {
        "id" => Lang::Id,
        "sw" => Lang::Sw,
        _ => Lang::En,
    }
}

/// The same short text in each language.
pub(crate) fn pick(
    language: &str,
    en: &'static str,
    id: &'static str,
    sw: &'static str,
) -> &'static str {
    match lang(language) {
        Lang::En => en,
        Lang::Id => id,
        Lang::Sw => sw,
    }
}

pub fn history_title(language: &str) -> &'static str {
    pick(
        language,
        "Latest entries (newest first):",
        "Catatan terakhir (terbaru di atas):",
        "Rekodi za mwisho (mpya juu):",
    )
}

pub fn history_empty(language: &str) -> &'static str {
    pick(
        language,
        "Nothing recorded yet.",
        "Belum ada catatan.",
        "Bado hakuna kilichorekodiwa.",
    )
}

pub fn voided_suffix(by: &str, reason: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(" [DIBATALKAN oleh {by}: {reason}]"),
        Lang::Sw => format!(" [IMEFUTWA na {by}: {reason}]"),
        Lang::En => format!(" [VOIDED by {by}: {reason}]"),
    }
}

pub fn no_reason(language: &str) -> &'static str {
    pick(language, "no reason given", "tanpa alasan", "hakuna sababu")
}

pub fn no_such_entry(number: i64, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!("Tidak ada catatan #{number}. Lihat /history."),
        Lang::Sw => format!("Hakuna rekodi #{number}. Angalia /history."),
        Lang::En => format!("There's no entry #{number}. See /history."),
    }
}

pub fn already_voided(number: i64, by: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!("#{number} sudah dibatalkan oleh {by}."),
        Lang::Sw => format!("#{number} ilishafutwa na {by}."),
        Lang::En => format!("#{number} was already voided by {by}."),
    }
}

pub fn voided(line: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!("Dibatalkan: {line}\nTidak dihitung lagi, tapi tetap ada di /history."),
        Lang::Sw => {
            format!("Imefutwa: {line}\nHaihesabiwi tena, lakini bado inaonekana kwenye /history.")
        }
        Lang::En => format!("Voided: {line}\nIt no longer counts, and stays visible in /history."),
    }
}

pub fn void_requested(number: i64, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "Permintaan membatalkan #{number} sudah dikirim. Pemilik atau manajer yang memutuskan; \
             sampai saat itu catatan ini tetap dihitung."
        ),
        Lang::Sw => format!(
            "Ombi la kufuta #{number} limetumwa. Mwenye duka au meneja ataamua; hadi hapo bado \
             inahesabiwa."
        ),
        Lang::En => format!(
            "Asked for #{number} to be voided. The owner or a manager decides; until then it \
             still counts."
        ),
    }
}

pub fn void_request_notice(
    who: &str,
    request: i64,
    line: &str,
    reason: &str,
    language: &str,
) -> String {
    match lang(language) {
        Lang::Id => format!(
            "{who} minta membatalkan catatan ini (permintaan {request}):\n{line}\nAlasan: {reason}\n\n\
             Balas /approve {request} atau /reject {request}."
        ),
        Lang::Sw => format!(
            "{who} anaomba kufuta rekodi hii (ombi {request}):\n{line}\nSababu: {reason}\n\n\
             Jibu /approve {request} au /reject {request}."
        ),
        Lang::En => format!(
            "{who} asks to void this entry (request {request}):\n{line}\nReason: {reason}\n\n\
             Reply /approve {request} or /reject {request}."
        ),
    }
}

pub fn not_allowed(language: &str) -> &'static str {
    pick(
        language,
        "Sorry, only the owner or a manager can do that.",
        "Maaf, hanya pemilik atau manajer yang bisa melakukan itu.",
        "Samahani, ni mwenye duka au meneja tu anayeweza kufanya hivyo.",
    )
}

pub fn nothing_pending(language: &str) -> &'static str {
    pick(
        language,
        "No void requests are waiting.",
        "Tidak ada permintaan pembatalan yang menunggu.",
        "Hakuna maombi ya kufuta yanayosubiri.",
    )
}

pub fn pending_line(request: i64, who: &str, reason: &str, entry: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "Permintaan {request} dari {who} ({reason}):\n{entry}\n/approve {request} · /reject {request}"
        ),
        Lang::Sw => format!(
            "Ombi {request} kutoka kwa {who} ({reason}):\n{entry}\n/approve {request} · /reject {request}"
        ),
        Lang::En => format!(
            "Request {request} from {who} ({reason}):\n{entry}\n/approve {request} · /reject {request}"
        ),
    }
}

pub fn no_such_request(number: i64, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!("Tidak ada permintaan {number} yang menunggu. Lihat /pending."),
        Lang::Sw => format!("Hakuna ombi {number} linalosubiri. Angalia /pending."),
        Lang::En => format!("There's no waiting request {number}. See /pending."),
    }
}

pub fn decided(request: i64, entry: i64, approved: bool, language: &str) -> String {
    match (approved, lang(language)) {
        (true, Lang::Id) => format!("Disetujui: #{entry} dibatalkan (permintaan {request})."),
        (false, Lang::Id) => format!("Ditolak: #{entry} tetap dihitung (permintaan {request})."),
        (true, Lang::Sw) => format!("Imekubaliwa: #{entry} imefutwa (ombi {request})."),
        (false, Lang::Sw) => format!("Imekataliwa: #{entry} bado inahesabiwa (ombi {request})."),
        (true, Lang::En) => format!("Approved: #{entry} is voided (request {request})."),
        (false, Lang::En) => format!("Rejected: #{entry} still counts (request {request})."),
    }
}

pub fn decision_notice(entry: i64, approved: bool, by: &str, language: &str) -> String {
    match (approved, lang(language)) {
        (true, Lang::Id) => format!("{by} menyetujui: #{entry} sudah dibatalkan."),
        (false, Lang::Id) => {
            format!("{by} menolak membatalkan #{entry}; catatan itu tetap dihitung.")
        }
        (true, Lang::Sw) => format!("{by} amekubali: #{entry} imefutwa."),
        (false, Lang::Sw) => format!("{by} amekataa kufuta #{entry}; bado inahesabiwa."),
        (true, Lang::En) => format!("{by} approved: #{entry} is voided."),
        (false, Lang::En) => format!("{by} said no to voiding #{entry}; it still counts."),
    }
}

pub fn longer_reports_not_allowed(language: &str) -> &'static str {
    pick(
        language,
        "Week and month reports are for the owner and managers. /report shows today.",
        "Laporan mingguan dan bulanan hanya untuk pemilik dan manajer. /report menampilkan hari ini.",
        "Ripoti za wiki na mwezi ni za mwenye duka na mameneja tu. /report inaonyesha ya leo.",
    )
}

pub fn retry_not_allowed(language: &str) -> &'static str {
    match lang(language) {
        Lang::Id => "Hanya pemilik atau manajer yang bisa mengirim ulang catatan ke Friday.",
        Lang::Sw => "Mwenye duka au meneja pekee ndiye anaweza kutuma rekodi tena kwa Friday.",
        Lang::En => "Only the owner or a manager can send an entry to Friday again.",
    }
}

pub fn retry_result(number: i64, queued: bool, language: &str) -> String {
    match (lang(language), queued) {
        (Lang::Id, true) => format!("Catatan #{number} dikirim lagi ke Friday dalam 30 detik."),
        (Lang::Id, false) => {
            format!("Catatan #{number} tidak tertahan; tidak ada yang perlu dikirim ulang.")
        }
        (Lang::Sw, true) => {
            format!("Rekodi #{number} itatumwa tena kwa Friday ndani ya sekunde 30.")
        }
        (Lang::Sw, false) => format!("Rekodi #{number} haijazuiwa; hakuna cha kutuma tena."),
        (Lang::En, true) => format!("Entry #{number} goes to Friday again within 30 seconds."),
        (Lang::En, false) => format!("Entry #{number} isn't stopped; nothing to send again."),
    }
}

pub fn not_in_friday(why: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(" [belum di Friday: {why}]"),
        Lang::Sw => format!(" [haiko kwenye Friday: {why}]"),
        Lang::En => format!(" [not in Friday: {why}]"),
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
    match (known, lang(language)) {
        (false, Lang::Id) => format!("Belum ada stok {item} yang tercatat."),
        (false, Lang::Sw) => format!("Bado hakuna stoki ya {item} iliyorekodiwa."),
        (false, Lang::En) => format!("No stock of {item} has been recorded yet."),
        (true, Lang::Id) => format!("Sisa {item}: {}{unit}.", qty(left)),
        (true, Lang::Sw) => format!("{item} zilizobaki: {}{unit}.", qty(left)),
        (true, Lang::En) => format!("{item}: {}{unit} left.", qty(left)),
    }
}

fn period(window: guss_core::store::TimeWindow, language: &str) -> &'static str {
    use guss_core::store::TimeWindow::*;
    match window {
        Day => pick(language, "today", "hari ini", "leo"),
        Week => pick(
            language,
            "in the last 7 days",
            "7 hari terakhir",
            "katika siku 7 zilizopita",
        ),
        Month => pick(
            language,
            "in the last 30 days",
            "30 hari terakhir",
            "katika siku 30 zilizopita",
        ),
    }
}

pub(crate) fn totals_about(
    about: &str,
    window: guss_core::store::TimeWindow,
    t: &crate::orchestrator::AboutTotals,
    language: &str,
) -> String {
    let when = period(window, language);
    let l = lang(language);
    if t.entries == 0 {
        return match l {
            Lang::Id => format!("Tidak ada catatan untuk {about} {when}."),
            Lang::Sw => format!("Hakuna kilichorekodiwa kwa {about} {when}."),
            Lang::En => format!("Nothing recorded for {about} {when}."),
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
        parts.push(match l {
            Lang::Id => format!("terjual {} seharga {}", qty(t.sold), list(&t.money_in)),
            Lang::Sw => format!("ziliuzwa {} kwa {}", qty(t.sold), list(&t.money_in)),
            Lang::En => format!("sold {} for {}", qty(t.sold), list(&t.money_in)),
        });
    }
    if !t.money_out.is_empty() {
        parts.push(match l {
            Lang::Id => format!("dibayar {}", list(&t.money_out)),
            Lang::Sw => format!("zililipwa {}", list(&t.money_out)),
            Lang::En => format!("paid {}", list(&t.money_out)),
        });
    }
    let parts = parts.join(", ");
    match l {
        Lang::Id => format!("{about} {when}: {parts} ({} catatan).", t.entries),
        Lang::Sw => format!("{about} {when}: {parts} (rekodi {}).", t.entries),
        Lang::En => format!("{about} {when}: {parts} ({} entries).", t.entries),
    }
}

pub fn no_such_line(n: usize, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!("Tidak ada baris {n} untuk dibatalkan."),
        Lang::Sw => format!("Hakuna mstari {n} wa kutendua."),
        Lang::En => format!("There's no line {n} to undo."),
    }
}

pub fn lines_undone(lines: &[usize], language: &str) -> String {
    let list = lines
        .iter()
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    match (lines.len() > 1, lang(language)) {
        (_, Lang::Id) => format!("Baris {list} dibatalkan. Sisanya tetap dihitung."),
        (false, Lang::Sw) => format!("Mstari {list} umetenduliwa. Mingine bado inahesabiwa."),
        (true, Lang::Sw) => format!("Mistari {list} imetenduliwa. Mingine bado inahesabiwa."),
        (false, Lang::En) => format!("Line {list} undone. The rest still counts."),
        (true, Lang::En) => format!("Lines {list} undone. The rest still counts."),
    }
}

pub fn duplicate_question(line: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "Sepertinya ini sudah dicatat:\n{line}\nBelum ada yang disimpan. Balas \"ya\" untuk tetap \
             menyimpan, atau \"batal\" jika itu penjualan yang sama."
        ),
        Lang::Sw => format!(
            "Inaonekana hii ilisharekodiwa:\n{line}\nBado hakuna kilichohifadhiwa. Jibu \"ndiyo\" \
             kuihifadhi tena, au \"hapana\" kama ni mauzo yale yale."
        ),
        Lang::En => format!(
            "Looks like this was already recorded:\n{line}\nNothing is saved yet. Reply \"yes\" to save \
             it again, or \"no\" if it's the same sale."
        ),
    }
}

pub fn duplicate_dropped(language: &str) -> String {
    pick(
        language,
        "(The possible double entry was not saved.)",
        "(Catatan ganda sebelumnya tidak disimpan.)",
        "(Rekodi iliyoonekana kurudiwa haikuhifadhiwa.)",
    )
    .to_string()
}

pub fn too_far_back(max: u32, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "Saya bisa mencatat sampai {max} hari ke belakang. Tidak ada yang disimpan; untuk yang \
             lebih lama, minta pemilik mencatatnya dengan catatan."
        ),
        Lang::Sw => format!(
            "Naweza kurekodi hadi siku {max} zilizopita. Hakuna kilichohifadhiwa; kwa za zamani \
             zaidi, mwombe mwenye duka azirekodi pamoja na maelezo."
        ),
        Lang::En => format!(
            "I can record up to {max} days back. Nothing was saved; for anything older, ask the \
             owner to record it with a note."
        ),
    }
}

pub fn outlier_question(item: &str, price: &str, usual: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "{item} biasanya terjual {usual}. Tetap simpan {price} per satuan? Belum ada yang \
             disimpan. Balas \"ya\" untuk menyimpan, atau kirim lagi dengan harga yang benar."
        ),
        Lang::Sw => format!(
            "{item} kwa kawaida huuzwa {usual}. Uhifadhi {price} kila moja? Bado hakuna \
             kilichohifadhiwa. Jibu \"ndiyo\" kuhifadhi, au tuma tena na bei sahihi."
        ),
        Lang::En => format!(
            "{item} usually sells at {usual}. Keep {price} each? Nothing is saved yet. Reply \
             \"yes\" to keep it, or send it again with the right price."
        ),
    }
}

pub fn outlier_dropped(language: &str) -> String {
    pick(
        language,
        "(The unusual price was not saved.)",
        "(Harga yang tidak biasa tadi tidak disimpan.)",
        "(Bei isiyo ya kawaida haikuhifadhiwa.)",
    )
    .to_string()
}

/// "returned 1 rice" with no earlier rice sale to give back.
pub fn nothing_to_return(item: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => {
            format!("Belum ada penjualan {item} yang bisa dikembalikan. Tidak ada yang disimpan.")
        }
        Lang::Sw => format!("Sioni mauzo ya {item} ya kurudisha. Hakuna kilichohifadhiwa."),
        Lang::En => format!("I can't find a sale of {item} to take back. Nothing was saved."),
    }
}

/// "how much does John owe?"
pub fn owed_one(customer: &str, amount: Option<String>, language: &str) -> String {
    match (amount, lang(language)) {
        (Some(a), Lang::Id) => format!("Utang {customer}: {a}."),
        (Some(a), Lang::Sw) => format!("{customer} anadaiwa {a}."),
        (Some(a), Lang::En) => format!("{customer} owes {a}."),
        (None, Lang::Id) => format!("{customer} tidak punya utang."),
        (None, Lang::Sw) => format!("{customer} hadaiwi chochote."),
        (None, Lang::En) => format!("{customer} doesn't owe anything."),
    }
}

/// "who owes me?"
pub fn owed_all(rows: &[(String, String)], language: &str) -> String {
    if rows.is_empty() {
        return pick(
            language,
            "Nobody owes you anything.",
            "Tidak ada yang punya utang.",
            "Hakuna anayedaiwa chochote.",
        )
        .into();
    }
    let lines: Vec<String> = rows.iter().map(|(n, a)| format!("- {n}: {a}")).collect();
    let title = pick(language, "Owed to you:", "Yang punya utang:", "Wanaodaiwa:");
    format!("{title}\n{}", lines.join("\n"))
}

/// "returned 1 sugar" when the sugar went on credit and isn't paid.
pub fn return_of_credit_sale(item: &str, customer: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "{item} itu dijual utang ke {customer}. Batalkan penjualannya dengan /void lalu catat ulang yang benar. Tidak ada yang disimpan."
        ),
        Lang::Sw => format!(
            "{item} hiyo ilichukuliwa kwa deni na {customer}. Futa mauzo hayo (/void) kisha urekodi alichobaki nacho. Hakuna kilichohifadhiwa."
        ),
        Lang::En => format!(
            "That {item} went on credit to {customer}. Void that sale (/void) and record what they kept instead. Nothing was saved."
        ),
    }
}

/// A team leader asked to decide for someone outside their team, or above
/// their limit.
pub fn team_leader_cannot(limit: &str, language: &str) -> String {
    match lang(language) {
        Lang::Id => format!(
            "Ketua tim hanya memutuskan permintaan dari timnya sendiri, sampai {limit} per catatan. Pemilik atau manajer yang memutuskan ini."
        ),
        Lang::Sw => format!(
            "Kiongozi wa timu huamua maombi ya timu yake tu, hadi {limit} kwa kila rekodi. Mwenye duka au meneja ataamua hili."
        ),
        Lang::En => format!(
            "Team leaders decide requests from their own team, up to {limit} per entry. The owner or a manager decides this one."
        ),
    }
}
