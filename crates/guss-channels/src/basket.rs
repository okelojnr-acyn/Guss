//! A scanned Sell batch waiting for "how is the customer paying?": cash
//! records it at once; an M-Pesa number has Friday send the customer the
//! payment prompt for the whole basket, and the sale is recorded once
//! Friday says it's paid (owner, 2026-11-22: "can Guss on sell prompt").
// shortcut: baskets live in memory; a restart drops an unanswered one (nothing was recorded, scan again).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use guss_core::domain::BusinessEvent;
use serde_json::Value;

/// How long a basket waits for its answer.
const WAIT: Duration = Duration::from_secs(10 * 60);

#[derive(Clone)]
pub struct Basket {
    pub events: Vec<BusinessEvent>,
    /// Friday's sale lines (product ids and tax types from the scan).
    pub lines: Vec<Value>,
    pub amount_cents: i64,
    pub currency: String,
}

static WAITING: LazyLock<Mutex<HashMap<String, (Instant, Basket)>>> = LazyLock::new(Mutex::default);

pub fn hold(chat: &str, basket: Basket) {
    if let Ok(mut m) = WAITING.lock() {
        m.insert(chat.to_string(), (Instant::now(), basket));
    }
}

/// The chat's basket, taken out, if it hasn't run out.
pub fn take(chat: &str) -> Option<Basket> {
    let (at, b) = WAITING.lock().ok()?.remove(chat)?;
    (at.elapsed() <= WAIT).then_some(b)
}

pub fn waiting(chat: &str) -> bool {
    WAITING
        .lock()
        .is_ok_and(|m| m.get(chat).is_some_and(|(at, _)| at.elapsed() <= WAIT))
}

/// The answer to "cash or M-Pesa number?".
#[derive(Debug, PartialEq)]
pub enum Answer {
    Cash,
    /// The M-Pesa button: ask for the customer's number.
    AskPhone,
    Phone(String),
    Cancel,
    Other,
}

/// The buttons Guss shows after a scan, so every next step is a tap
/// (owner, 2026-11-22: "all these should be guided"). Each label is
/// [en, id, sw]; a tap arrives as its label's text.
pub mod labels {
    pub const CASH: [&str; 3] = ["💵 Cash", "💵 Tunai", "💵 Taslimu"];
    pub const MPESA: [&str; 3] = [
        "📱 M-Pesa prompt",
        "📱 Permintaan M-Pesa",
        "📱 Ombi la M-Pesa",
    ];
    pub const CANCEL: [&str; 3] = ["✖ Cancel", "✖ Batal", "✖ Ghairi"];
    pub const SELL_ONE: [&str; 3] = ["🛒 Sell 1", "🛒 Jual 1", "🛒 Uza 1"];
    pub const GOT_MORE: [&str; 3] = ["📦 Got more", "📦 Stok masuk", "📦 Mzigo umeingia"];
    pub const DONE: [&str; 3] = ["↩ Done", "↩ Selesai", "↩ Maliza"];

    pub fn pick(language: &str, l: [&'static str; 3]) -> &'static str {
        crate::journal_texts::pick(language, l[0], l[1], l[2])
    }
    pub fn is(text: &str, l: [&str; 3]) -> bool {
        l.iter().any(|x| x.eq_ignore_ascii_case(text.trim()))
    }
}

/// A checked product's next step.
#[derive(Debug, PartialEq)]
pub enum CheckTap {
    SellOne,
    GotMore,
    Done,
}

pub fn check_tap(text: &str) -> Option<CheckTap> {
    use labels::*;
    if is(text, SELL_ONE) {
        Some(CheckTap::SellOne)
    } else if is(text, GOT_MORE) {
        Some(CheckTap::GotMore)
    } else if is(text, DONE) {
        Some(CheckTap::Done)
    } else {
        None
    }
}

/// The product a Check found, per chat, and whether Guss asked "how many came in?".
type Checked = HashMap<String, (Instant, String, bool)>;
static CHECKED: LazyLock<Mutex<Checked>> = LazyLock::new(Mutex::default);

/// Remembers the product code a Check found, for the Sell 1 / Got more buttons.
pub fn checked(chat: &str, code: &str) {
    if let Ok(mut m) = CHECKED.lock() {
        m.insert(chat.to_string(), (Instant::now(), code.to_string(), false));
    }
}

/// The checked code, if any, and whether Guss asked "how many came in?".
pub fn last_checked(chat: &str) -> Option<(String, bool)> {
    let m = CHECKED.lock().ok()?;
    let (at, code, asking) = m.get(chat)?;
    (at.elapsed() <= WAIT).then(|| (code.clone(), *asking))
}

/// "Got more" was tapped: the next number is how many came in.
pub fn ask_quantity(chat: &str) {
    if let Ok(mut m) = CHECKED.lock()
        && let Some(e) = m.get_mut(chat)
    {
        e.0 = Instant::now();
        e.2 = true;
    }
}

pub fn forget_checked(chat: &str) {
    if let Ok(mut m) = CHECKED.lock() {
        m.remove(chat);
    }
}

pub fn answer(text: &str) -> Answer {
    if labels::is(text, labels::CASH) {
        return Answer::Cash;
    }
    if labels::is(text, labels::MPESA) {
        return Answer::AskPhone;
    }
    if labels::is(text, labels::CANCEL) {
        return Answer::Cancel;
    }
    let t = text.trim().to_lowercase();
    if ["cash", "1", "tunai", "pesa taslimu", "taslimu", "cash."].contains(&t.as_str()) {
        return Answer::Cash;
    }
    if ["cancel", "no", "undo", "hapana", "batal", "tidak"].contains(&t.as_str()) {
        return Answer::Cancel;
    }
    // A phone number: Friday checks and normalises it (+2547..., 07..., 01...).
    let digits: String = t.chars().filter(|c| c.is_ascii_digit()).collect();
    let only_number = t.chars().all(|c| c.is_ascii_digit() || " +-()".contains(c));
    if only_number && (9..=12).contains(&digits.len()) {
        return Answer::Phone(t.trim().to_string());
    }
    Answer::Other
}

/// The replies, in the shop's language.
pub mod say {
    use crate::journal_texts::{Lang, lang};

    pub fn question(total: &str, language: &str) -> String {
        match lang(language) {
            Lang::Id => {
                format!("Total {total}. Bagaimana pelanggan membayar? Ketuk tombol di bawah.")
            }
            Lang::Sw => format!("Jumla {total}. Mteja analipa vipi? Gusa kitufe hapa chini."),
            Lang::En => format!("Total {total}. How is the customer paying? Tap a button below."),
        }
    }

    pub fn ask_phone(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "Type the customer's M-Pesa number (07…, 01… or +254…). They get the prompt on their phone.",
            "Ketik nomor M-Pesa pelanggan (07…, 01… atau +254…). Mereka menerima permintaan di ponselnya.",
            "Andika namba ya M-Pesa ya mteja (07…, 01… au +254…). Atapata ombi kwenye simu yake.",
        )
    }

    pub fn not_a_phone(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "That isn't a phone number. Type it like 0712 345 678, or tap Cash or Cancel.",
            "Itu bukan nomor telepon. Ketik seperti 0812 345 678, atau ketuk Tunai atau Batal.",
            "Hiyo si namba ya simu. Andika kama 0712 345 678, au gusa Taslimu au Ghairi.",
        )
    }

