use figment::providers::{Env, Format, Toml};
use figment::Figment;
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to load configuration: {0}")]
    Load(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct BusinessConfig {
    pub name: String,
    pub currency: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
}

fn default_language() -> String {
    "en".to_string()
}

fn default_timezone() -> String {
    "UTC".to_string()
}

/// Configures the one LlmProvider implementation (guss-llm) against
/// whichever OpenAI-compatible endpoint the operator has chosen - Ollama,
/// Groq, or OpenAI all work unmodified here.
#[derive(Debug, Clone, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_llm_base_url")]
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default = "default_llm_model")]
    pub model: String,
}

fn default_llm_base_url() -> String {
    // Ollama's local OpenAI-compatible endpoint - works with zero API key.
    "http://localhost:11434/v1".to_string()
}

fn default_llm_model() -> String {
    "llama3.1".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct StoreConfig {
    #[serde(default = "default_store_url")]
    pub url: String,
}

fn default_store_url() -> String {
    "sqlite://guss.db".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct SttConfig {
    /// Path to a whisper.cpp GGML/GGUF model file, downloaded separately
    /// per docs/setup guides (not bundled in the repo).
    #[serde(default = "default_whisper_model_path")]
    pub whisper_model_path: String,
    /// Optional Silero VAD model path - recommended (reduces whisper's
    /// known hallucination-on-silence failure mode) but not required.
    pub whisper_vad_model_path: Option<String>,
}

fn default_whisper_model_path() -> String {
    "models/ggml-base.en.bin".to_string()
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            whisper_model_path: default_whisper_model_path(),
            whisper_vad_model_path: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TtsConfig {
    /// "elevenlabs" or "piper". Piper is the free local fallback so the
    /// repo works end-to-end with zero paid keys.
    #[serde(default = "default_tts_provider")]
    pub provider: String,
    pub elevenlabs_api_key: Option<String>,
    pub elevenlabs_voice_id: Option<String>,
    #[serde(default = "default_piper_binary")]
    pub piper_binary: String,
    #[serde(default = "default_piper_voice_model_path")]
    pub piper_voice_model_path: String,
}

fn default_tts_provider() -> String {
    "piper".to_string()
}

fn default_piper_binary() -> String {
    "piper".to_string()
}

fn default_piper_voice_model_path() -> String {
    "models/voice.onnx".to_string()
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: default_tts_provider(),
            elevenlabs_api_key: None,
            elevenlabs_voice_id: None,
            piper_binary: default_piper_binary(),
            piper_voice_model_path: default_piper_voice_model_path(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChannelConfig {
    /// "cli", "telegram", or "whatsapp".
    #[serde(default = "default_channel_kind")]
    pub kind: String,
    pub telegram_bot_token: Option<String>,
    pub whatsapp_verify_token: Option<String>,
    pub whatsapp_access_token: Option<String>,
    pub whatsapp_phone_number_id: Option<String>,
    /// Meta app secret, used to verify the X-Hub-Signature-256 header on
    /// every webhook POST - without this check, anyone who finds the
    /// webhook URL could inject fake messages into the ledger.
    pub whatsapp_app_secret: Option<String>,
    #[serde(default = "default_whatsapp_listen_addr")]
    pub whatsapp_listen_addr: String,
}

fn default_channel_kind() -> String {
    "cli".to_string()
}

fn default_whatsapp_listen_addr() -> String {
    "0.0.0.0:8080".to_string()
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            kind: default_channel_kind(),
            telegram_bot_token: None,
            whatsapp_verify_token: None,
            whatsapp_access_token: None,
            whatsapp_phone_number_id: None,
            whatsapp_app_secret: None,
            whatsapp_listen_addr: default_whatsapp_listen_addr(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub business: BusinessConfig,
    pub llm: LlmConfig,
    #[serde(default)]
    pub store: StoreConfig,
    #[serde(default)]
    pub stt: SttConfig,
    #[serde(default)]
    pub tts: TtsConfig,
    #[serde(default)]
    pub channel: ChannelConfig,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            url: default_store_url(),
        }
    }
}

impl AppConfig {
    /// Loads config in increasing priority: `config.toml` (if present),
    /// then `.env` (if present), then real environment variables. Env keys
    /// use a double-underscore to address nested fields, e.g.
    /// `GUSS_LLM__API_KEY` -> `llm.api_key`.
    pub fn load() -> Result<Self, ConfigError> {
        let _ = dotenvy::dotenv();

        Figment::new()
            .merge(Toml::file("config.toml"))
            .merge(Env::prefixed("GUSS_").split("__"))
            .extract()
            .map_err(|e| ConfigError::Load(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use figment::Jail;

    #[test]
    fn loads_from_toml_with_defaults_filled_in() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.toml",
                r#"
                [business]
                name = "Acme Shop"
                currency = "USD"

                [llm]
                api_key = "test-key"
                "#,
            )?;

            let config = AppConfig::load().expect("config should load");
            assert_eq!(config.business.name, "Acme Shop");
            assert_eq!(config.business.language, "en");
            assert_eq!(config.llm.base_url, "http://localhost:11434/v1");
            assert_eq!(config.llm.api_key, Some("test-key".to_string()));
            assert_eq!(config.store.url, "sqlite://guss.db");
            assert_eq!(config.tts.provider, "piper");
            Ok(())
        });
    }

    #[test]
    fn env_vars_override_toml() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "config.toml",
                r#"
                [business]
                name = "Acme Shop"
                currency = "USD"

                [llm]
                model = "llama3.1"
                "#,
            )?;
            jail.set_env("GUSS_LLM__MODEL", "mixtral");

            let config = AppConfig::load().expect("config should load");
            assert_eq!(config.llm.model, "mixtral");
            Ok(())
        });
    }
}
