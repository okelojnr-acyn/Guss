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
    Phone(String),
    Cancel,
    Other,
}

pub fn answer(text: &str) -> Answer {
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
            Lang::Id => format!(
                "Total {total}. Bagaimana pelanggan membayar? Balas tunai, atau nomor M-Pesa pelanggan untuk mengirim permintaan bayar. (batal untuk membuang)"
            ),
            Lang::Sw => format!(
                "Jumla {total}. Mteja analipa vipi? Jibu cash, au namba ya M-Pesa ya mteja nimtumie ombi la kulipa. (hapana kuacha)"
            ),
            Lang::En => format!(
                "Total {total}. How is the customer paying? Reply cash, or the customer's M-Pesa number to send them the payment prompt. (no to drop it)"
            ),
        }
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
            "No M-Pesa payment came in, so I cancelled the request (if they pay later, it's refunded). Reply cash if they paid another way, or send the number again.",
            "Tidak ada pembayaran M-Pesa, jadi permintaan dibatalkan (kalau dibayar nanti, dikembalikan). Balas tunai kalau dibayar dengan cara lain, atau kirim nomornya lagi.",
            "Hakuna malipo ya M-Pesa, kwa hiyo nimeghairi ombi (wakilipa baadaye, yatarudishwa). Jibu cash kama walilipa kwa njia nyingine, au tuma namba tena.",
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
