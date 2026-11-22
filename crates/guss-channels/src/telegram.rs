use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use guss_core::domain::SourceChannel;
use guss_core::people::{Action, Actor, Role};
use guss_core::store::{Store, TimeWindow};
use guss_llm::LlmProvider;
use guss_scheduler::ReportSink;
use guss_voice::AudioFormat;
use teloxide::RequestError;
use teloxide::prelude::*;
use teloxide::types::{InputFile, UpdateKind};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::friday::FridayClient;
use crate::orchestrator::{Notify, Orchestrator, Outcome};
use crate::telegram_ux::{self, Command, TelegramAccess};
use crate::voice_support::VoiceSupport;

/// Delivers scheduled (EOD/weekly/monthly) reports to a fixed Telegram
/// chat - the business owner's own chat with the bot, configured once
/// via GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID.
pub struct TelegramReportSink {
    bot: Bot,
    chat_id: ChatId,
}

impl TelegramReportSink {
    pub fn new(bot_token: &str, chat_id: i64) -> Self {
        Self {
            bot: new_bot(bot_token),
            chat_id: ChatId(chat_id),
        }
    }
}

#[async_trait]
impl ReportSink for TelegramReportSink {
    async fn send_report(&self, text: String) -> Result<(), String> {
        send_text(&self.bot, self.chat_id, &text)
            .await
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Error)]
pub enum TelegramError {
    #[error("Telegram API error: {0}")]
    Api(String),
}

/// How many inbound messages can be processed concurrently. Bounds memory
/// and outbound LLM/TTS calls under a message burst, rather than spawning
/// one task per update unboundedly.
const MAX_CONCURRENT_MESSAGES: usize = 8;

/// Runs the Telegram channel via long polling. Each update is deduplicated
/// by its message id (`Orchestrator::handle_inbound`) before processing,
/// since Telegram can redeliver updates on timeout - and processing is
/// capped at `MAX_CONCURRENT_MESSAGES` concurrent tasks via a semaphore
/// acquired before each spawn, so a burst of messages can't spawn
/// unboundedly many tasks.
const LONG_POLL_SECS: u32 = 30;

// teloxide's default client gives up after 17 s, shorter than the 30 s
// long poll: with a valid token every quiet poll timed out and Guss
// exited and restarted every ~18 s. The client must outlast the poll.
fn new_bot(token: &str) -> Bot {
    let client = teloxide::net::default_reqwest_settings()
        .timeout(Duration::from_secs(u64::from(LONG_POLL_SECS) + 15))
        .build()
        .expect("reqwest client builds with static settings");
    Bot::with_client(token, client)
}

fn is_transient(e: &RequestError) -> bool {
    matches!(
        e,
        RequestError::Network(_) | RequestError::Io(_) | RequestError::RetryAfter(_)
    )
}

fn backoff(failures: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(failures.min(5)).min(30))
}

