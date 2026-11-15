use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile, SourceChannel};
use guss_core::money::CurrencyAmount;
use guss_core::store::{Store, StoreError, TimeWindow};
use guss_insights::{build_report, render_report};
use guss_llm::LlmProvider;
use guss_nlu::{Nlu, NluError, extract_keywords};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

/// How long an owner has to say "no"/"undo" after Guss logs something,
/// before the entry is considered final. This is the confirmation-before-
/// commit mitigation for LLM extraction mistakes: events are written
/// immediately (so the owner sees real confirmation numbers, not a vague
/// "did you mean...?"), but can be reversed within this window - see
/// docs/ARCHITECTURE.md.
const UNDO_WINDOW: Duration = Duration::minutes(5);

const UNDO_WORDS: &[&str] = &["no", "undo", "tidak", "batal"];

/// Replies that keep an amount in the other currency it was given in.
const KEEP_WORDS: &[&str] = &["keep", "yes", "ya", "simpan"];

/// How long a "that's in USD, not your usual KES" question stays open.
/// After this the held entries are dropped (nothing was saved).
const CURRENCY_QUESTION_TTL: Duration = Duration::minutes(10);

/// What to do when a message names an amount in a currency other than
/// the business currency. A wrong currency silently corrupts every total,
/// so the default asks before saving (the one exception to Guss's
/// save-then-undo rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OtherCurrencyPolicy {
    /// Hold the entry and ask the owner to confirm the currency.
    #[default]
    Ask,
    /// Record the amount in the business currency (the number unchanged).
    UseBusinessCurrency,
    /// Record it in the currency given, without asking.
    Allow,
}

impl std::str::FromStr for OtherCurrencyPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "" | "ask" => Ok(Self::Ask),
            "business" | "home" | "shop" => Ok(Self::UseBusinessCurrency),
            "allow" => Ok(Self::Allow),
            other => Err(format!(
                "unknown other-currency policy {other:?} (use ask, business or allow)"
            )),
        }
    }
}

struct AwaitingCurrency {
    events: Vec<BusinessEvent>,
    /// Upper-case codes of the other currencies in `events`.
    foreign: Vec<String>,
    asked_at: DateTime<Utc>,
}

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Nlu(#[from] NluError),
    #[error("voice input/output isn't available: {0}")]
    VoiceNotConfigured(String),
}

struct PendingCommit {
    event_ids: Vec<Uuid>,
    committed_at: DateTime<Utc>,
}

/// Ties NLU extraction, storage, and report rendering together behind one
/// per-conversation API that every Channel adapter (CLI, Telegram,
/// WhatsApp) calls into. `conversation_key` scopes the undo window per
/// chat (e.g. a Telegram chat id) - CLI just uses a fixed key since it's
/// a single session.
pub struct Orchestrator<P: LlmProvider, S: Store> {
    nlu: Nlu<P>,
    store: Arc<S>,
    business: BusinessProfile,
    pending: Mutex<HashMap<String, PendingCommit>>,
    other_currency_policy: OtherCurrencyPolicy,
    awaiting_currency: Mutex<HashMap<String, AwaitingCurrency>>,
}

