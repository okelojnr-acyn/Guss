//! What people see in Telegram: the bot's profile (description shown in
//! an empty chat, short description, command menu), the /start welcome,
//! and who may use the bot at all. Every user-facing string exists in
//! `en` and `id`.

use guss_core::people::Role;
use teloxide::prelude::*;
use teloxide::types::BotCommand;

/// Who may talk to the bot. A Telegram bot is public by username, and
/// every message writes to the shop's books, so once an owner or any
/// allowed id is configured everyone else is refused.
#[derive(Debug, Clone, Default)]
pub struct TelegramAccess {
    pub owner_chat_id: Option<i64>,
    /// Can approve voids and see longer reports (Friday's chain of command).
    pub manager_user_ids: Vec<i64>,
    /// Staff: record entries and see today's report.
    pub allowed_user_ids: Vec<i64>,
}

impl TelegramAccess {
    pub fn is_open(&self) -> bool {
        self.owner_chat_id.is_none()
            && self.manager_user_ids.is_empty()
            && self.allowed_user_ids.is_empty()
    }

    /// A user's role, or None if they may not use the bot. An open bot
    /// (nothing configured yet) treats everyone as the owner: that's the
    /// one person setting it up.
    pub fn role_of(&self, user_id: i64) -> Option<Role> {
        if self.is_open() || self.owner_chat_id == Some(user_id) {
            Some(Role::Owner)
        } else if self.manager_user_ids.contains(&user_id) {
            Some(Role::Manager)
        } else if self.allowed_user_ids.contains(&user_id) {
            Some(Role::Staff)
        } else {
            None
        }
    }

    pub fn allows(&self, user_id: i64) -> bool {
        self.role_of(user_id).is_some()
    }

    /// Chats that hear about void requests: the owner and managers.
    pub fn approver_chats(&self) -> Vec<i64> {
        self.owner_chat_id
            .into_iter()
            .chain(self.manager_user_ids.iter().copied())
            .collect()
    }

    /// Parses "123, 456" (the env form); returns the ids and any entries
    /// that weren't numbers.
    pub fn parse_ids(raw: &str) -> (Vec<i64>, Vec<String>) {
        let mut ids = Vec::new();
        let mut bad = Vec::new();
        for part in raw
            .split([',', ' ', ';'])
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            match part.parse::<i64>() {
                Ok(id) => ids.push(id),
                Err(_) => bad.push(part.to_string()),
            }
        }
        (ids, bad)
    }
}

/// A slash command, with any "@botname" suffix stripped.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Help,
    Report(Option<String>),
    Undo,
    MyId,
    History,
    /// "/void 12 double entry": the entry number and the reason.
    Void(Option<String>),
    Pending,
    Approve(Option<String>),
    Reject(Option<String>),
    /// Link this Telegram account to the person's Friday account.
    Link,
    /// "/paylink John": a Paystack link for everything John owes.
    PayLink(Option<String>),
    /// "/retry 12": send entry 12 to Friday again (it was stopped).
    Retry(Option<String>),
    Unknown,
}

pub fn parse_command(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let word = parts.next()?.split('@').next()?.to_lowercase();
    let arg = parts
        .next()
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty());
    Some(match word.as_str() {
        "start" => Command::Start,
        "help" | "bantuan" => Command::Help,
        "report" | "laporan" => Command::Report(arg),
        "undo" | "batal" => Command::Undo,
        "myid" | "idsaya" => Command::MyId,
        "history" | "riwayat" => Command::History,
        "void" | "hapus" => Command::Void(arg),
        "pending" | "menunggu" => Command::Pending,
        "approve" | "setujui" => Command::Approve(arg),
        "reject" | "tolak" => Command::Reject(arg),
        "link" | "tautkan" => Command::Link,
        "paylink" | "tautanbayar" | "kiungo" => Command::PayLink(arg),
        "retry" | "kirimulang" | "tumatena" => Command::Retry(arg),
        _ => Command::Unknown,
    })
}