pub async fn run<P, S>(
    orchestrator: Arc<Orchestrator<P, S>>,
    voice: Arc<VoiceSupport>,
    bot_token: &str,
    access: TelegramAccess,
    friday: Option<Arc<FridayClient>>,
) -> Result<(), TelegramError>
where
    P: LlmProvider + Send + Sync + 'static,
    S: Store + Send + Sync + 'static,
{
    let bot = new_bot(bot_token);
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_MESSAGES));
    let mut tasks = JoinSet::new();
    let mut offset: i32 = 0;

    tracing::info!("Telegram channel starting (long polling)");
    // Friday's "Link my Telegram" button needs this bot's username.
    if let Some(f) = friday.as_ref() {
        match bot.get_me().await {
            Ok(me) => {
                if let Some(username) = me.user.username.as_deref()
                    && let Err(e) = f.report_bot(username).await
                {
                    tracing::warn!(error = %e, "couldn't tell Friday the bot's username");
                }
            }
            Err(e) => tracing::warn!(error = %e, "couldn't read the bot's username"),
        }
    }
    if access.is_open() {
        tracing::warn!(
            "No Telegram owner or allowed users set: anyone who finds this bot can record entries.              Set GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID and/or GUSS_CHANNEL__TELEGRAM_ALLOWED_USER_IDS."
        );
    }
    {
        let business = orchestrator.business();
        telegram_ux::apply_profile(&bot, &business.name, &business.language).await;
    }
    let access = Arc::new(access);

    let mut failures: u32 = 0;
    loop {
        let updates = match bot
            .get_updates()
            .offset(offset)
            .timeout(LONG_POLL_SECS)
            .await
        {
            Ok(updates) => {
                failures = 0;
                updates
            }
            Err(e) if is_transient(&e) => {
                failures += 1;
                let wait = backoff(failures);
                tracing::warn!(error = %e, retry_in_secs = wait.as_secs(), "Telegram unreachable, retrying");
                tokio::time::sleep(wait).await;
                continue;
            }
            // A wrong token and other API refusals stay fatal, so a bad
            // setting shows up at startup instead of being retried forever.
            Err(e) => return Err(TelegramError::Api(e.to_string())),
        };

        for update in updates {
            offset = offset.max(update.id.0 as i32 + 1);

            if let UpdateKind::Message(msg) = update.kind {
                let permit = semaphore
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("semaphore never closed");
                let orchestrator = orchestrator.clone();
                let voice = voice.clone();
                let bot = bot.clone();
                let access = access.clone();
                let friday = friday.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    if let Err(e) =
                        handle_message(&orchestrator, &voice, &bot, &access, friday.as_deref(), msg)
                            .await
                    {
                        tracing::warn!(error = %e, "failed to handle Telegram message");
                    }
                });
            }
        }

        // Drain finished tasks so the JoinSet doesn't grow unboundedly
        // across polling iterations.
        while tasks.try_join_next().is_some() {}
    }
}

/// "/paylink John": asks Friday for a Paystack page for everything John
/// owes, as the person asking, and leaves a note so friday_sync checks
/// it until John pays (then records "John paid").
async fn pay_link<P, S>(
    orchestrator: &Orchestrator<P, S>,
    friday: &FridayClient,
    actor: &Actor,
    user_id: i64,
    customer: &str,
    language: &str,
) -> String
where
    P: LlmProvider,
    S: Store,
{
    let token = match friday.token_for(user_id).await {
        Ok(Some(t)) => t,
        _ => return telegram_ux::link_first(language, &orchestrator.business().name),
    };
    let reply = match friday
        .post(
            &token,
            "/api/invoicing/pos/debts/paystack-link",
            &serde_json::json!({ "customer_name": customer }),
        )
        .await
    {
        Ok(v) => v,
        Err(crate::friday::FridayError::Status { body, .. }) => {
            return telegram_ux::paylink_failed(language, &friday_reason(&body));
        }
        Err(crate::friday::FridayError::Forbidden(why)) => {
            return telegram_ux::paylink_failed(language, &why);
        }
        Err(e) => {
            tracing::warn!(error = %e, "couldn't get a pay link");
            return telegram_ux::paylink_failed(language, "Friday didn't answer; try again");
        }
    };
    let (Some(reference), Some(url)) =
        (reply["reference"].as_str(), reply["redirect_url"].as_str())
    else {
        return telegram_ux::paylink_failed(language, "Friday's answer was incomplete");
    };
    let who = reply["customer"].as_str().unwrap_or(customer);
    let owed = guss_core::money::CurrencyAmount::new(
        reply["owed_cents"].as_i64().unwrap_or(0) as f64 / 100.0,
        reply["currency"].as_str().unwrap_or("KES"),
    );
    if let Err(e) = orchestrator
        .note_pay_link(
            actor,
            &format!("Paystack pay link for {who}: {owed}"),
            reference,
        )
        .await
    {
        tracing::warn!(error = %e, "couldn't keep the pay link note");
    }
    telegram_ux::paylink_sent(language, who, &owed.to_string(), url)
}

/// Friday's {"detail": "..."} reason, or the body as it came.
fn friday_reason(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["detail"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect())
}

