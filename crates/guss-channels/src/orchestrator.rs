use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use guss_core::domain::{BusinessEvent, BusinessProfile, SourceChannel};
use guss_core::money::CurrencyAmount;
use guss_core::people::{Action, Actor};
use guss_core::store::{JournalEntry, Store, StoreError, TimeWindow};
use guss_insights::{build_report, render_report};
use guss_llm::LlmProvider;
use guss_nlu::{Nlu, NluError, extract_keywords};

use crate::items::{self, Catalog, UnitQuestion};
use crate::journal_texts as texts;
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
const QUESTION_TTL: Duration = Duration::minutes(10);

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

/// Entries held back (nothing saved) until the owner answers a question.
struct Held {
    events: Vec<BusinessEvent>,
    question: Question,
    asked_at: DateTime<Utc>,
}

enum Question {
    /// "That's in USD, not your usual KES". Upper-case codes.
    Currency { foreign: Vec<String> },
    /// "How many bottles in one crate of soda?"
    Unit(UnitQuestion),
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

/// A reply plus messages for other people (delivered by the channel).
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub reply: String,
    pub notify: Vec<Notify>,
}

impl Outcome {
    fn reply(text: impl Into<String>) -> Self {
        Self {
            reply: text.into(),
            notify: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notify {
    /// Everyone who can decide void requests (owner and managers).
    Approvers(String),
    /// One person, by actor id ("telegram:12345").
    Person(String, String),
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
    held: Mutex<HashMap<String, Held>>,
}

impl<P: LlmProvider, S: Store> Orchestrator<P, S> {
    pub fn new(nlu: Nlu<P>, store: Arc<S>, business: BusinessProfile) -> Self {
        Self {
            nlu,
            store,
            business,
            pending: Mutex::new(HashMap::new()),
            other_currency_policy: OtherCurrencyPolicy::default(),
            held: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_other_currency_policy(mut self, policy: OtherCurrencyPolicy) -> Self {
        self.other_currency_policy = policy;
        self
    }

    pub fn business(&self) -> &BusinessProfile {
        &self.business
    }

    /// Handles one inbound message as the owner (CLI, and channels that
    /// don't know their people yet).
    pub async fn handle_message(
        &self,
        conversation_key: &str,
        source_channel: SourceChannel,
        text: &str,
    ) -> Result<String, OrchestratorError> {
        let actor = Actor::owner(source_channel_tag(source_channel));
        self.handle_message_as(&actor, conversation_key, source_channel, text)
            .await
    }

    /// Handles one inbound message from `actor`: either an undo command, or
    /// free text to extract business events from. Returns the reply text.
    pub async fn handle_message_as(
        &self,
        actor: &Actor,
        conversation_key: &str,
        source_channel: SourceChannel,
        text: &str,
    ) -> Result<String, OrchestratorError> {
        let trimmed = text.trim().to_lowercase();
        let language = self.business.language.as_str();
        let home = self.home_currency();

        // An answer to a question about held entries comes first.
        let mut dropped_note = None;
        let held = self.held.lock().await.remove(conversation_key);
        if let Some(h) = held.filter(|h| Utc::now() - h.asked_at <= QUESTION_TTL) {
            if UNDO_WORDS.contains(&trimmed.as_str()) {
                return Ok(currency_cancelled_reply(language));
            }
            match h.question {
                Question::Currency { foreign } => {
                    let answer = trimmed.to_uppercase();
                    if foreign.contains(&answer) || KEEP_WORDS.contains(&trimmed.as_str()) {
                        return self.commit(actor, conversation_key, h.events).await;
                    }
                    if answer == home {
                        return self
                            .commit(actor, conversation_key, to_currency(h.events, &home))
                            .await;
                    }
                    dropped_note = Some(currency_dropped_note(&foreign, language));
                }
                Question::Unit(q) => {
                    if let Some(count) = items::parse_count(&trimmed) {
                        self.learn_unit(&q, count).await?;
                        return self.prepare(actor, conversation_key, h.events, &home).await;
                    }
                    dropped_note = Some(unit_dropped_note(&q.item, language));
                }
            }
            // Anything else: the question lapses, nothing held is saved,
            // and this message is handled on its own.
        }

        if UNDO_WORDS.contains(&trimmed.as_str()) {
            return self.handle_undo(actor, conversation_key).await;
        }

        let reply = self
            .extract_and_commit(actor, conversation_key, source_channel, text, &home)
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
        actor: &Actor,
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
        let mut context_snippets: Vec<String> = context
            .into_iter()
            .map(|d| format!("{}: {}", d.title, d.content))
            .collect();
        if let Some(line) = self.catalog().await?.context_line(language) {
            context_snippets.push(line);
        }

        let extraction = self
            .nlu
            .extract_checked(
                self.business.id,
                source_channel,
                text,
                &self.business.default_currency,
                &context_snippets,
            )
            .await?;
        if !extraction.issues.is_empty() {
            // Save nothing from a message Guss couldn't fully work out: half
            // a message recorded is harder to fix than none.
            return Ok(issues_reply(&extraction.issues, language));
        }
        let events = extraction.events;

        if events.is_empty() {
            return Ok(no_events_reply(language));
        }
        self.prepare(actor, conversation_key, events, home).await
    }

    async fn catalog(&self) -> Result<Catalog, OrchestratorError> {
        Ok(Catalog::new(self.store.list_items(self.business.id).await?))
    }

    /// Records "one crate of soda = 24 bottles" on the item.
    async fn learn_unit(&self, q: &UnitQuestion, count: f64) -> Result<(), OrchestratorError> {
        let catalog = self.catalog().await?;
        if let Some(item) = catalog.find(&q.item) {
            let mut item = item.clone();
            item.units.retain(|(u, _)| u != &q.unit);
            item.units.push((q.unit.clone(), count));
            self.store.save_item(self.business.id, &item).await?;
        }
        Ok(())
    }

    /// Items and units, then currency; asks about the first thing it can't
    /// settle (holding everything), otherwise saves.
    async fn prepare(
        &self,
        actor: &Actor,
        conversation_key: &str,
        mut events: Vec<BusinessEvent>,
        home: &str,
    ) -> Result<String, OrchestratorError> {
        let language = self.business.language.as_str();
        let catalog = self.catalog().await?;
        if let Some(q) = items::resolve(&mut events, &catalog) {
            let reply = items::unit_question_text(&q, language);
            self.hold(conversation_key, events, Question::Unit(q)).await;
            return Ok(reply);
        }

        let foreign = foreign_currencies(&events, home);
        if foreign.is_empty() {
            return self.commit(actor, conversation_key, events).await;
        }
        match self.other_currency_policy {
            OtherCurrencyPolicy::Allow => self.commit(actor, conversation_key, events).await,
            OtherCurrencyPolicy::UseBusinessCurrency => {
                self.commit(actor, conversation_key, to_currency(events, home))
                    .await
            }
            OtherCurrencyPolicy::Ask => {
                let reply = currency_question(&foreign, home, language);
                self.hold(conversation_key, events, Question::Currency { foreign })
                    .await;
                Ok(reply)
            }
        }
    }

    async fn hold(&self, conversation_key: &str, events: Vec<BusinessEvent>, question: Question) {
        self.held.lock().await.insert(
            conversation_key.to_string(),
            Held {
                events,
                question,
                asked_at: Utc::now(),
            },
        );
    }

    /// Saves events and opens the undo window for them.
    async fn commit(
        &self,
        actor: &Actor,
        conversation_key: &str,
        events: Vec<BusinessEvent>,
    ) -> Result<String, OrchestratorError> {
        let catalog = self.catalog().await?;
        for item in items::learn(&events, &catalog) {
            self.store.save_item(self.business.id, &item).await?;
        }
        let mut ids = Vec::with_capacity(events.len());
        let mut lines = Vec::with_capacity(events.len());
        for event in &events {
            self.store.save_event_by(event, Some(actor)).await?;
            ids.push(event.id());
            lines.push(describe_event(event, &self.business.language));
        }
        if events.iter().all(|e| matches!(e, BusinessEvent::Note(_))) {
            lines.push(String::new());
            lines.push(notes_only_hint(&self.business.language).to_string());
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

    async fn handle_undo(
        &self,
        actor: &Actor,
        conversation_key: &str,
    ) -> Result<String, OrchestratorError> {
        let entry = {
            let mut pending = self.pending.lock().await;
            pending.remove(conversation_key)
        };

        match entry {
            Some(commit) if Utc::now() - commit.committed_at <= UNDO_WINDOW => {
                // Nothing is deleted: the entries stay in the journal as
                // voided ("undone"), they just stop counting.
                for id in commit.event_ids {
                    self.store.void_event(id, actor, "undone").await?;
                }
                Ok(undone_reply(&self.business.language))
            }
            _ => Ok(nothing_to_undo_reply(&self.business.language)),
        }
    }

    /// The last `limit` entries, newest first, with who recorded them and
    /// any void: "#12 Sold: 3 rice at 120.00 KES = 360.00 KES (Mary)".
    pub async fn history(&self, limit: i64) -> Result<String, OrchestratorError> {
        let language = self.business.language.as_str();
        let entries = self.store.journal(self.business.id, limit).await?;
        if entries.is_empty() {
            return Ok(texts::history_empty(language).into());
        }
        let lines: Vec<String> = entries.iter().map(|e| self.entry_line(e)).collect();
        Ok(format!(
            "{}\n{}",
            texts::history_title(language),
            lines.join("\n")
        ))
    }

    fn entry_line(&self, e: &JournalEntry) -> String {
        let language = self.business.language.as_str();
        let mut line = format!("#{} {}", e.number, describe_event(&e.event, language));
        if let Some(who) = &e.recorded_by {
            line.push_str(&format!(" ({who})"));
        }
        if let Some(v) = &e.voided {
            line.push_str(&texts::voided_suffix(&v.by, &v.reason, language));
        }
        if let Some(err) = &e.sync_error {
            line.push_str(&texts::not_in_friday(err, language));
        }
        line
    }

    /// Stop entry `number` from counting. Owners, and managers for entries
    /// not made by the owner, do it directly; anyone else makes a request
    /// that the owner or a manager decides. Nothing is ever deleted.
    pub async fn void_entry(
        &self,
        actor: &Actor,
        number: i64,
        reason: &str,
    ) -> Result<Outcome, OrchestratorError> {
        let language = self.business.language.as_str();
        let reason = if reason.trim().is_empty() {
            texts::no_reason(language)
        } else {
            reason.trim()
        };
        let Some(entry) = self.store.journal_entry(self.business.id, number).await? else {
            return Ok(Outcome::reply(texts::no_such_entry(number, language)));
        };
        if let Some(v) = &entry.voided {
            return Ok(Outcome::reply(texts::already_voided(
                number, &v.by, language,
            )));
        }
        let line = self.entry_line(&entry);
        if actor.role.can(Action::VoidDirectly)
            && actor.role.outranks_for_voids(entry.recorded_by_role)
        {
            self.store
                .void_event(entry.event.id(), actor, reason)
                .await?;
            return Ok(Outcome::reply(texts::voided(&line, language)));
        }
        let request = self
            .store
            .request_void(self.business.id, number, actor, reason)
            .await?;
        Ok(Outcome {
            reply: texts::void_requested(number, language),
            notify: vec![Notify::Approvers(texts::void_request_notice(
                &actor.name,
                request,
                &line,
                reason,
                language,
            ))],
        })
    }

    /// Void requests waiting for a decision (owner and managers only).
    pub async fn pending_voids(&self, actor: &Actor) -> Result<String, OrchestratorError> {
        let language = self.business.language.as_str();
        if !actor.role.can(Action::DecideVoids) {
            return Ok(texts::not_allowed(language).into());
        }
        let pending = self.store.pending_void_requests(self.business.id).await?;
        if pending.is_empty() {
            return Ok(texts::nothing_pending(language).into());
        }
        let mut lines = Vec::new();
        for r in pending {
            let entry = match self.store.journal_entry(self.business.id, r.entry).await? {
                Some(e) => self.entry_line(&e),
                None => format!("#{}", r.entry),
            };
            lines.push(texts::pending_line(
                r.number,
                &r.requested_by,
                &r.reason,
                &entry,
                language,
            ));
        }
        Ok(lines.join("\n\n"))
    }

    /// Approve or reject void request `number`.
    pub async fn decide_void(
        &self,
        actor: &Actor,
        number: i64,
        approve: bool,
    ) -> Result<Outcome, OrchestratorError> {
        let language = self.business.language.as_str();
        if !actor.role.can(Action::DecideVoids) {
            return Ok(Outcome::reply(texts::not_allowed(language)));
        }
        let pending = self.store.pending_void_requests(self.business.id).await?;
        let Some(request) = pending.into_iter().find(|r| r.number == number) else {
            return Ok(Outcome::reply(texts::no_such_request(number, language)));
        };
        let entry = self
            .store
            .journal_entry(self.business.id, request.entry)
            .await?;
        let author_role = entry.as_ref().and_then(|e| e.recorded_by_role);
        // No approving your own request, and no overruling the owner.
        let own = request.requested_by_id == actor.id;
        if (own && actor.role != guss_core::people::Role::Owner)
            || !actor.role.outranks_for_voids(author_role)
        {
            return Ok(Outcome::reply(texts::not_allowed(language)));
        }
        self.store
            .decide_void_request(self.business.id, number, actor, approve)
            .await?;
        if approve && let Some(e) = &entry {
            let reason = format!("{} ({})", request.reason, request.requested_by);
            self.store.void_event(e.event.id(), actor, &reason).await?;
        }
        let reply = texts::decided(number, request.entry, approve, language);
        Ok(Outcome {
            notify: vec![Notify::Person(
                request.requested_by_id.clone(),
                texts::decision_notice(request.entry, approve, &actor.name, language),
            )],
            reply,
        })
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
        let actor = Actor::owner(source_channel_tag(source_channel));
        self.handle_inbound_as(
            &actor,
            conversation_key,
            source_channel,
            provider_message_id,
            text,
        )
        .await
    }

    /// `handle_inbound` for a known person (their role decides what they
    /// may do, and entries are recorded under their name).
    pub async fn handle_inbound_as(
        &self,
        actor: &Actor,
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
        self.handle_message_as(actor, conversation_key, source_channel, text)
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
            let what = quantity_text(s.quantity, s.unit.as_deref(), s.unit_factor, &s.item_name);
            match language {
                "id" => format!("Terjual: {what} seharga {} = {}", s.unit_price, total),
                _ => format!("Sold: {what} at {} = {}", s.unit_price, total),
            }
        }
        BusinessEvent::Expense(e) => match language {
            "id" => format!("Pengeluaran: {} sebesar {}", e.description, e.amount),
            _ => format!("Expense: {} for {}", e.description, e.amount),
        },
        BusinessEvent::StockUpdate(su) => match (&su.unit_cost, language) {
            // A paid restock shows the arithmetic, so a wrong number is
            // caught at a glance: "+30 at 650.00 KES = 19500.00 KES paid".
            (Some(cost), lang) if su.delta > 0.0 => {
                let paid =
                    CurrencyAmount::new(su.delta * cost.major_units(), cost.currency.as_str());
                if lang == "id" {
                    format!(
                        "Stok {}: {:+} @ {} = {} dibayar",
                        su.item_name, su.delta, cost, paid
                    )
                } else {
                    format!(
                        "Stock {}: {:+} at {} = {} paid",
                        su.item_name, su.delta, cost, paid
                    )
                }
            }
            (_, "id") => format!("Stok {}: {:+}", su.item_name, su.delta),
            _ => format!("Stock {}: {:+}", su.item_name, su.delta),
        },
        BusinessEvent::Note(n) => match language {
            "id" => format!("Catatan (tidak dihitung): {}", n.text),
            _ => format!("Note (not counted): {}", n.text),
        },
    }
}

/// "3 rice", "2 crate soda (= 48)": the base-unit count only when a
/// conversion happened, so the owner can check it.
fn quantity_text(quantity: f64, unit: Option<&str>, factor: f64, item: &str) -> String {
    match unit {
        Some(u) if (factor - 1.0).abs() > f64::EPSILON => {
            format!("{quantity} {u} {item} (= {})", quantity * factor)
        }
        Some(u) => format!("{quantity} {u} {item}"),
        None => format!("{quantity} {item}"),
    }
}

fn unit_dropped_note(item: &str, language: &str) -> String {
    match language {
        "id" => format!("(Catatan {item} sebelumnya tidak disimpan.)"),
        _ => format!("(The earlier {item} entry was not saved.)"),
    }
}

fn issues_reply(issues: &[guss_nlu::Issue], language: &str) -> String {
    let lines: Vec<String> = issues.iter().map(|i| i.message(language)).collect();
    let body = lines.join("\n");
    match language {
        "id" => format!("{body}\n\nBelum ada yang disimpan. Kirim lagi dengan angka yang benar."),
        _ => format!("{body}\n\nNothing was saved. Send it again with the right numbers."),
    }
}

/// When a message only produced notes, say plainly that nothing was
/// counted, with an example of how to say it so it is.
fn notes_only_hint(language: &str) -> &'static str {
    match language {
        "id" => {
            "Ini hanya disimpan sebagai catatan, tidak masuk penjualan atau pengeluaran. \
             Kalau ini penjualan atau pembelian, kirim lagi, misalnya \"jual 3 beras 12000\" \
             atau \"beli 30 kaos 65rb per biji\"."
        }
        _ => {
            "Saved only as a note, not counted in sales or expenses. If it was a sale or a \
             purchase, send it again, e.g. \"sold 3 rice at 120\" or \"bought 30 tshirts at 650 each\"."
        }
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

    #[tokio::test]
    async fn paid_restock_shows_the_arithmetic() {
        let o = orchestrator(
            json!({"events": [{"kind": "stock_update", "item_name": "tshirt", "delta": 30.0,
                "unit": null, "unit_cost_major": 650.0, "total_cost_major": null,
                "reorder_threshold": null}]}),
            "en",
            OtherCurrencyPolicy::Ask,
        )
        .await;
        let reply = say(&o, "Bought 30 tshirts 650 each").await;
        assert!(
            reply.starts_with("Stock tshirt: +30 at 650.00 KES = 19500.00 KES paid"),
            "{reply}"
        );
    }

    #[tokio::test]
    async fn wrong_total_saves_nothing_and_says_why() {
        let o = orchestrator(
            json!({"events": [
                {"kind": "expense", "description": "delivery", "amount_major": 500.0,
                 "quantity": null, "unit_amount_major": null, "currency": null, "category": null},
                {"kind": "stock_update", "item_name": "tshirt", "delta": 30.0, "unit": null,
                 "unit_cost_major": 650.0, "total_cost_major": 18500.0, "reorder_threshold": null}]}),
            "en",
            OtherCurrencyPolicy::Ask,
        )
        .await;
        let reply = say(&o, "paid delivery 500, bought 30 tshirts at 650 = 18500").await;
        assert!(reply.contains("30 x 650 = 19500, not 18500"), "{reply}");
        assert!(reply.contains("Nothing was saved"), "{reply}");
        let events = o
            .store
            .list_events(o.business.id, TimeWindow::Day)
            .await
            .unwrap();
        assert!(events.is_empty(), "the delivery must not be half-saved");
    }

    #[tokio::test]
    async fn a_note_alone_says_it_was_not_counted() {
        let o = orchestrator(
            json!({"events": [{"kind": "note", "text": "nought 30 tshirts"}]}),
            "en",
            OtherCurrencyPolicy::Ask,
        )
        .await;
        let reply = say(&o, "nought 30 tshirts").await;
        assert!(
            reply.starts_with("Note (not counted): nought 30 tshirts"),
            "{reply}"
        );
        assert!(
            reply.contains("not counted in sales or expenses"),
            "{reply}"
        );
    }

    /// Answers each extraction with the next scripted response.
    struct Script(std::sync::Mutex<Vec<Value>>);

    #[async_trait]
    impl LlmProvider for Script {
        async fn complete_json(&self, _req: JsonCompletionRequest) -> Result<Value, LlmError> {
            Ok(self.0.lock().unwrap().remove(0))
        }
    }

    #[tokio::test]
    async fn unknown_unit_is_asked_once_then_remembered() {
        let restock_bottles = json!({"events": [{"kind": "stock_update", "item_name": "Sodas",
            "delta": 10.0, "unit": "bottles", "unit_cost_major": null, "total_cost_major": null,
            "reorder_threshold": null}]});
        let sold_crates = json!({"events": [{"kind": "sale", "item_name": "soda", "quantity": 2.0,
            "unit": "crates", "unit_price_major": 1200.0, "total_major": null,
            "currency": null, "category": null}]});
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let business = BusinessProfile {
            id: Uuid::new_v4(),
            name: "Duka".into(),
            default_currency: "KES".into(),
            language: "en".into(),
            timezone: "UTC".into(),
        };
        let script = Script(std::sync::Mutex::new(vec![
            restock_bottles,
            sold_crates.clone(),
            sold_crates,
        ]));
        let o = Orchestrator::new(Nlu::new(script, "en"), store.clone(), business.clone());
        let say = |text: &'static str| o.handle_message("chat", SourceChannel::Telegram, text);

        say("got 10 bottles of soda").await.unwrap();
        let reply = say("sold 2 crates of soda at 1200").await.unwrap();
        assert!(
            reply.starts_with("How many bottle in one crate of soda?"),
            "{reply}"
        );
        assert_eq!(
            store.current_stock(business.id, "soda").await.unwrap(),
            10.0
        );

        let reply = say("24").await.unwrap();
        assert!(
            reply.starts_with("Sold: 2 crate soda (= 48) at 1200.00 KES = 2400.00 KES"),
            "{reply}"
        );
        assert_eq!(
            store.current_stock(business.id, "soda").await.unwrap(),
            -38.0
        );

        // Known now: no question the second time.
        let reply = say("sold 2 crates of soda at 1200").await.unwrap();
        assert!(reply.starts_with("Sold: 2 crate soda (= 48)"), "{reply}");
        let items = store.list_items(business.id).await.unwrap();
        assert_eq!(items[0].name, "soda");
        assert_eq!(items[0].base_unit.as_deref(), Some("bottle"));
        assert_eq!(items[0].units, vec![("crate".to_string(), 24.0)]);
    }

    fn person(id: &str, role: guss_core::people::Role) -> Actor {
        Actor::new(format!("telegram:{id}"), id, role)
    }

    async fn record_as(o: &Orchestrator<FixedProvider, SqliteStore>, who: &Actor) {
        o.handle_message_as(who, &who.id, SourceChannel::Telegram, "sold 3 rice at 120")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn undo_voids_and_keeps_the_entry() {
        let o = orchestrator(kes_sale(), "en", OtherCurrencyPolicy::Ask).await;
        say(&o, "sold 3 rice at 120").await;
        assert_eq!(say(&o, "no").await, "Undone.");
        assert!(
            saved_sales(&o).await.is_empty(),
            "voided entries don't count"
        );
        let journal = o.store.journal(o.business.id, 10).await.unwrap();
        assert_eq!(journal.len(), 1, "but the entry is kept");
        assert_eq!(journal[0].voided.as_ref().unwrap().reason, "undone");
        let history = o.history(10).await.unwrap();
        assert!(
            history.contains("[VOIDED by telegram: undone]"),
            "{history}"
        );
    }

    #[tokio::test]
    async fn staff_void_is_a_request_a_manager_approves() {
        use guss_core::people::Role;
        let o = orchestrator(kes_sale(), "en", OtherCurrencyPolicy::Ask).await;
        let mary = person("mary", Role::Staff);
        let joe = person("joe", Role::Manager);
        record_as(&o, &mary).await;
        let entry = o.store.journal(o.business.id, 1).await.unwrap()[0].number;
        assert!(o.history(5).await.unwrap().contains("(mary)"));

        let asked = o.void_entry(&mary, entry, "recorded twice").await.unwrap();
        assert!(
            asked
                .reply
                .starts_with(&format!("Asked for #{entry} to be voided")),
            "{}",
            asked.reply
        );
        match &asked.notify[..] {
            [Notify::Approvers(text)] => {
                assert!(
                    text.contains("mary asks to void this entry (request 1)"),
                    "{text}"
                );
                assert!(text.contains("Reason: recorded twice"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            saved_sales(&o).await.len(),
            1,
            "still counts until approved"
        );

        // Staff can't approve, not even someone else's request.
        let nope = o.decide_void(&mary, 1, true).await.unwrap();
        assert_eq!(
            nope.reply,
            "Sorry, only the owner or a manager can do that."
        );

        assert!(
            o.pending_voids(&joe)
                .await
                .unwrap()
                .contains("Request 1 from mary")
        );
        let done = o.decide_void(&joe, 1, true).await.unwrap();
        assert_eq!(
            done.reply,
            format!("Approved: #{entry} is voided (request 1).")
        );
        assert_eq!(
            done.notify,
            vec![Notify::Person(
                "telegram:mary".into(),
                format!("joe approved: #{entry} is voided.")
            )]
        );
        assert!(saved_sales(&o).await.is_empty());
        assert_eq!(
            o.pending_voids(&joe).await.unwrap(),
            "No void requests are waiting."
        );
    }

    #[tokio::test]
    async fn managers_cannot_overrule_the_owner() {
        use guss_core::people::Role;
        let o = orchestrator(kes_sale(), "en", OtherCurrencyPolicy::Ask).await;
        let boss = person("boss", Role::Owner);
        let joe = person("joe", Role::Manager);
        record_as(&o, &boss).await;
        let entry = o.store.journal(o.business.id, 1).await.unwrap()[0].number;

        // A manager voiding the owner's entry only asks...
        let asked = o.void_entry(&joe, entry, "wrong price").await.unwrap();
        assert!(asked.reply.starts_with("Asked for"), "{}", asked.reply);
        // ...and can't approve it (it's theirs, and it's the owner's entry).
        let nope = o.decide_void(&joe, 1, true).await.unwrap();
        assert_eq!(
            nope.reply,
            "Sorry, only the owner or a manager can do that."
        );
        // The owner can say no; the entry keeps counting.
        let no = o.decide_void(&boss, 1, false).await.unwrap();
        assert_eq!(
            no.reply,
            format!("Rejected: #{entry} still counts (request 1).")
        );
        assert_eq!(saved_sales(&o).await.len(), 1);

        // The owner voids directly.
        let done = o.void_entry(&boss, entry, "wrong price").await.unwrap();
        assert!(done.reply.starts_with("Voided: #"), "{}", done.reply);
        assert!(done.notify.is_empty());
        let again = o.void_entry(&boss, entry, "x").await.unwrap();
        assert_eq!(again.reply, format!("#{entry} was already voided by boss."));
    }

    #[tokio::test]
    async fn a_manager_voids_staff_entries_directly() {
        use guss_core::people::Role;
        let o = orchestrator(kes_sale(), "en", OtherCurrencyPolicy::Ask).await;
        record_as(&o, &person("mary", Role::Staff)).await;
        let entry = o.store.journal(o.business.id, 1).await.unwrap()[0].number;
        let done = o
            .void_entry(&person("joe", Role::Manager), entry, "")
            .await
            .unwrap();
        assert!(done.reply.starts_with("Voided:"), "{}", done.reply);
        let journal = o.store.journal(o.business.id, 1).await.unwrap();
        assert_eq!(
            journal[0].voided.as_ref().unwrap().reason,
            "no reason given"
        );
        assert_eq!(
            o.void_entry(&person("joe", Role::Manager), 999, "x")
                .await
                .unwrap()
                .reply,
            "There's no entry #999. See /history."
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
