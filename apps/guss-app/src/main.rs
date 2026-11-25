mod config;
mod init;

use std::sync::Arc;

use clap::{Parser, Subcommand};
use config::AppConfig;
use guss_channels::{Orchestrator, OtherCurrencyPolicy, VoiceSupport, cli, telegram, web};
use guss_core::domain::BusinessProfile;
use guss_core::store::Store;
use guss_llm::OpenAiCompatibleProvider;
use guss_nlu::Nlu;
use guss_voice::Tts;
use guss_voice::tts::{ElevenLabsTts, PiperTts};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "guss", about = "A self-hosted voice/chat business co-pilot")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Interactive setup wizard - prints a .env file to stdout
    Init,
    /// Writes a consistent copy of the database to stdout (nightly
    /// backups: Friday Host stores it). Nothing else goes to stdout.
    Backup,
}

#[cfg(feature = "local-whisper")]
use guss_voice::{Stt, stt::WhisperStt};

fn build_schedule_config(scheduler: &config::SchedulerConfig) -> guss_scheduler::ScheduleConfig {
    let default = guss_scheduler::ScheduleConfig::default();
    guss_scheduler::ScheduleConfig {
        daily_cron: scheduler.daily_cron.clone().unwrap_or(default.daily_cron),
        weekly_cron: scheduler.weekly_cron.clone().unwrap_or(default.weekly_cron),
        monthly_cron: scheduler
            .monthly_cron
            .clone()
            .unwrap_or(default.monthly_cron),
    }
}

