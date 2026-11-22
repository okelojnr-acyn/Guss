//! Receipt photos on their way to Friday: a photo sent in a chat waits for
//! the pay-me-back expense it comes with, then for that expense's Minions
//! claim, which sync uploads it to.
// shortcut: kept in memory, so a restart before sync loses the photo (the claim still goes; it can be added in Minions); persist it in the store if that bites.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

/// A photo waits this long for its expense (the who-paid question can sit
/// between them).
const WAIT: Duration = Duration::from_secs(15 * 60);
/// At most this many photos wait for sync; more are dropped (logged).
const MAX_READY: usize = 200;

#[derive(Default)]
struct Waiting {
    held: HashMap<String, (Instant, Vec<u8>)>,
    ready: HashMap<Uuid, Vec<u8>>,
}

static BOX: LazyLock<Mutex<Waiting>> = LazyLock::new(Mutex::default);

/// A photo arrived in `chat`: it goes with the next pay-me-back expense.
pub fn hold(chat: &str, jpeg: Vec<u8>) {
    if let Ok(mut b) = BOX.lock() {
        b.held.insert(chat.to_string(), (Instant::now(), jpeg));
    }
}

/// `expenses` (pay-me-back) were just saved in `chat`: the waiting photo,
/// if any, belongs to the first.
pub fn attach(chat: &str, expenses: &[Uuid]) {
    let Some(first) = expenses.first() else {
        return;
    };
    let Ok(mut b) = BOX.lock() else {
        return;
    };
    let Some((at, photo)) = b.held.remove(chat) else {
        return;
    };
    if at.elapsed() > WAIT {
        return;
    }
    if b.ready.len() >= MAX_READY {
        tracing::warn!("too many receipt photos waiting for Friday; dropped one");
        return;
    }
    b.ready.insert(*first, photo);
}

/// The photo for this expense, taken out (put it back on failure).
pub fn take(expense: Uuid) -> Option<Vec<u8>> {
    BOX.lock().ok()?.ready.remove(&expense)
}

pub fn put_back(expense: Uuid, photo: Vec<u8>) {
    if let Ok(mut b) = BOX.lock() {
        b.ready.insert(expense, photo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_photo_goes_with_the_next_pay_back_expense_of_its_chat() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        attach("chat-x", &[a]);
        assert_eq!(take(a), None, "no photo held");
        hold("chat-x", vec![1, 2]);
        attach("chat-y", &[b]);
        assert_eq!(take(b), None, "another chat's expense");
        attach("chat-x", &[a, b]);
        assert_eq!(take(a), Some(vec![1, 2]));
        assert_eq!(take(a), None, "once");
    }
}
