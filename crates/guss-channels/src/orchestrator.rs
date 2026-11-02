use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile, SourceChannel};
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
}

impl<P: LlmProvider, S: Store> Orchestrator<P, S> {
    pub fn new(nlu: Nlu<P>, store: Arc<S>, business: BusinessProfile) -> Self {
        Self {
            nlu,
            store,
            business,
            pending: Mutex::new(HashMap::new()),
        }
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
        if UNDO_WORDS.contains(&trimmed.as_str()) {
            return self.handle_undo(conversation_key).await;
        }

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
            return Ok(no_events_reply(&self.business.language));
        }

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