#[cfg_attr(not(feature = "local-whisper"), allow(unused_variables))]
fn build_voice_support(config: &config::AppConfig, language: &str) -> VoiceSupport {
    let tts: Option<Arc<dyn Tts>> = match config.tts.provider.as_str() {
        "elevenlabs" => match (
            &config.tts.elevenlabs_api_key,
            &config.tts.elevenlabs_voice_id,
        ) {
            (Some(key), Some(voice_id)) if !key.is_empty() && !voice_id.is_empty() => {
                Some(Arc::new(ElevenLabsTts::new(key.clone(), voice_id.clone())))
            }
            _ => {
                eprintln!(
                    "GUSS_TTS__PROVIDER=elevenlabs but elevenlabs_api_key/elevenlabs_voice_id are missing - voice replies disabled."
                );
                None
            }
        },
        _ => Some(Arc::new(PiperTts::new(
            config.tts.piper_binary.clone(),
            config.tts.piper_voice_model_path.clone(),
        )) as Arc<dyn Tts>),
    };

    #[cfg(feature = "local-whisper")]
    let stt: Option<Arc<dyn Stt>> = {
        match WhisperStt::new(
            &config.stt.whisper_model_path,
            language,
            config.stt.whisper_vad_model_path.clone(),
        ) {
            Ok(stt) => Some(Arc::new(stt)),
            Err(e) => {
                eprintln!(
                    "Couldn't load the local Whisper model at \"{}\": {e}. Voice input is disabled until a model is downloaded - see docs/setup-elevenlabs-piper.md.",
                    config.stt.whisper_model_path
                );
                None
            }
        }
    };
    #[cfg(not(feature = "local-whisper"))]
    let stt = None;

    VoiceSupport { stt, tts }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Init)) {
        // Prompts go to stderr and the finished .env content goes to
        // stdout, so `guss init > .env` redirects only the real output,
        // not the interactive questions.
        let stdin = std::io::stdin().lock();
        let stderr = std::io::stderr().lock();
        match init::run_wizard(stdin, stderr) {
            Ok(env) => print!("{env}"),
            Err(e) => {
                eprintln!("guss init failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if matches!(args.command, Some(Command::Backup)) {
        use std::io::Write;
        let result = async {
            let config = AppConfig::load().map_err(|e| e.to_string())?;
            let store = guss_store::AnyStore::connect(&config.store.url)
                .await
                .map_err(|e| e.to_string())?;
            store.snapshot().await.map_err(|e| e.to_string())
        }
        .await;
        match result {
            Ok(bytes) => {
                let mut out = std::io::stdout().lock();
                if let Err(e) = out.write_all(&bytes).and_then(|_| out.flush()) {
                    eprintln!("guss backup failed: {e}");
                    std::process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("guss backup failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    tracing_subscriber::fmt::init();

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Guss failed to start: {e}");
            eprintln!(
                "Copy .env.example to .env (or write config.toml) and fill in your business name/currency and LLM settings."
            );
            std::process::exit(1);
        }
    };

    let store = match guss_store::AnyStore::connect(&config.store.url).await {
        Ok(store) => Arc::new(store),
        Err(e) => {
            eprintln!(
                "Guss failed to open its database at {}: {e}",
                config.store.url
            );
            std::process::exit(1);
        }
    };

    let business = match store.load_business_profile().await {
        // The configured values win on every start: they come from the
        // settings form (Friday Host) or .env, and the stored profile used
        // to keep its first-start values forever (changing the currency
        // in settings did nothing). The id stays, so history is kept.
        Ok(Some(mut profile)) => {
            let configured = (
                &config.business.name,
                &config.business.currency,
                &config.business.language,
                &config.business.timezone,
            );
            let stored = (
                &profile.name,
                &profile.default_currency,
                &profile.language,
                &profile.timezone,
            );
            if configured != stored {
                profile.name = config.business.name.clone();
                profile.default_currency = config.business.currency.clone();
                profile.language = config.business.language.clone();
                profile.timezone = config.business.timezone.clone();
                if let Err(e) = store.save_business_profile(&profile).await {
                    eprintln!("Guss failed to update the business profile: {e}");
                    std::process::exit(1);
                }
                tracing::info!(business = %profile.name, currency = %profile.default_currency, "business profile updated from config");
            }
            profile
        }
        Ok(None) => {
            let profile = BusinessProfile {
                id: Uuid::new_v4(),
                name: config.business.name.clone(),
                default_currency: config.business.currency.clone(),
                language: config.business.language.clone(),
                timezone: config.business.timezone.clone(),
            };
            if let Err(e) = store.save_business_profile(&profile).await {
                eprintln!("Guss failed to save the business profile: {e}");
                std::process::exit(1);
            }
            profile
        }
        Err(e) => {
            eprintln!("Guss failed to load the business profile: {e}");
            std::process::exit(1);
        }
    };

    tracing::info!(business = %business.name, channel = %config.channel.kind, "Guss starting");

    let provider = OpenAiCompatibleProvider::new(
        config.llm.base_url.clone(),
        config.llm.api_key.clone(),
        config.llm.model.clone(),
    );
    let nlu = Nlu::new(provider, business.language.clone());
    let voice = Arc::new(build_voice_support(&config, &business.language));
    let friday = match (
        config
            .friday
            .url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        config
            .friday
            .key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        (Some(url), Some(key)) => {
            let client = Arc::new(guss_channels::friday::FridayClient::new(url, key));
            let (sync_client, sync_store, sync_business) =
                (client.clone(), store.clone(), business.clone());
            tokio::spawn(async move {
                guss_channels::friday_sync::run(sync_store.as_ref(), &sync_client, sync_business)
                    .await;
            });
            tracing::info!(
                url,
                "connected to Friday: roles from Friday, entries sent to Inventory and Accounting"
            );
            Some(client)
        }
        (Some(_), None) | (None, Some(_)) => {
            tracing::warn!("set both GUSS_FRIDAY__URL and GUSS_FRIDAY__KEY to connect to Friday");
            None
        }
        (None, None) => None,
    };
    let other_currency_policy = config
        .business
        .other_currencies
        .parse::<OtherCurrencyPolicy>()
        .unwrap_or_else(|e| {
            tracing::warn!("{e}; asking before saving other currencies");
            OtherCurrencyPolicy::Ask
        });
    let mut orchestrator =
        Orchestrator::new(nlu, store, business).with_other_currency_policy(other_currency_policy);
    if let Some(f) = friday.clone() {
        orchestrator = orchestrator.with_step_up(f);
    }
    let orchestrator = Arc::new(orchestrator);

    match config.web.token.as_deref().map(str::trim) {
        Some(token) if token.len() >= 24 => {
            let notifier: Option<Arc<dyn web::Notifier>> = match (
                config.channel.kind.as_str(),
                config.channel.telegram_bot_token.as_deref(),
            ) {
                ("telegram", Some(bot)) => Some(Arc::new(telegram::TelegramNotifier::new(bot))),
                _ => None,
            };
            let web_config = web::WebConfig {
                token: token.to_string(),
                listen_addr: config.web.listen_addr.clone(),
                notifier,
            };
            let web_orchestrator = orchestrator.clone();
            tokio::spawn(async move {
                if let Err(e) = web::run(web_orchestrator, web_config).await {
                    tracing::error!(error = %e, "web API stopped");
                }
            });
        }
        Some(_) => {
            tracing::warn!("GUSS_WEB__TOKEN is shorter than 24 characters - web API not started.")
        }
        None => {}
    }

    match config.channel.kind.as_str() {
        "cli" => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            if let Err(e) = cli::run(&orchestrator, &voice, stdin, stdout).await {
                eprintln!("CLI channel exited with an error: {e}");
                std::process::exit(1);
            }
        }
        "telegram" => {
            let Some(token) = &config.channel.telegram_bot_token else {
                eprintln!(
                    "GUSS_CHANNEL__KIND=telegram but GUSS_CHANNEL__TELEGRAM_BOT_TOKEN is not set - see docs/setup-telegram.md."
                );
                std::process::exit(1);
            };

            let _job_scheduler = if let Some(chat_id) = config.channel.telegram_owner_chat_id {
                let sink = Arc::new(telegram::TelegramReportSink::new(token, chat_id));
                let reports = Arc::new(guss_channels::orchestrator::OwnerReports {
                    orchestrator: orchestrator.clone(),
                    owner: guss_core::people::Actor::owner(format!("telegram:{chat_id}")),
                });
                match guss_scheduler::start(
                    reports,
                    sink,
                    build_schedule_config(&config.channel.scheduler),
                )
                .await
                {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!(
                            "Failed to start the report scheduler: {e} - continuing without proactive EOD/weekly/monthly pushes."
                        );
                        None
                    }
                }
            } else {
                tracing::info!(
                    "GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID not set - scheduled reports are disabled (on-demand \"report\" still works)."
                );
                None
            };

            let (allowed_user_ids, bad) = guss_channels::telegram_ux::TelegramAccess::parse_ids(
                config
                    .channel
                    .telegram_allowed_user_ids
                    .as_deref()
                    .unwrap_or(""),
            );
            if !bad.is_empty() {
                tracing::warn!(
                    ?bad,
                    "ignoring GUSS_CHANNEL__TELEGRAM_ALLOWED_USER_IDS entries that aren't numbers"
                );
            }
            let (manager_user_ids, bad) = guss_channels::telegram_ux::TelegramAccess::parse_ids(
                config.channel.telegram_manager_ids.as_deref().unwrap_or(""),
            );
            if !bad.is_empty() {
                tracing::warn!(
                    ?bad,
                    "ignoring GUSS_CHANNEL__TELEGRAM_MANAGER_IDS entries that aren't numbers"
                );
            }
            let access = guss_channels::telegram_ux::TelegramAccess {
                owner_chat_id: config.channel.telegram_owner_chat_id,
                manager_user_ids,
                allowed_user_ids,
            };
            if let Err(e) = telegram::run(orchestrator, voice, token, access, friday).await {
                eprintln!("Telegram channel exited with an error: {e}");
                std::process::exit(1);
            }
        }
        "whatsapp" => {
            let (Some(verify_token), Some(access_token), Some(phone_number_id), Some(app_secret)) = (
                &config.channel.whatsapp_verify_token,
                &config.channel.whatsapp_access_token,
                &config.channel.whatsapp_phone_number_id,
                &config.channel.whatsapp_app_secret,
            ) else {
                eprintln!(
                    "GUSS_CHANNEL__KIND=whatsapp requires GUSS_CHANNEL__WHATSAPP_VERIFY_TOKEN, \
                     _ACCESS_TOKEN, _PHONE_NUMBER_ID, and _APP_SECRET - see docs/setup-whatsapp-cloud-api.md."
                );
                std::process::exit(1);
            };
            let wa_config = guss_channels::whatsapp::WhatsAppConfig {
                verify_token: verify_token.clone(),
                access_token: access_token.clone(),
                phone_number_id: phone_number_id.clone(),
                app_secret: app_secret.clone(),
                listen_addr: config.channel.whatsapp_listen_addr.clone(),
            };

            let _job_scheduler = if let Some(owner_number) =
                &config.channel.whatsapp_owner_phone_number
            {
                let sink = Arc::new(guss_channels::whatsapp::WhatsAppReportSink::new(
                    access_token.clone(),
                    phone_number_id.clone(),
                    owner_number.clone(),
                ));
                let reports = Arc::new(guss_channels::orchestrator::OwnerReports {
                    orchestrator: orchestrator.clone(),
                    owner: guss_core::people::Actor::owner("whatsapp:owner"),
                });
                match guss_scheduler::start(
                    reports,
                    sink,
                    build_schedule_config(&config.channel.scheduler),
                )
                .await
                {
                    Ok(s) => Some(s),
                    Err(e) => {
                        eprintln!(
                            "Failed to start the report scheduler: {e} - continuing without proactive EOD/weekly/monthly pushes."
                        );
                        None
                    }
                }
            } else {
                tracing::info!(
                    "GUSS_CHANNEL__WHATSAPP_OWNER_PHONE_NUMBER not set - scheduled reports are disabled (on-demand \"report\" still works)."
                );
                None
            };

            if let Err(e) = guss_channels::whatsapp::run(orchestrator, voice, wa_config).await {
                eprintln!("WhatsApp channel exited with an error: {e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!(
                "Channel \"{other}\" isn't wired up yet - \"cli\", \"telegram\", and \"whatsapp\" work right now."
            );
            std::process::exit(1);
        }
    }
}