static MANAGER_MENUS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<i64>>> =
    std::sync::LazyLock::new(Default::default);

/// What Guss says about a scanned code: the product, as the person may see
/// it in Friday, remembered so "sold 2" means that product.
async fn scanned<P: guss_llm::LlmProvider, S: guss_core::store::Store>(
    friday: Option<&FridayClient>,
    orchestrator: &crate::Orchestrator<P, S>,
    actor: &Actor,
    conversation_key: &str,
    code: &str,
    language: &str,
) -> String {
    let code = code.trim();
    let business = orchestrator.business();
    let Some(f) = friday else {
        return format!("Scanned: {code}");
    };
    let token = match f.token_for_actor(&actor.id).await {
        Ok(Some(t)) => t,
        Ok(None) => return telegram_ux::link_first(language, &business.name),
        Err(e) => {
            tracing::warn!(error = %e, "couldn't get a Friday token for a scan");
            return format!("Scanned: {code} (Friday didn't answer; try again)");
        }
    };
    match crate::scan::find_product(f, &token, code).await {
        Ok(Some(p)) => {
            let name = p
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(code)
                .to_string();
            orchestrator.note_scanned(conversation_key, &name).await;
            format!(
                "{}\nSay \"sold 2\" or \"got 10\" to record it.",
                crate::scan::product_line(&p, &business.default_currency)
            )
        }
        Ok(None) => format!(
            "No product in Friday has the code {code}. Add it in Inventory (barcode field), or tell me what it is."
        ),
        // No Inventory access for this person or for Guss: say so plainly.
        Err(crate::friday::FridayError::Forbidden(_)) => {
            format!("Scanned: {code}. You or Guss can't read Inventory in Friday.")
        }
        Err(e) => {
            tracing::warn!(error = %e, "couldn't look up a scanned code");
            format!("Scanned: {code} (Friday didn't answer; try again)")
        }
    }
}

