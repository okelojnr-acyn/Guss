use std::sync::Arc;

use guss_voice::{Stt, Tts};

/// Optional voice support shared by every Channel adapter (CLI, Telegram,
/// WhatsApp). Both sides are independent: a business can run text-only
/// (both `None`), STT-only, TTS-only, or both.
#[derive(Default, Clone)]
pub struct VoiceSupport {
    pub stt: Option<Arc<dyn Stt>>,
    pub tts: Option<Arc<dyn Tts>>,
}