/// "today"/"week"/"month" (and Indonesian, Swahili) -> report window; None = today.
pub fn report_window(arg: Option<&str>) -> Option<guss_core::store::TimeWindow> {
    use guss_core::store::TimeWindow;
    let arg = arg.map(str::to_lowercase);
    match arg.as_deref() {
        None | Some("today" | "day" | "hari" | "hariini" | "leo") => Some(TimeWindow::Day),
        Some("week" | "minggu" | "wiki") => Some(TimeWindow::Week),
        Some("month" | "bulan" | "mwezi") => Some(TimeWindow::Month),
        Some(_) => None,
    }
}

/// "12 double entry" -> (12, "double entry"); "#12" works too.
pub fn number_and_rest(arg: Option<&str>) -> Option<(i64, String)> {
    let arg = arg?.trim();
    let mut parts = arg.splitn(2, char::is_whitespace);
    let number = parts.next()?.trim_start_matches('#').parse().ok()?;
    Some((number, parts.next().unwrap_or("").trim().to_string()))
}

pub fn link_code(language: &str, code: &str) -> String {
    if is_id(language) {
        format!(
            "Kode Anda: {code}\n\nBuka Friday: Aplikasi & integrasi, Guss, Tautkan Telegram, lalu \
             masukkan kode ini dalam 30 menit. Setelah itu peran Anda di Friday berlaku di sini."
        )
    } else if is_sw(language) {
        format!(
            "Nambari yako: {code}\n\nFungua Friday: Programu na miunganisho, Guss, Unganisha Telegram, \
             kisha uiweke ndani ya dakika 30. Baada ya hapo jukumu lako la Friday litatumika hapa."
        )
    } else {
        format!(
            "Your code: {code}\n\nOpen Friday: Apps & integrations, Guss, Link your Telegram, and \
             enter it within 30 minutes. After that your Friday role applies here."
        )
    }
}

pub fn link_unavailable(language: &str) -> String {
    if is_id(language) {
        "Guss ini belum terhubung ke Friday.".to_string()
    } else if is_sw(language) {
        "Guss hii haijaunganishwa na Friday.".to_string()
    } else {
        "This Guss isn't connected to Friday.".to_string()
    }
}

pub fn link_failed(language: &str) -> String {
    if is_id(language) {
        "Tidak bisa mengambil kode dari Friday sekarang. Coba lagi sebentar lagi.".to_string()
    } else if is_sw(language) {
        "Imeshindikana kupata nambari kutoka Friday sasa hivi. Jaribu tena baada ya dakika moja."
            .to_string()
    } else {
        "Couldn't get a code from Friday right now. Try again in a minute.".to_string()
    }
}

/// Refusal when Friday decides who may use Guss and this person isn't
/// linked (or is a viewer there).
pub fn link_first(language: &str, business: &str) -> String {
    if is_id(language) {
        format!(
            "Halo! Ini asisten toko {business}. Kirim /link untuk menautkan akun Friday Anda dulu."
        )
    } else if is_sw(language) {
        format!(
            "Habari! Huyu ni msaidizi wa duka la {business}. Tuma /link kwanza kuunganisha akaunti yako ya Friday."
        )
    } else {
        format!(
            "Hi! This is {business}'s shop assistant. Send /link to connect your Friday account first."
        )
    }
}

pub fn role_cant_use_guss(language: &str) -> String {
    if is_id(language) {
        "Akun Telegram Anda sudah tertaut, tetapi peran Anda di Friday (hanya lihat) tidak bisa memakai Guss. \
         Minta pemilik mengubah peran Anda di Friday."
            .into()
    } else if is_sw(language) {
        "Telegram yako imeunganishwa, lakini jukumu lako la Friday (kuangalia tu) haliwezi kutumia Guss. \
         Mwombe mwenye duka abadilishe jukumu lako kwenye Friday."
            .into()
    } else {
        "Your Telegram is linked, but your Friday role (view only) can't use Guss. \
         Ask the owner to change your role in Friday."
            .into()
    }
}

pub fn retry_usage(language: &str) -> String {
    if is_id(language) {
        "Tulis nomor catatan: /retry 12. Nomor ada di /history.".into()
    } else if is_sw(language) {
        "Andika nambari ya rekodi: /retry 12. Nambari ziko kwenye /history.".into()
    } else {
        "Say which entry: /retry 12. The numbers are in /history.".into()
    }
}