async fn handle_message<P, S>(
    orchestrator: &Orchestrator<P, S>,
    voice: &VoiceSupport,
    bot: &Bot,
    access: &TelegramAccess,
    friday: Option<&FridayClient>,
    msg: teloxide::types::Message,
) -> Result<(), TelegramError>
where
    P: LlmProvider,
    S: Store,
{
    let chat_id = msg.chat.id;
    let conversation_key = chat_id.0.to_string();
    let provider_message_id = msg.id.0.to_string();
    let language = orchestrator.business().language.clone();
    let business_name = orchestrator.business().name.clone();

    // Groups would let anyone in them write to the books.
    if !msg.chat.is_private() {
        send_text(bot, chat_id, &telegram_ux::private_chats_only(&language)).await?;
        return Ok(());
    }
    let Some(user) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = user.id.0 as i64;
    let command = msg.text().and_then(telegram_ux::parse_command);

    if command == Some(Command::MyId) {
        send_text(bot, chat_id, &telegram_ux::your_id(&language, user_id)).await?;
        return Ok(());
    }
    // The one-tap link from Friday ("Link my Telegram"): before the role
    // check, since this is what gives them one.
    if let Some(Command::StartLink(code)) = &command {
        let reply = match friday {
            None => telegram_ux::link_unavailable(&language),
            Some(f) => match f.redeem(code, user_id, &user.first_name).await {
                Ok(_) => {
                    f.refresh().await;
                    tracing::info!(user_id, "linked from Friday");
                    telegram_ux::linked_ok(&language, &business_name)
                }
                Err(crate::friday::FridayError::Status { body, .. }) => {
                    telegram_ux::linked_failed(&language, &friday_reason(&body))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't redeem a link from Friday");
                    telegram_ux::linked_failed(&language, "Friday didn't answer; try again")
                }
            },
        };
        send_text(bot, chat_id, &reply).await?;
        return Ok(());
    }
    if command == Some(Command::Link) {
        let reply = match friday {
            None => telegram_ux::link_unavailable(&language),
            Some(f) => match f.link_code(user_id, &user.first_name).await {
                Ok(code) => {
                    tracing::info!(user_id, "link code sent");
                    f.refresh().await;
                    telegram_ux::link_code(&language, &code)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "couldn't get a Friday link code");
                    telegram_ux::link_failed(&language)
                }
            },
        };
        send_text(bot, chat_id, &reply).await?;
        return Ok(());
    }
    let Some(role) = role_of(access, friday, user_id).await else {
        if let Some(f) = friday {
            let reply = if linked_without_role(f, user_id).await {
                telegram_ux::role_cant_use_guss(&language)
            } else {
                telegram_ux::link_first(&language, &business_name)
            };
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        tracing::info!(user_id, "refused a message from a user who isn't allowed");
        send_text(
            bot,
            chat_id,
            &telegram_ux::private_bot(&language, &business_name, user_id),
        )
        .await?;
        return Ok(());
    };
    let name = match &user.last_name {
        Some(last) => format!("{} {last}", user.first_name),
        None => user.first_name.clone(),
    };
    let actor = Actor::new(format!("telegram:{user_id}"), name, role);
    // Owners' and managers' own chats get their fuller menu, once per run.
    if role >= Role::Manager
        && MANAGER_MENUS
            .lock()
            .map(|mut m| m.insert(user_id))
            .unwrap_or(false)
    {
        telegram_ux::apply_manager_menu(bot, chat_id, &language).await;
    }
    let api = |e: crate::OrchestratorError| TelegramError::Api(e.to_string());

    // A code from the scan page (plan 16.3).
    if let Some(data) = msg.web_app_data() {
        let reply = scanned(
            friday,
            orchestrator,
            &actor,
            &conversation_key,
            &data.data,
            &language,
        )
        .await;
        send_text(bot, chat_id, &reply).await?;
        return Ok(());
    }

    match command {
        Some(Command::Start) => {
            let reply = telegram_ux::welcome(&language, &business_name, &user.first_name);
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Help) => {
            send_text(bot, chat_id, &telegram_ux::help(&language)).await?;
            return Ok(());
        }
        Some(Command::Report(arg)) => {
            let reply = match telegram_ux::report_window(arg.as_deref()) {
                Some(window) if window != TimeWindow::Day && !role.can(Action::ReportLonger) => {
                    crate::journal_texts::longer_reports_not_allowed(&language).to_string()
                }
                Some(window) => orchestrator
                    .handle_report_for(&actor, window)
                    .await
                    .map_err(api)?,
                None => telegram_ux::bad_report_window(&language),
            };
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::History) => {
            let reply = orchestrator.history(15).await.map_err(api)?;
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Scan) => {
            let url = std::env::var("GUSS_CHANNEL__TELEGRAM_SCAN_URL").unwrap_or_default();
            match telegram_ux::scan_keyboard(url.trim(), &language) {
                Some(keyboard) => {
                    bot.send_message(chat_id, telegram_ux::scan_prompt(&language))
                        .reply_markup(keyboard)
                        .await
                        .map_err(|e| TelegramError::Api(e.to_string()))?;
                }
                None => send_text(bot, chat_id, telegram_ux::scan_not_set_up(&language)).await?,
            }
            return Ok(());
        }
        Some(Command::Pending) => {
            let reply = orchestrator.pending_voids(&actor).await.map_err(api)?;
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Void(arg)) => {
            let Some((number, reason)) = telegram_ux::number_and_rest(arg.as_deref()) else {
                send_text(bot, chat_id, &telegram_ux::void_usage(&language)).await?;
                return Ok(());
            };
            let outcome = orchestrator
                .void_entry(&actor, number, &reason)
                .await
                .map_err(api)?;
            deliver(bot, access, friday, chat_id, outcome).await?;
            return Ok(());
        }
        Some(Command::Approve(ref arg)) | Some(Command::Reject(ref arg)) => {
            let approve = matches!(command, Some(Command::Approve(_)));
            let Some((number, _)) = telegram_ux::number_and_rest(arg.as_deref()) else {
                send_text(bot, chat_id, &telegram_ux::decide_usage(&language)).await?;
                return Ok(());
            };
            let outcome = orchestrator
                .decide_void(&actor, number, approve)
                .await
                .map_err(api)?;
            deliver(bot, access, friday, chat_id, outcome).await?;
            return Ok(());
        }
        Some(Command::PayLink(arg)) => {
            let reply = match (arg.as_deref(), friday) {
                (None, _) => telegram_ux::paylink_usage(&language),
                (Some(_), None) => telegram_ux::link_unavailable(&language),
                (Some(customer), Some(f)) => {
                    pay_link(orchestrator, f, &actor, user_id, customer, &language).await
                }
            };
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Retry(arg)) => {
            let reply = match telegram_ux::number_and_rest(arg.as_deref()) {
                None => telegram_ux::retry_usage(&language),
                Some((number, _)) => orchestrator.resend(&actor, number).await.map_err(api)?,
            };
            send_text(bot, chat_id, &reply).await?;
            return Ok(());
        }
        Some(Command::Unknown) => {
            send_text(bot, chat_id, &telegram_ux::unknown_command(&language)).await?;
            return Ok(());
        }
        Some(Command::Undo)
        | Some(Command::MyId)
        | Some(Command::Link)
        | Some(Command::StartLink(_))
        | None => {}
    }
    let undo_text = (command == Some(Command::Undo)).then(|| "undo".to_string());

    let text = if let Some(undo) = undo_text {
        undo
    } else if let Some(text) = msg.text() {
        text.to_string()
    } else if let Some(telegram_voice) = msg.voice() {
        match transcribe_voice_note(voice, bot, telegram_voice.file.id.clone()).await {
            Ok(transcript) => transcript,
            Err(e) => {
                send_text(
                    bot,
                    chat_id,
                    &format!("Couldn't understand that voice note: {e}"),
                )
                .await?;
                return Ok(());
            }
        }
    } else {
        send_text(
            bot,
            chat_id,
            "I can only read text or voice notes right now.",
        )
        .await?;
        return Ok(());
    };

    let reply = match orchestrator
        .handle_inbound_as(
            &actor,
            &conversation_key,
            SourceChannel::Telegram,
            &provider_message_id,
            &text,
        )
        .await
    {
        Ok(reply) => reply,
        // Say so instead of going quiet: the person must know nothing was
        // recorded.
        Err(e) => {
            tracing::warn!(error = %e, "couldn't handle a Telegram message");
            Some(e.user_message(&language))
        }
    };

    let Some(reply_text) = reply else {
        return Ok(()); // redelivered update, already handled
    };

    send_text(bot, chat_id, &reply_text).await?;

    if let Some(tts) = &voice.tts {
        match tts.synthesize(&reply_text).await {
            Ok(clip) => {
                let file_name = match clip.format {
                    AudioFormat::Wav => "reply.wav",
                    AudioFormat::Mp3 => "reply.mp3",
                };
                // Sent via send_audio rather than send_voice: Telegram's
                // "voice message" bubble requires OGG/Opus specifically,
                // which Guss's TTS backends don't produce - this plays as
                // a normal audio attachment instead.
                if let Err(e) = bot
                    .send_audio(chat_id, InputFile::memory(clip.bytes).file_name(file_name))
                    .await
                {
                    tracing::warn!(error = %e, "failed to send TTS audio to Telegram");
                }
            }
            Err(e) => tracing::warn!(error = %e, "TTS synthesis failed for Telegram reply"),
        }
    }

    Ok(())
}