    pub fn how_many(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "How many came in? Type a number.",
            "Berapa yang masuk? Ketik angka.",
            "Ngapi zimeingia? Andika namba.",
        )
    }

    pub fn ready(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "Ready: scan the next batch with the buttons below.",
            "Siap: pindai berikutnya dengan tombol di bawah.",
            "Tayari: skani inayofuata kwa vitufe vilivyo chini.",
        )
    }

    pub fn dropped(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "Dropped; nothing was recorded.",
            "Dibuang; tidak ada yang dicatat.",
            "Imeachwa; hakuna kilichorekodiwa.",
        )
    }

    pub fn lapsed(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "(The scanned sale wasn't saved: scan it again.)",
            "(Penjualan yang dipindai tidak disimpan: pindai lagi.)",
            "(Mauzo uliyoskani hayakuhifadhiwa: skani tena.)",
        )
    }

    pub fn sent(prompt: &str, language: &str) -> String {
        match lang(language) {
            Lang::Id => format!("{prompt}\nSaya kabari begitu sudah dibayar."),
            Lang::Sw => format!("{prompt}\nNitakuambia likishalipwa."),
            Lang::En => format!("{prompt}\nI'll tell you as soon as it's paid."),
        }
    }

    pub fn paid(total: &str, language: &str) -> String {
        match lang(language) {
            Lang::Id => format!("✅ Dibayar lewat M-Pesa: {total}."),
            Lang::Sw => format!("✅ Imelipwa kwa M-Pesa: {total}."),
            Lang::En => format!("✅ Paid by M-Pesa: {total}."),
        }
    }

    pub fn not_paid(language: &str) -> &'static str {
        crate::journal_texts::pick(
            language,
            "No M-Pesa payment came in, so I cancelled the request (if they pay later, it's refunded). Tap Cash if they paid another way, or M-Pesa prompt to try again.",
            "Tidak ada pembayaran M-Pesa, jadi permintaan dibatalkan (kalau dibayar nanti, dikembalikan). Ketuk Tunai kalau dibayar dengan cara lain, atau Permintaan M-Pesa untuk mencoba lagi.",
            "Hakuna malipo ya M-Pesa, kwa hiyo nimeghairi ombi (wakilipa baadaye, yatarudishwa). Gusa Taslimu kama walilipa kwa njia nyingine, au Ombi la M-Pesa kujaribu tena.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers() {
        assert_eq!(answer("Cash"), Answer::Cash);
        assert_eq!(answer("0712 345 678"), Answer::Phone("0712 345 678".into()));
        assert_eq!(
            answer("+254712345678"),
            Answer::Phone("+254712345678".into())
        );
        assert_eq!(answer("no"), Answer::Cancel);
        assert_eq!(answer("sold 3 rice"), Answer::Other);
        assert_eq!(answer("12"), Answer::Other);
        assert_eq!(answer("💵 Taslimu"), Answer::Cash);
        assert_eq!(answer("📱 M-Pesa prompt"), Answer::AskPhone);
        assert_eq!(answer("✖ Batal"), Answer::Cancel);
    }

    #[test]
    fn check_buttons_lead_somewhere() {
        assert_eq!(check_tap("🛒 Uza 1"), Some(CheckTap::SellOne));
        assert_eq!(check_tap("📦 Got more"), Some(CheckTap::GotMore));
        assert_eq!(check_tap("sold 2"), None);
        checked("chat-c", "6161100");
        assert_eq!(last_checked("chat-c"), Some(("6161100".into(), false)));
        ask_quantity("chat-c");
        assert_eq!(last_checked("chat-c"), Some(("6161100".into(), true)));
        forget_checked("chat-c");
        assert_eq!(last_checked("chat-c"), None);
    }

    #[test]
    fn a_basket_is_taken_once() {
        let b = Basket {
            events: vec![],
            lines: vec![],
            amount_cents: 100,
            currency: "KES".into(),
        };
        hold("chat-b", b);
        assert!(waiting("chat-b"));
        assert!(take("chat-b").is_some());
        assert!(take("chat-b").is_none());
    }
}