pub fn void_usage(language: &str) -> String {
    if is_id(language) {
        "Tulis nomor catatan dan alasannya, misalnya: /void 12 tercatat dua kali. Nomor ada di /history."
            .to_string()
    } else if is_sw(language) {
        "Andika nambari ya rekodi na sababu, kwa mfano: /void 12 imerekodiwa mara mbili. Nambari ziko kwenye /history."
            .to_string()
    } else {
        "Give the entry number and why, e.g. /void 12 recorded twice. Numbers are in /history."
            .to_string()
    }
}

pub fn decide_usage(language: &str) -> String {
    if is_id(language) {
        "Tulis nomor permintaan, misalnya /approve 3. Lihat /pending.".to_string()
    } else if is_sw(language) {
        "Andika nambari ya ombi, kwa mfano /approve 3. Angalia /pending.".to_string()
    } else {
        "Give the request number, e.g. /approve 3. See /pending.".to_string()
    }
}

fn is_id(language: &str) -> bool {
    language == "id"
}

fn is_sw(language: &str) -> bool {
    language == "sw"
}

pub fn welcome(language: &str, business: &str, first_name: &str) -> String {
    if is_id(language) {
        format!(
            "Halo {first_name}! Saya asisten toko {business}.\n\n{}",
            help(language)
        )
    } else if is_sw(language) {
        format!(
            "Habari {first_name}! Mimi ni msaidizi wa duka la {business}.\n\n{}",
            help(language)
        )
    } else {
        format!(
            "Hi {first_name}! I'm {business}'s shop assistant.\n\n{}",
            help(language)
        )
    }
}

pub fn paylink_usage(language: &str) -> String {
    if is_id(language) {
        "Tulis nama pelanggan: /paylink Budi".into()
    } else if is_sw(language) {
        "Andika jina la mteja: /paylink Juma".into()
    } else {
        "Say whose: /paylink John".into()
    }
}

/// The reply to "/paylink John": the link to forward.
pub fn paylink_sent(language: &str, customer: &str, owed: &str, url: &str) -> String {
    if is_id(language) {
        format!(
            "Tautan bayar untuk {customer} ({owed}):\n{url}\n\nTeruskan ke dia. Kartu, M-Pesa atau \
             bank lewat Paystack. Begitu dia bayar, saya catat."
        )
    } else if is_sw(language) {
        format!(
            "Kiungo cha malipo cha {customer} ({owed}):\n{url}\n\nMtumie. Kadi, M-Pesa au benki \
             kupitia Paystack. Akilipa, nitarekodi."
        )
    } else {
        format!(
            "Pay link for {customer} ({owed}):\n{url}\n\nForward it to them. Card, M-Pesa or bank \
             through Paystack. When they pay, I'll record it."
        )
    }
}

/// "/paylink" when Friday couldn't make one; `why` is Friday's reason.
pub fn paylink_failed(language: &str, why: &str) -> String {
    if is_id(language) {
        format!("Tidak bisa membuat tautan bayar: {why}")
    } else if is_sw(language) {
        format!("Sikuweza kutengeneza kiungo cha malipo: {why}")
    } else {
        format!("Couldn't make a pay link: {why}")
    }
}