async fn transcribe_voice_note(
    voice: &VoiceSupport,
    bot: &Bot,
    file_id: teloxide::types::FileId,
) -> Result<String, TelegramError> {
    let Some(stt) = &voice.stt else {
        return Err(TelegramError::Api(
            "no speech-to-text provider is configured".to_string(),
        ));
    };

    let file = bot
        .get_file(file_id)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;
    let url = format!(
        "https://api.telegram.org/file/bot{}/{}",
        bot.token(),
        file.path
    );
    let bytes = reqwest::get(&url)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;

    let wav = guss_voice::convert::to_wav16k_mono(&bytes)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;
    stt.transcribe(&wav)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))
}

/// Who this Telegram user is. With Friday connected, Friday decides: a
/// linked person has their Friday role (a viewer can't use Guss), and an
/// unlinked one must /link first. The owner set in Guss's own settings
/// always gets in, and if Friday can't be reached the configured ids
/// apply, so nobody is locked out by an outage.
async fn role_of(
    access: &TelegramAccess,
    friday: Option<&FridayClient>,
    user_id: i64,
) -> Option<Role> {
    let Some(friday) = friday else {
        return access.role_of(user_id);
    };
    if access.owner_chat_id == Some(user_id) {
        return Some(Role::Owner);
    }
    match friday.people().await {
        Ok(people) if people.by_telegram.contains_key(&user_id) => {
            people.by_telegram.get(&user_id).and_then(|p| p.role)
        }
        // Not in the cached list: they may have just entered their code
        // in Friday, so ask again rather than say "link first" for a minute.
        Ok(_) => {
            friday.refresh().await;
            friday
                .people()
                .await
                .ok()
                .and_then(|p| p.by_telegram.get(&user_id).and_then(|p| p.role))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Friday unreachable; using the configured Telegram ids");
            access.role_of(user_id)
        }
    }
}

