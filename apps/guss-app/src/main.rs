mod config;

use std::sync::Arc;

use config::AppConfig;
use guss_channels::{cli, Orchestrator};
use guss_core::domain::BusinessProfile;
use guss_core::store::Store;
use guss_llm::OpenAiCompatibleProvider;
use guss_nlu::Nlu;
use uuid::Uuid;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Guss failed to start: {e}");
            eprintln!("Copy .env.example to .env (or write config.toml) and fill in your business name/currency and LLM settings.");
            std::process::exit(1);
        }
    };

    let store = match guss_store::SqliteStore::connect(&config.store.url).await {
        Ok(store) => Arc::new(store),
        Err(e) => {
            eprintln!("Guss failed to open its database at {}: {e}", config.store.url);
            std::process::exit(1);
        }
    };

    let business = match store.load_business_profile().await {
        Ok(Some(profile)) => profile,
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

    let provider = OpenAiCompatibleProvider::new(config.llm.base_url.clone(), config.llm.api_key.clone(), config.llm.model.clone());
    let nlu = Nlu::new(provider, business.language.clone());
    let orchestrator = Orchestrator::new(nlu, store, business);

    match config.channel.kind.as_str() {
        "cli" => {
            let stdin = std::io::stdin().lock();
            let stdout = std::io::stdout().lock();
            if let Err(e) = cli::run(&orchestrator, stdin, stdout).await {
                eprintln!("CLI channel exited with an error: {e}");
                std::process::exit(1);
            }
        }
        other => {
            eprintln!("Channel \"{other}\" isn't wired up yet - only \"cli\" works right now (Telegram/WhatsApp land in later milestones).");
            std::process::exit(1);
        }
    }
}