impl<P: LlmProvider, S: Store> Orchestrator<P, S> {
    pub fn new(nlu: Nlu<P>, store: Arc<S>, business: BusinessProfile) -> Self {
        Self {
            nlu,
            store,
            business,
            pending: Mutex::new(HashMap::new()),
            other_currency_policy: OtherCurrencyPolicy::default(),
            awaiting_currency: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_other_currency_policy(mut self, policy: OtherCurrencyPolicy) -> Self {
        self.other_currency_policy = policy;
        self
    }

    pub fn business(&self) -> &BusinessProfile {
        &self.business
    }

    /// Handles one inbound message: either an undo command, or free text
    /// to extract business events from. Returns the reply text.
    pub async fn handle_message(
        &self,
        conversation_key: &str,
        source_channel: SourceChannel,
        text: &str,
    ) -> Result<String, OrchestratorError> {
        let trimmed = text.trim().to_lowercase();
        let language = self.business.language.as_str();
        let home = self.home_currency();

        // An answer to "that's in USD, not your usual KES" comes first.
        let mut dropped_note = None;
        let awaiting = self.awaiting_currency.lock().await.remove(conversation_key);
        if let Some(q) = awaiting.filter(|q| Utc::now() - q.asked_at <= CURRENCY_QUESTION_TTL) {
            if UNDO_WORDS.contains(&trimmed.as_str()) {
                return Ok(currency_cancelled_reply(language));
            }
            let answer = trimmed.to_uppercase();
            if q.foreign.contains(&answer) || KEEP_WORDS.contains(&trimmed.as_str()) {
                return self.commit(conversation_key, q.events).await;
            }
            if answer == home {
                return self
                    .commit(conversation_key, to_currency(q.events, &home))
                    .await;
            }
            // Anything else: the question lapses, nothing from it is saved,
            // and this message is handled on its own.
            dropped_note = Some(currency_dropped_note(&q.foreign, language));
        }

        if UNDO_WORDS.contains(&trimmed.as_str()) {
            return self.handle_undo(conversation_key).await;
        }

        let reply = self
            .extract_and_commit(conversation_key, source_channel, text, &home)
            .await?;
        Ok(match dropped_note {
            Some(note) => format!("{note}\n{reply}"),
            None => reply,
        })
    }

    fn home_currency(&self) -> String {
        self.business.default_currency.trim().to_uppercase()
    }

    async fn extract_and_commit(
        &self,
        conversation_key: &str,
        source_channel: SourceChannel,
        text: &str,
        home: &str,
    ) -> Result<String, OrchestratorError> {
        let language = self.business.language.as_str();
        let keywords = extract_keywords(text);
        let context = self
            .store
            .search_documents(self.business.id, &keywords, 3)
            .await?;
        let context_snippets: Vec<String> = context
            .into_iter()
            .map(|d| format!("{}: {}", d.title, d.content))
            .collect();

        let events = self
            .nlu
            .extract(
                self.business.id,
                source_channel,
                text,
                &self.business.default_currency,
                &context_snippets,
            )
            .await?;

        if events.is_empty() {
            return Ok(no_events_reply(language));
        }

        let foreign = foreign_currencies(&events, home);
        if foreign.is_empty() {
            return self.commit(conversation_key, events).await;
        }
        match self.other_currency_policy {
            OtherCurrencyPolicy::Allow => self.commit(conversation_key, events).await,
            OtherCurrencyPolicy::UseBusinessCurrency => {
                self.commit(conversation_key, to_currency(events, home))
                    .await
            }
            OtherCurrencyPolicy::Ask => {
                let reply = currency_question(&foreign, home, language);
                self.awaiting_currency.lock().await.insert(
                    conversation_key.to_string(),
                    AwaitingCurrency {
                        events,
                        foreign,
                        asked_at: Utc::now(),
                    },
                );
                Ok(reply)
            }
        }
    }

    /// Saves events and opens the undo window for them.
    async fn commit(
        &self,
        conversation_key: &str,
        events: Vec<BusinessEvent>,
    ) -> Result<String, OrchestratorError> {
        let mut ids = Vec::with_capacity(events.len());
        let mut lines = Vec::with_capacity(events.len());
        for event in &events {
            self.store.save_event(event).await?;
            ids.push(event.id());
            lines.push(describe_event(event, &self.business.language));
        }

        {
            let mut pending = self.pending.lock().await;
            pending.insert(
                conversation_key.to_string(),
                PendingCommit {
                    event_ids: ids,
                    committed_at: Utc::now(),
                },
            );
        }

        Ok(confirmation_reply(&lines, &self.business.language))
    }

    async fn handle_undo(&self, conversation_key: &str) -> Result<String, OrchestratorError> {
        let entry = {
            let mut pending = self.pending.lock().await;
            pending.remove(conversation_key)
        };

        match entry {
            Some(commit) if Utc::now() - commit.committed_at <= UNDO_WINDOW => {
                for id in commit.event_ids {
                    self.store.delete_event(id).await?;
                }
                Ok(undone_reply(&self.business.language))
            }
            _ => Ok(nothing_to_undo_reply(&self.business.language)),
        }
    }

    pub async fn handle_report(&self, window: TimeWindow) -> Result<String, OrchestratorError> {
        let report = build_report(
            self.store.as_ref(),
            self.business.id,
            window,
            &self.business.default_currency,
        )
        .await?;
        Ok(render_report(
            &report,
            &self.business.name,
            &self.business.language,
        ))
    }

    /// Entry point for webhook-delivered channels (Telegram, WhatsApp),
    /// which can redeliver the same update more than once. Deduplicates by
    /// the provider's own message id before doing anything else; returns
    /// `Ok(None)` for a message already seen so the caller sends nothing
    /// back (silently swallowing a redelivery, not a real exchange).
    pub async fn handle_inbound(
        &self,
        conversation_key: &str,
        source_channel: SourceChannel,
        provider_message_id: &str,
        text: &str,
    ) -> Result<Option<String>, OrchestratorError> {
        let is_new = self
            .store
            .mark_message_processed(source_channel_tag(source_channel), provider_message_id)
            .await?;
        if !is_new {
            tracing::debug!(provider_message_id, "skipping redelivered message");
            return Ok(None);
        }
        self.handle_message(conversation_key, source_channel, text)
            .await
            .map(Some)
    }
}

fn source_channel_tag(channel: SourceChannel) -> &'static str {
    match channel {
        SourceChannel::Cli => "cli",
        SourceChannel::Telegram => "telegram",
        SourceChannel::WhatsApp => "whatsapp",
        SourceChannel::Web => "web",
    }
}

fn describe_event(event: &BusinessEvent, language: &str) -> String {
    match event {
        BusinessEvent::Sale(s) => {
            let total = s.total();
            match language {
                "id" => format!(
                    "Terjual: {} {} seharga {} = {}",
                    s.quantity, s.item_name, s.unit_price, total
                ),
                _ => format!(
                    "Sold: {} {} at {} = {}",
                    s.quantity, s.item_name, s.unit_price, total
                ),
            }
        }
        BusinessEvent::Expense(e) => match language {
            "id" => format!("Pengeluaran: {} sebesar {}", e.description, e.amount),
            _ => format!("Expense: {} for {}", e.description, e.amount),
        },
        BusinessEvent::StockUpdate(su) => match language {
            "id" => format!("Stok {}: {:+}", su.item_name, su.delta),
            _ => format!("Stock {}: {:+}", su.item_name, su.delta),
        },
        BusinessEvent::Note(n) => match language {
            "id" => format!("Catatan: {}", n.text),
            _ => format!("Note: {}", n.text),
        },
    }
}

/// Upper-case codes of every money amount in `events` that isn't in the
/// business currency, in order of first appearance. Restock costs are
/// always recorded in the business currency by the NLU, so only sales and
/// expenses can differ.
fn foreign_currencies(events: &[BusinessEvent], home: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for event in events {
        let ccy = match event {
            BusinessEvent::Sale(s) => &s.unit_price.currency,
            BusinessEvent::Expense(e) => &e.amount.currency,
            _ => continue,
        };
        let ccy = ccy.trim().to_uppercase();
        if ccy != home && !out.contains(&ccy) {
            out.push(ccy);
        }
    }
    out
}

/// Same numbers, recorded in `currency` (the owner said the amount was in
/// the business currency, not that it should be converted).
fn to_currency(events: Vec<BusinessEvent>, currency: &str) -> Vec<BusinessEvent> {
    let restate = |a: &CurrencyAmount| CurrencyAmount::new(a.major_units(), currency);
    events
        .into_iter()
        .map(|event| match event {
            BusinessEvent::Sale(mut s) => {
                s.unit_price = restate(&s.unit_price);
                BusinessEvent::Sale(s)
            }
            BusinessEvent::Expense(mut e) => {
                e.amount = restate(&e.amount);
                BusinessEvent::Expense(e)
            }
            other => other,
        })
        .collect()
}

fn currency_question(foreign: &[String], home: &str, language: &str) -> String {
    let list = foreign.join(" / ");
    let keep = foreign[0].to_lowercase();
    let home_word = home.to_lowercase();
    match language {
        "id" => format!(
            "Itu dalam {list}, bukan {home} seperti biasanya. Belum ada yang disimpan.\n\
             Balas \"{keep}\" untuk tetap memakai {list}, atau \"{home_word}\" jika maksudnya {home}."
        ),
        _ => format!(
            "That's in {list}, not your usual {home}. Nothing is saved yet.\n\
             Reply \"{keep}\" to keep it in {list}, or \"{home_word}\" if it was {home}."
        ),
    }
}

fn currency_cancelled_reply(language: &str) -> String {
    match language {
        "id" => "Dibatalkan, tidak ada yang disimpan.".to_string(),
        _ => "Cancelled, nothing was saved.".to_string(),
    }
}

fn currency_dropped_note(foreign: &[String], language: &str) -> String {
    let list = foreign.join(" / ");
    match language {
        "id" => format!("(Catatan dalam {list} sebelumnya tidak disimpan.)"),
        _ => format!("(The earlier {list} entry was not saved.)"),
    }
}

fn confirmation_reply(lines: &[String], language: &str) -> String {
    let body = lines.join("\n");
    match language {
        "id" => format!("{body}\n\nBalas \"tidak\" dalam 5 menit untuk membatalkan."),
        _ => format!("{body}\n\nReply \"no\" within 5 minutes to undo."),
    }
}

fn no_events_reply(language: &str) -> String {
    match language {
        "id" => "Maaf, saya tidak menemukan apa pun untuk dicatat dari pesan itu.".to_string(),
        _ => "I couldn't find anything to log in that message.".to_string(),
    }
}

fn undone_reply(language: &str) -> String {
    match language {
        "id" => "Dibatalkan.".to_string(),
        _ => "Undone.".to_string(),
    }
}

fn nothing_to_undo_reply(language: &str) -> String {
    match language {
        "id" => "Tidak ada yang bisa dibatalkan (mungkin sudah lewat 5 menit).".to_string(),
        _ => "Nothing to undo (it may be past the 5-minute window).".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use guss_llm::{JsonCompletionRequest, LlmError};
    use guss_store::SqliteStore;
    use serde_json::{Value, json};

    /// Answers every extraction with the same events.
    struct FixedProvider(Value);

    #[async_trait]
    impl LlmProvider for FixedProvider {
        async fn complete_json(&self, _req: JsonCompletionRequest) -> Result<Value, LlmError> {
            Ok(self.0.clone())
        }
    }

    fn usd_sale() -> Value {
        json!({"events": [{"kind": "sale", "item_name": "rice", "quantity": 1.0,
            "unit_price_major": 50.0, "currency": "USD", "category": null}]})
    }

    fn kes_sale() -> Value {
        json!({"events": [{"kind": "sale", "item_name": "rice", "quantity": 3.0,
            "unit_price_major": 120.0, "currency": null, "category": null}]})
    }

    async fn orchestrator(
        events: Value,
        language: &str,
        policy: OtherCurrencyPolicy,
    ) -> Orchestrator<FixedProvider, SqliteStore> {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let business = BusinessProfile {
            id: Uuid::new_v4(),
            name: "Duka".into(),
            default_currency: "KES".into(),
            language: language.into(),
            timezone: "UTC".into(),
        };
        Orchestrator::new(Nlu::new(FixedProvider(events), language), store, business)
            .with_other_currency_policy(policy)
    }

    async fn saved_sales(o: &Orchestrator<FixedProvider, SqliteStore>) -> Vec<CurrencyAmount> {
        o.store
            .list_events(o.business.id, TimeWindow::Day)
            .await
            .unwrap()
            .into_iter()
            .filter_map(|e| match e {
                BusinessEvent::Sale(s) => Some(s.total()),
                _ => None,
            })
            .collect()
    }

    async fn say(o: &Orchestrator<FixedProvider, SqliteStore>, text: &str) -> String {
        o.handle_message("chat", SourceChannel::Telegram, text)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn business_currency_sale_is_saved_without_asking() {
        let o = orchestrator(kes_sale(), "en", OtherCurrencyPolicy::Ask).await;
        let reply = say(&o, "sold 3 rice at 120").await;
        assert!(reply.starts_with("Sold:"), "{reply}");
        assert_eq!(
            saved_sales(&o).await,
            vec![CurrencyAmount::new(360.0, "KES")]
        );
    }

    #[tokio::test]
    async fn other_currency_is_held_until_confirmed() {
        let o = orchestrator(usd_sale(), "en", OtherCurrencyPolicy::Ask).await;
        let reply = say(&o, "sold rice for $50").await;
        assert!(
            reply.contains("That's in USD, not your usual KES"),
            "{reply}"
        );
        assert!(saved_sales(&o).await.is_empty());

        let reply = say(&o, "USD").await;
        assert!(reply.starts_with("Sold:"), "{reply}");
        assert_eq!(
            saved_sales(&o).await,
            vec![CurrencyAmount::new(50.0, "USD")]
        );

        // The confirmed entry gets the normal undo window.
        assert_eq!(say(&o, "no").await, "Undone.");
        assert!(saved_sales(&o).await.is_empty());
    }

    #[tokio::test]
    async fn answering_with_business_currency_records_it_in_business_currency() {
        let o = orchestrator(usd_sale(), "en", OtherCurrencyPolicy::Ask).await;
        say(&o, "sold rice for 50 dollars").await;
        say(&o, "kes").await;
        assert_eq!(
            saved_sales(&o).await,
            vec![CurrencyAmount::new(50.0, "KES")]
        );
    }

    #[tokio::test]
    async fn no_cancels_a_held_entry() {
        let o = orchestrator(usd_sale(), "id", OtherCurrencyPolicy::Ask).await;
        let reply = say(&o, "jual beras 50 dolar").await;
        assert!(reply.contains("Itu dalam USD, bukan KES"), "{reply}");
        assert_eq!(
            say(&o, "batal").await,
            "Dibatalkan, tidak ada yang disimpan."
        );
        assert!(saved_sales(&o).await.is_empty());
    }

    #[tokio::test]
    async fn an_unrelated_reply_drops_the_held_entry_and_is_handled_itself() {
        let o = orchestrator(usd_sale(), "en", OtherCurrencyPolicy::Ask).await;
        say(&o, "sold rice for $50").await;
        // The provider returns the USD sale again, so this asks again,
        // after saying the first one was not saved.
        let reply = say(&o, "sold rice for $50 again").await;
        assert!(
            reply.starts_with("(The earlier USD entry was not saved.)"),
            "{reply}"
        );
        assert!(reply.contains("That's in USD"), "{reply}");
        assert!(saved_sales(&o).await.is_empty());
    }

    #[tokio::test]
    async fn policy_allow_saves_other_currency_directly() {
        let o = orchestrator(usd_sale(), "en", OtherCurrencyPolicy::Allow).await;
        say(&o, "sold rice for $50").await;
        assert_eq!(
            saved_sales(&o).await,
            vec![CurrencyAmount::new(50.0, "USD")]
        );
    }

    #[tokio::test]
    async fn policy_business_records_in_business_currency() {
        let o = orchestrator(usd_sale(), "en", OtherCurrencyPolicy::UseBusinessCurrency).await;
        say(&o, "sold rice for $50").await;
        assert_eq!(
            saved_sales(&o).await,
            vec![CurrencyAmount::new(50.0, "KES")]
        );
    }

    #[test]
    fn policy_parses_from_config_words() {
        assert_eq!("".parse(), Ok(OtherCurrencyPolicy::Ask));
        assert_eq!("Ask".parse(), Ok(OtherCurrencyPolicy::Ask));
        assert_eq!(
            "business".parse(),
            Ok(OtherCurrencyPolicy::UseBusinessCurrency)
        );
        assert_eq!("allow".parse(), Ok(OtherCurrencyPolicy::Allow));
        assert!("convert".parse::<OtherCurrencyPolicy>().is_err());
    }
}