/// Linked in Friday, but with a role that can't use Guss (a viewer).
async fn linked_without_role(friday: &FridayClient, user_id: i64) -> bool {
    friday.people().await.is_ok_and(|p| {
        p.by_telegram
            .get(&user_id)
            .is_some_and(|p| p.role.is_none())
    })
}

/// Who hears about void requests: the owner and managers typed into
/// Guss's settings, and every owner and manager linked through Friday
/// (plan 16.1: linked owners never got them).
async fn approvers(access: &TelegramAccess, friday: Option<&FridayClient>) -> Vec<i64> {
    let mut ids = access.approver_chats();
    if let Some(f) = friday
        && let Ok(people) = f.people().await
    {
        for p in people.by_telegram.values() {
            if matches!(p.role, Some(Role::Owner | Role::Manager)) && !ids.contains(&p.telegram_id)
            {
                ids.push(p.telegram_id);
            }
        }
    }
    ids
}

/// Sends the reply, then the notices for other people: void requests to
/// the owner and managers (not back to the person asking), decisions to
/// the person who asked. A notice that can't be delivered is logged, never
/// fatal: the request is stored and visible in /pending either way.
async fn deliver(
    bot: &Bot,
    access: &TelegramAccess,
    friday: Option<&FridayClient>,
    chat_id: ChatId,
    outcome: Outcome,
) -> Result<(), TelegramError> {
    send_text(bot, chat_id, &outcome.reply).await?;
    for notice in outcome.notify {
        let (targets, text) = match notice {
            Notify::Approvers(text) => (approvers(access, friday).await, text),
            Notify::Person(id, text) => {
                match id.strip_prefix("telegram:").and_then(|n| n.parse().ok()) {
                    Some(n) => (vec![n], text),
                    None => continue,
                }
            }
        };
        for target in targets.into_iter().filter(|t| *t != chat_id.0) {
            if let Err(e) = send_text(bot, ChatId(target), &text).await {
                tracing::warn!(target, error = %e, "couldn't deliver a notice");
            }
        }
    }
    Ok(())
}

