//! What people see in Telegram: the bot's profile (description shown in
//! an empty chat, short description, command menu), the /start welcome,
//! and who may use the bot at all. Every user-facing string exists in
//! `en` and `id`.

use teloxide::prelude::*;
use teloxide::types::BotCommand;

/// Who may talk to the bot. A Telegram bot is public by username, and
/// every message writes to the shop's books, so once an owner or any
/// allowed id is configured everyone else is refused.
#[derive(Debug, Clone, Default)]
pub struct TelegramAccess {
    pub owner_chat_id: Option<i64>,
    pub allowed_user_ids: Vec<i64>,
}

impl TelegramAccess {
    pub fn is_open(&self) -> bool {
        self.owner_chat_id.is_none() && self.allowed_user_ids.is_empty()
    }

    pub fn allows(&self, user_id: i64) -> bool {
        self.is_open()
            || self.owner_chat_id == Some(user_id)
            || self.allowed_user_ids.contains(&user_id)
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
    Unknown,
}

pub fn parse_command(text: &str) -> Option<Command> {
    let text = text.trim();
    let rest = text.strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let word = parts.next()?.split('@').next()?.to_lowercase();
    let arg = parts
        .next()
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty());
    Some(match word.as_str() {
        "start" => Command::Start,
        "help" | "bantuan" => Command::Help,
        "report" | "laporan" => Command::Report(arg),
        "undo" | "batal" => Command::Undo,
        "myid" | "idsaya" => Command::MyId,
        _ => Command::Unknown,
    })
}

/// "today"/"week"/"month" (and Indonesian) -> report window; None = today.
pub fn report_window(arg: Option<&str>) -> Option<guss_core::store::TimeWindow> {
    use guss_core::store::TimeWindow;
    match arg {
        None | Some("today" | "day" | "hari" | "hariini") => Some(TimeWindow::Day),
        Some("week" | "minggu") => Some(TimeWindow::Week),
        Some("month" | "bulan") => Some(TimeWindow::Month),
        Some(_) => None,
    }
}

fn is_id(language: &str) -> bool {
    language == "id"
}

pub fn welcome(language: &str, business: &str, first_name: &str) -> String {
    if is_id(language) {
        format!(
            "Halo {first_name}! Saya asisten toko {business}.\n\n{}",
            help(language)
        )
    } else {
        format!(
            "Hi {first_name}! I'm {business}'s shop assistant.\n\n{}",
            help(language)
        )
    }
}

pub fn help(language: &str) -> String {
    if is_id(language) {
        "Cukup ceritakan apa yang terjadi, seperti mengirim pesan ke rekan kerja:\n\
         • terjual 3 karung beras @120\n\
         • bayar 500 untuk transport\n\
         • stok masuk 20 soda\n\n\
         Saya akan mengonfirmasi yang saya catat. Salah? Balas \"batal\" atau /undo dalam 5 menit.\n\n\
         /report menampilkan penjualan hari ini (/report minggu, /report bulan)."
            .to_string()
    } else {
        "Just tell me what happened, the way you'd text a colleague:\n\
         • sold 3 bags of rice at 120\n\
         • paid 500 for transport\n\
         • restocked 20 sodas\n\n\
         I'll confirm what I recorded. A mistake? Reply \"no\" or /undo within 5 minutes.\n\n\
         /report shows today's sales (/report week, /report month)."
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
    } else {
        format!("Your Telegram ID: {user_id}")
    }
}

pub fn private_chats_only(language: &str) -> String {
    if is_id(language) {
        "Saya hanya bekerja di chat pribadi. Kirim pesan langsung ke saya.".to_string()
    } else {
        "I only work in a private chat. Message me directly.".to_string()
    }
}

pub fn unknown_command(language: &str) -> String {
    if is_id(language) {
        "Perintah itu tidak saya kenal. Coba /help.".to_string()
    } else {
        "I don't know that command. Try /help.".to_string()
    }
}

pub fn bad_report_window(language: &str) -> String {
    if is_id(language) {
        "Pakai /report, /report minggu, atau /report bulan.".to_string()
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
    } else {
        format!("{business}'s shop assistant: record sales, stock and expenses by chat.")
    };
    s.chars().take(120).collect()
}

fn commands(language: &str) -> Vec<BotCommand> {
    let c =
        |cmd: &str, en: &str, id: &str| BotCommand::new(cmd, if is_id(language) { id } else { en });
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
    for (lang, code) in [(business_language, ""), ("en", "en"), ("id", "id")] {
        let mut desc = bot
            .set_my_description()
            .description(description(lang, business));
        let mut short = bot
            .set_my_short_description()
            .short_description(short_description(lang, business));
        let mut cmds = bot.set_my_commands(commands(lang));
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
            owner_chat_id: Some(10),
            allowed_user_ids: vec![20],
        };
        assert!(owned.allows(10) && owned.allows(20) && !owned.allows(30));
        let (ids, bad) = TelegramAccess::parse_ids(" 20, 30;x  40 ");
        assert_eq!(ids, vec![20, 30, 40]);
        assert_eq!(bad, vec!["x".to_string()]);
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
        for lang in ["en", "id"] {
            assert!(description(lang, "Demo Shop").chars().count() <= 512);
            assert!(short_description(lang, &"x".repeat(200)).chars().count() <= 120);
            assert!(welcome(lang, "Demo Shop", "Ann").contains("Demo Shop"));
            assert!(private_bot(lang, "Demo Shop", 42).contains("42"));
        }
        assert_ne!(help("en"), help("id"));
    }
}