pub fn help(language: &str) -> String {
    if is_id(language) {
        "Cukup ceritakan apa yang terjadi, seperti mengirim pesan ke rekan kerja:\n\
         • terjual 3 karung beras @120\n\
         • bayar 500 untuk transport\n\
         • stok masuk 20 soda\n\n\
         Saya akan mengonfirmasi yang saya catat. Salah? Balas \"batal\" atau /undo dalam 5 menit.\n\n\
         /report menampilkan penjualan hari ini (/report minggu, /report bulan).\n\n\
         /history menampilkan catatan terakhir dengan nomornya. Catatan yang salah setelah 5 menit: \
         /void 12 alasannya. Tidak ada yang dihapus; pemilik atau manajer menyetujui.\n\n\
         /paylink Budi memberi tautan Paystack untuk utang Budi, untuk diteruskan. \
         Begitu dia bayar, saya catat."
            .to_string()
    } else if is_sw(language) {
        "Niambie tu kilichotokea, kama unavyomtumia mwenzako ujumbe:\n\
         • nimeuza mchele mifuko 3 kwa 120\n\
         • nimelipa 500 nauli\n\
         • soda 20 zimeingia\n\n\
         Nitathibitisha nilichorekodi. Kosa? Jibu \"hapana\" au /undo ndani ya dakika 5.\n\n\
         /report inaonyesha mauzo ya leo (/report wiki, /report mwezi).\n\n\
         /history inaonyesha rekodi za mwisho na nambari zake. Rekodi yenye kosa baada ya dakika 5: \
         /void 12 sababu. Hakuna kinachofutwa kabisa; mwenye duka au meneja anaidhinisha.\n\n\
         /paylink Juma inakupa kiungo cha Paystack cha deni la Juma, umtumie. \
         Akilipa, nitarekodi."
            .to_string()
    } else {
        "Just tell me what happened, the way you'd text a colleague:\n\
         • sold 3 bags of rice at 120\n\
         • paid 500 for transport\n\
         • restocked 20 sodas\n\n\
         I'll confirm what I recorded. A mistake? Reply \"no\" or /undo within 5 minutes.\n\n\
         /report shows today's sales (/report week, /report month).\n\n\
         /history lists the latest entries with their numbers. A wrong entry after 5 minutes: \
         /void 12 the reason. Nothing is deleted; the owner or a manager approves.\n\n\
         /paylink John gives a Paystack link for what John owes, to forward to him. \
         When he pays, I record it."
            .to_string()
    }
}

pub fn private_bot(language: &str, business: &str, user_id: i64) -> String {
    if is_id(language) {
        format!(
            "Halo! Ini asisten toko pribadi {business}, jadi hanya timnya yang bisa memakainya.\n\n\
             ID Telegram Anda: {user_id}\n\
             Jika Anda bekerja di sini, kirim ID ini ke pemilik agar Anda ditambahkan."
        )
    } else if is_sw(language) {
        format!(
            "Habari! Huyu ni msaidizi binafsi wa duka la {business}, kwa hiyo ni timu yake tu inayoweza kumtumia.\n\n\
             Kitambulisho chako cha Telegram: {user_id}\n\
             Kama unafanya kazi hapa, mtumie mwenye duka kitambulisho hiki akuongeze."
        )
    } else {
        format!(
            "Hi! This is {business}'s private shop assistant, so only its team can use it.\n\n\
             Your Telegram ID: {user_id}\n\
             If you work here, send this ID to the owner so they can add you."
        )
    }
}

pub fn your_id(language: &str, user_id: i64) -> String {
    if is_id(language) {
        format!("ID Telegram Anda: {user_id}")
    } else if is_sw(language) {
        format!("Kitambulisho chako cha Telegram: {user_id}")
    } else {
        format!("Your Telegram ID: {user_id}")
    }
}

pub fn private_chats_only(language: &str) -> String {
    if is_id(language) {
        "Saya hanya bekerja di chat pribadi. Kirim pesan langsung ke saya.".to_string()
    } else if is_sw(language) {
        "Nafanya kazi kwenye mazungumzo ya faragha tu. Nitumie ujumbe moja kwa moja.".to_string()
    } else {
        "I only work in a private chat. Message me directly.".to_string()
    }
}

pub fn unknown_command(language: &str) -> String {
    if is_id(language) {
        "Perintah itu tidak saya kenal. Coba /help.".to_string()
    } else if is_sw(language) {
        "Sijui amri hiyo. Jaribu /help.".to_string()
    } else {
        "I don't know that command. Try /help.".to_string()
    }
}

pub fn bad_report_window(language: &str) -> String {
    if is_id(language) {
        "Pakai /report, /report minggu, atau /report bulan.".to_string()
    } else if is_sw(language) {
        "Tumia /report, /report wiki, au /report mwezi.".to_string()
    } else {
        "Use /report, /report week or /report month.".to_string()
    }
}