async fn send_text(bot: &Bot, chat_id: ChatId, text: &str) -> Result<(), TelegramError> {
    bot.send_message(chat_id, text)
        .await
        .map_err(|e| TelegramError::Api(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use guss_core::domain::BusinessProfile;
    use guss_llm::{JsonCompletionRequest, LlmError, LlmProvider};
    use guss_nlu::Nlu;
    use guss_store::SqliteStore;
    use serde_json::{Value, json};
    use teloxide::types::Message;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct EmptyProvider;

    #[async_trait]
    impl LlmProvider for EmptyProvider {
        async fn complete_json(&self, _req: JsonCompletionRequest) -> Result<Value, LlmError> {
            Ok(json!({"events": [{"kind": "note", "text": "noted"}]}))
        }
    }

    fn sample_message_json(message_id: i32, chat_id: i64, text: &str) -> Value {
        json!({
            "message_id": message_id,
            "date": 1700000000,
            "chat": {"id": chat_id, "type": "private", "first_name": "Test"},
            "from": {"id": chat_id, "is_bot": false, "first_name": "Test"},
            "text": text,
        })
    }

    async fn test_orchestrator() -> Orchestrator<EmptyProvider, SqliteStore> {
        let store = Arc::new(SqliteStore::connect("sqlite::memory:").await.unwrap());
        let business = BusinessProfile {
            id: uuid::Uuid::new_v4(),
            name: "Acme Shop".into(),
            default_currency: "USD".into(),
            language: "en".into(),
            timezone: "UTC".into(),
        };
        let nlu = Nlu::new(EmptyProvider, "en");
        Orchestrator::new(nlu, store, business)
    }

    #[tokio::test]
    async fn redelivered_message_is_not_replied_to_twice() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(wiremock::matchers::path_regex("(?i)sendmessage$"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": sample_message_json(1, 42, "reply")
            })))
            .expect(1) // exactly one send, even though handle_message runs twice on the same update
            .mount(&server)
            .await;

        let bot = Bot::new("TEST_TOKEN").set_api_url(server.uri().parse().unwrap());
        let orchestrator = test_orchestrator().await;
        let voice = VoiceSupport::default();

        let message: Message =
            serde_json::from_value(sample_message_json(7, 42, "slow day today")).unwrap();

        let access = TelegramAccess::default();
        handle_message(&orchestrator, &voice, &bot, &access, None, message.clone())
            .await
            .unwrap();
        handle_message(&orchestrator, &voice, &bot, &access, None, message)
            .await
            .unwrap();

        server.verify().await;
    }

    async fn sent_texts(server: &MockServer) -> Vec<String> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect()
    }

    #[tokio::test]
    async fn stranger_is_refused_and_nothing_is_recorded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": sample_message_json(1, 42, "reply")
            })))
            .mount(&server)
            .await;
        let bot = Bot::new("TEST_TOKEN").set_api_url(server.uri().parse().unwrap());
        let orchestrator = test_orchestrator().await;
        let access = TelegramAccess {
            manager_user_ids: vec![],
            owner_chat_id: Some(10),
            allowed_user_ids: vec![],
        };
        let message: Message =
            serde_json::from_value(sample_message_json(7, 42, "sold 3 rice at 120")).unwrap();

        handle_message(
            &orchestrator,
            &VoiceSupport::default(),
            &bot,
            &access,
            None,
            message,
        )
        .await
        .unwrap();

        let texts = sent_texts(&server).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("private") && texts[0].contains("42"),
            "{}",
            texts[0]
        );
    }

    #[tokio::test]
    async fn start_welcomes_an_allowed_user_without_recording() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ok": true,
                "result": sample_message_json(1, 42, "reply")
            })))
            .mount(&server)
            .await;
        let bot = Bot::new("TEST_TOKEN").set_api_url(server.uri().parse().unwrap());
        let orchestrator = test_orchestrator().await;
        let access = TelegramAccess {
            manager_user_ids: vec![],
            owner_chat_id: Some(42),
            allowed_user_ids: vec![],
        };
        let message: Message =
            serde_json::from_value(sample_message_json(8, 42, "/start")).unwrap();

        handle_message(
            &orchestrator,
            &VoiceSupport::default(),
            &bot,
            &access,
            None,
            message,
        )
        .await
        .unwrap();

        let texts = sent_texts(&server).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].starts_with("Hi Test!") && texts[0].contains("/report"),
            "{}",
            texts[0]
        );
    }
}

#[cfg(test)]
mod poll_tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(9), Duration::from_secs(30));
    }
}
