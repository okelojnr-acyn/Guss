mod config;

use config::AppConfig;
use guss_core::store::Store;

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

    tracing::info!(business = %config.business.name, "Guss starting");

    let store = match guss_store::SqliteStore::connect(&config.store.url).await {
        Ok(store) => store,
        Err(e) => {
            eprintln!("Guss failed to open its database at {}: {e}", config.store.url);
            std::process::exit(1);
        }
    };

    let _ = store.load_business_profile().await;

    println!("Guss is configured for \"{}\" ({}). Channel/LLM/voice wiring lands in later milestones.", config.business.name, config.business.currency);
}