fn description(language: &str, business: &str) -> String {
    if is_id(language) {
        format!(
            "Asisten toko {business}. Ceritakan apa yang terjadi dengan kata-kata biasa, \
             misalnya \"terjual 3 karung beras @120\" atau \"bayar 500 untuk transport\", \
             dan saya mencatat pembukuannya. Minta /report kapan saja. Hanya tim toko yang bisa memakai saya."
        )
    } else if is_sw(language) {
        format!(
            "Msaidizi wa duka la {business}. Niambie kilichotokea kwa maneno ya kawaida, \
             kama \"nimeuza mchele mifuko 3 kwa 120\" au \"nimelipa 500 nauli\", \
             nami ninatunza vitabu. Omba /report wakati wowote. Ni timu ya duka tu inayoweza kunitumia."
        )
    } else {
        format!(
            "{business}'s shop assistant. Tell me what happened in plain words, like \
             \"sold 3 bags of rice at 120\" or \"paid 500 for transport\", and I keep the books. \
             Ask for /report any time. Only the shop's team can use me."
        )
    }
}

fn short_description(language: &str, business: &str) -> String {
    let s = if is_id(language) {
        format!("Asisten toko {business}: catat penjualan, stok, dan pengeluaran lewat chat.")
    } else if is_sw(language) {
        format!("Msaidizi wa duka la {business}: rekodi mauzo, stoki na matumizi kwa ujumbe.")
    } else {
        format!("{business}'s shop assistant: record sales, stock and expenses by chat.")
    };
    s.chars().take(120).collect()
}

/// The menu for staff: no approvals (plan 12.7, menus by role).
fn staff_commands(language: &str) -> Vec<BotCommand> {
    commands(language)
        .into_iter()
        .filter(|c| c.command != "pending")
        .collect()
}

/// The menu for owners and managers: everything, plus /retry.
fn manager_commands(language: &str) -> Vec<BotCommand> {
    let mut cmds = commands(language);
    let text = if is_id(language) {
        "Kirim ulang catatan ke Friday: /retry 12"
    } else if is_sw(language) {
        "Tuma rekodi tena kwa Friday: /retry 12"
    } else {
        "Send an entry to Friday again: /retry 12"
    };
    cmds.push(BotCommand::new("retry", text));
    cmds
}

/// Gives an owner's or manager's own chat their fuller menu. Logged, never
/// fatal: commands are checked by role whatever the menu shows.
pub async fn apply_manager_menu(bot: &Bot, chat_id: ChatId, language: &str) {
    let res = bot
        .set_my_commands(manager_commands(language))
        .scope(teloxide::types::BotCommandScope::Chat {
            chat_id: teloxide::types::Recipient::Id(chat_id),
        })
        .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "couldn't set a manager's command menu");
    }
}

fn commands(language: &str) -> Vec<BotCommand> {
    let sw_of = |cmd: &str| match cmd {
        "report" => "Ripoti ya mauzo: leo, wiki au mwezi",
        "undo" => "Tendua rekodi ya mwisho",
        "history" => "Rekodi za mwisho na nambari zake",
        "void" => "Futa rekodi: /void 12 sababu",
        "pending" => "Maombi ya kufuta ya kuidhinisha",
        "paylink" => "Kiungo cha malipo: /paylink Juma",
        "help" => "Jinsi ya kunitumia",
        _ => "Onyesha kitambulisho chako cha Telegram",
    };
    let c = |cmd: &str, en: &str, id: &str| {
        let text = if is_id(language) {
            id
        } else if is_sw(language) {
            sw_of(cmd)
        } else {
            en
        };
        BotCommand::new(cmd, text)
    };
    vec![
        c(
            "report",
            "Sales report: today, week or month",
            "Laporan penjualan: hari ini, minggu, bulan",
        ),
        c(
            "undo",
            "Take back the last entry",
            "Batalkan catatan terakhir",
        ),
        c(
            "history",
            "Latest entries with their numbers",
            "Catatan terakhir dengan nomornya",
        ),
        c(
            "void",
            "Stop an entry counting: /void 12 reason",
            "Batalkan catatan: /void 12 alasan",
        ),
        c(
            "pending",
            "Void requests to approve",
            "Permintaan pembatalan untuk disetujui",
        ),
        c(
            "paylink",
            "A pay link for what a customer owes: /paylink John",
            "Tautan bayar untuk utang pelanggan: /paylink Budi",
        ),
        c("help", "How to use me", "Cara memakai saya"),
        c(
            "myid",
            "Show your Telegram ID",
            "Tampilkan ID Telegram Anda",
        ),
    ]
}

/// Sets the bot's description, short description and command menu, in
/// both languages (Telegram picks by the user's app language), with the
/// business's own language as the default for everyone else. Failures
/// are logged, never fatal: the bot works without its profile.
pub async fn apply_profile(bot: &Bot, business: &str, business_language: &str) {
    let mut ok = true;
    for (lang, code) in [
        (business_language, ""),
        ("en", "en"),
        ("id", "id"),
        ("sw", "sw"),
    ] {
        let mut desc = bot
            .set_my_description()
            .description(description(lang, business));
        let mut short = bot
            .set_my_short_description()
            .short_description(short_description(lang, business));
        // Everyone's menu is the staff one; owners and managers get theirs
        // per chat (apply_manager_menu).
        let mut cmds = bot.set_my_commands(staff_commands(lang));
        if !code.is_empty() {
            desc = desc.language_code(code);
            short = short.language_code(code);
            cmds = cmds.language_code(code);
        }
        for result in [desc.await.err(), short.await.err(), cmds.await.err()]
            .into_iter()
            .flatten()
        {
            ok = false;
            tracing::warn!(error = %result, language = lang, "couldn't set the Telegram bot profile");
        }
    }
    if ok {
        tracing::info!("Telegram bot profile set (description, about text, command menu)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_rules() {
        let open = TelegramAccess::default();
        assert!(open.is_open() && open.allows(1));
        let owned = TelegramAccess {
            manager_user_ids: vec![],
            owner_chat_id: Some(10),
            allowed_user_ids: vec![20],
        };
        assert!(owned.allows(10) && owned.allows(20) && !owned.allows(30));
        let (ids, bad) = TelegramAccess::parse_ids(" 20, 30;x  40 ");
        assert_eq!(ids, vec![20, 30, 40]);
        assert_eq!(bad, vec!["x".to_string()]);
    }

    #[test]
    fn roles_follow_the_configured_ids() {
        let access = TelegramAccess {
            owner_chat_id: Some(10),
            manager_user_ids: vec![15],
            allowed_user_ids: vec![20],
        };
        assert_eq!(access.role_of(10), Some(Role::Owner));
        assert_eq!(access.role_of(15), Some(Role::Manager));
        assert_eq!(access.role_of(20), Some(Role::Staff));
        assert_eq!(access.role_of(30), None);
        assert_eq!(access.approver_chats(), vec![10, 15]);
        assert_eq!(TelegramAccess::default().role_of(99), Some(Role::Owner));
    }

    #[test]
    fn journal_commands_parse() {
        assert_eq!(
            parse_command("/void 12 Recorded Twice"),
            Some(Command::Void(Some("12 Recorded Twice".into())))
        );
        assert_eq!(parse_command("/history"), Some(Command::History));
        assert_eq!(
            parse_command("/setujui 3"),
            Some(Command::Approve(Some("3".into())))
        );
        assert_eq!(
            number_and_rest(Some("#12 recorded twice")),
            Some((12, "recorded twice".into()))
        );
        assert_eq!(number_and_rest(Some("12")), Some((12, String::new())));
        assert_eq!(number_and_rest(Some("twelve")), None);
        assert_eq!(number_and_rest(None), None);
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse_command("/start"), Some(Command::Start));
        assert_eq!(
            parse_command("/report@shop_bot week"),
            Some(Command::Report(Some("week".into())))
        );
        assert_eq!(parse_command("/laporan"), Some(Command::Report(None)));
        assert_eq!(parse_command("/batal"), Some(Command::Undo));
        assert_eq!(parse_command("/whatever"), Some(Command::Unknown));
        assert_eq!(parse_command("sold 3 rice"), None);
        assert!(report_window(Some("bulan")).is_some());
        assert!(report_window(Some("year")).is_none());
    }

    #[test]
    fn texts_are_bilingual_and_fit_limits() {
        for lang in ["en", "id", "sw"] {
            assert!(description(lang, "Demo Shop").chars().count() <= 512);
            assert!(short_description(lang, &"x".repeat(200)).chars().count() <= 120);
            assert!(welcome(lang, "Demo Shop", "Ann").contains("Demo Shop"));
            assert!(private_bot(lang, "Demo Shop", 42).contains("42"));
        }
        assert_ne!(help("en"), help("id"));
        assert_ne!(help("en"), help("sw"));
        assert!(report_window(Some("wiki")).is_some());
    }
}
