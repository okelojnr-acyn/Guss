use std::io::{BufRead, Write};

/// Interactive setup wizard: asks a short series of questions and builds
/// the content for a `.env` file. Takes generic Read/Write so the real
/// `guss init` command (stdin/stdout) and tests (an in-memory buffer) run
/// through the exact same code path.
pub fn run_wizard(mut input: impl BufRead, mut output: impl Write) -> std::io::Result<String> {
    writeln!(
        output,
        "Guss setup - answer a few questions, then copy the result into a .env file (or let `guss init > .env` do it for you).\n"
    )?;

    let name = ask(&mut input, &mut output, "Business name", "My Shop")?;
    let currency = ask(
        &mut input,
        &mut output,
        "Default currency (ISO 4217, e.g. USD, IDR)",
        "USD",
    )?;
    let language = ask_choice(
        &mut input,
        &mut output,
        "Language",
        &[("en", "English"), ("id", "Bahasa Indonesia")],
        "en",
    )?;
    let timezone = ask(
        &mut input,
        &mut output,
        "Timezone (IANA, e.g. Asia/Jakarta)",
        "UTC",
    )?;

    writeln!(output, "\n--- LLM ---")?;
    let llm_choice = ask_choice(
        &mut input,
        &mut output,
        "LLM provider",
        &[
            (
                "ollama",
                "Ollama - local, free, no API key, needs Ollama running",
            ),
            ("groq", "Groq - free tier, fast, needs an API key"),
            ("openai", "OpenAI - needs an API key"),
        ],
        "ollama",
    )?;
    let (llm_base_url, llm_model_default) = match llm_choice.as_str() {
        "groq" => ("https://api.groq.com/openai/v1", "llama-3.1-8b-instant"),
        "openai" => ("https://api.openai.com/v1", "gpt-4o-mini"),
        _ => ("http://localhost:11434/v1", "llama3.1"),
    };
    let llm_model = ask(&mut input, &mut output, "LLM model", llm_model_default)?;
    let llm_api_key = if llm_choice == "ollama" {
        String::new()
    } else {
        ask(&mut input, &mut output, "LLM API key", "")?
    };

    writeln!(output, "\n--- Channel ---")?;
    let channel = ask_choice(
        &mut input,
        &mut output,
        "Channel",
        &[
            ("cli", "CLI - instant, zero setup, good for trying Guss out"),
            (
                "telegram",
                "Telegram - free bot token, voice notes supported",
            ),
            (
                "whatsapp",
                "WhatsApp Cloud API - official, free tier, needs a Meta app",
            ),
        ],
        "cli",
    )?;

    let mut telegram_bot_token = String::new();
    let mut telegram_owner_chat_id = String::new();
    let mut whatsapp_verify_token = String::new();
    let mut whatsapp_access_token = String::new();
    let mut whatsapp_phone_number_id = String::new();
    let mut whatsapp_app_secret = String::new();
    let mut whatsapp_owner_phone_number = String::new();

    if channel == "telegram" {
        telegram_bot_token = ask(
            &mut input,
            &mut output,
            "Telegram bot token (see docs/setup-telegram.md)",
            "",
        )?;
        telegram_owner_chat_id = ask(
            &mut input,
            &mut output,
            "Your Telegram chat id, for scheduled reports (leave blank to disable)",
            "",
        )?;
    } else if channel == "whatsapp" {
        whatsapp_verify_token = ask(
            &mut input,
            &mut output,
            "WhatsApp webhook verify token (you choose this)",
            "",
        )?;
        whatsapp_access_token = ask(
            &mut input,
            &mut output,
            "WhatsApp Graph API access token",
            "",
        )?;
        whatsapp_phone_number_id = ask(&mut input, &mut output, "WhatsApp phone number id", "")?;
        whatsapp_app_secret = ask(
            &mut input,
            &mut output,
            "Meta app secret (for webhook signature verification)",
            "",
        )?;
        whatsapp_owner_phone_number = ask(
            &mut input,
            &mut output,
            "Your WhatsApp number, for scheduled reports (leave blank to disable)",
            "",
        )?;
    }

    writeln!(output, "\n--- Voice ---")?;
    let tts = ask_choice(
        &mut input,
        &mut output,
        "Text-to-speech",
        &[
            (
                "piper",
                "Piper - free, local, needs the piper CLI binary installed",
            ),
            (
                "elevenlabs",
                "ElevenLabs - needs an API key, higher quality",
            ),
        ],
        "piper",
    )?;
    let mut elevenlabs_api_key = String::new();
    let mut elevenlabs_voice_id = String::new();
    if tts == "elevenlabs" {
        elevenlabs_api_key = ask(&mut input, &mut output, "ElevenLabs API key", "")?;
        elevenlabs_voice_id = ask(&mut input, &mut output, "ElevenLabs voice id", "")?;
    }

    let env = format!(
        "GUSS_BUSINESS__NAME={name}\n\
         GUSS_BUSINESS__CURRENCY={currency}\n\
         GUSS_BUSINESS__LANGUAGE={language}\n\
         GUSS_BUSINESS__TIMEZONE={timezone}\n\
         \n\
         GUSS_LLM__BASE_URL={llm_base_url}\n\
         GUSS_LLM__API_KEY={llm_api_key}\n\
         GUSS_LLM__MODEL={llm_model}\n\
         \n\
         GUSS_STORE__URL=sqlite://guss.db\n\
         \n\
         GUSS_TTS__PROVIDER={tts}\n\
         GUSS_TTS__ELEVENLABS_API_KEY={elevenlabs_api_key}\n\
         GUSS_TTS__ELEVENLABS_VOICE_ID={elevenlabs_voice_id}\n\
         GUSS_TTS__PIPER_BINARY=piper\n\
         GUSS_TTS__PIPER_VOICE_MODEL_PATH=models/voice.onnx\n\
         \n\
         GUSS_CHANNEL__KIND={channel}\n\
         GUSS_CHANNEL__TELEGRAM_BOT_TOKEN={telegram_bot_token}\n\
         GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID={telegram_owner_chat_id}\n\
         GUSS_CHANNEL__WHATSAPP_VERIFY_TOKEN={whatsapp_verify_token}\n\
         GUSS_CHANNEL__WHATSAPP_ACCESS_TOKEN={whatsapp_access_token}\n\
         GUSS_CHANNEL__WHATSAPP_PHONE_NUMBER_ID={whatsapp_phone_number_id}\n\
         GUSS_CHANNEL__WHATSAPP_APP_SECRET={whatsapp_app_secret}\n\
         GUSS_CHANNEL__WHATSAPP_OWNER_PHONE_NUMBER={whatsapp_owner_phone_number}\n\
         GUSS_CHANNEL__WHATSAPP_LISTEN_ADDR=0.0.0.0:8080\n"
    );

    writeln!(
        output,
        "\nDone. The .env content follows below - redirect it to a file (guss init > .env) and run `guss`."
    )?;
    Ok(env)
}

fn ask(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
    default: &str,
) -> std::io::Result<String> {
    if default.is_empty() {
        write!(output, "{prompt}: ")?;
    } else {
        write!(output, "{prompt} [{default}]: ")?;
    }
    output.flush()?;

    let mut line = String::new();
    input.read_line(&mut line)?;
    let answer = line.trim();
    Ok(if answer.is_empty() {
        default.to_string()
    } else {
        answer.to_string()
    })
}

fn ask_choice(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
    choices: &[(&str, &str)],
    default: &str,
) -> std::io::Result<String> {
    writeln!(output, "{prompt}:")?;
    for (key, desc) in choices {
        let marker = if *key == default { "*" } else { " " };
        writeln!(output, "  {marker} {key} - {desc}")?;
    }
    let answer = ask(
        input,
        output,
        &format!(
            "Choose ({})",
            choices
                .iter()
                .map(|(k, _)| *k)
                .collect::<Vec<_>>()
                .join("/")
        ),
        default,
    )?;
    if choices.iter().any(|(k, _)| *k == answer) {
        Ok(answer)
    } else {
        writeln!(
            output,
            "Didn't recognize \"{answer}\" - using default \"{default}\"."
        )?;
        Ok(default.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn defaults_are_used_when_everything_is_left_blank() {
        let input = Cursor::new(b"\n".repeat(20)); // enough blank lines for every prompt
        let mut output = Vec::new();

        let env = run_wizard(input, &mut output).unwrap();

        assert!(env.contains("GUSS_BUSINESS__NAME=My Shop"));
        assert!(env.contains("GUSS_BUSINESS__CURRENCY=USD"));
        assert!(env.contains("GUSS_BUSINESS__LANGUAGE=en"));
        assert!(env.contains("GUSS_LLM__BASE_URL=http://localhost:11434/v1"));
        assert!(env.contains("GUSS_CHANNEL__KIND=cli"));
        assert!(env.contains("GUSS_TTS__PROVIDER=piper"));
    }

    #[test]
    fn telegram_choice_prompts_for_bot_token_and_chat_id() {
        let input = Cursor::new(
            b"Acme Shop\n\
              IDR\n\
              id\n\
              Asia/Jakarta\n\
              groq\n\
              \n\
              test-groq-key\n\
              telegram\n\
              test-bot-token\n\
              12345\n\
              piper\n"
                .to_vec(),
        );
        let mut output = Vec::new();

        let env = run_wizard(input, &mut output).unwrap();

        assert!(env.contains("GUSS_BUSINESS__NAME=Acme Shop"));
        assert!(env.contains("GUSS_BUSINESS__CURRENCY=IDR"));
        assert!(env.contains("GUSS_BUSINESS__LANGUAGE=id"));
        assert!(env.contains("GUSS_LLM__BASE_URL=https://api.groq.com/openai/v1"));
        assert!(env.contains("GUSS_LLM__API_KEY=test-groq-key"));
        assert!(env.contains("GUSS_CHANNEL__KIND=telegram"));
        assert!(env.contains("GUSS_CHANNEL__TELEGRAM_BOT_TOKEN=test-bot-token"));
        assert!(env.contains("GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID=12345"));
    }

    #[test]
    fn unrecognized_choice_falls_back_to_default() {
        // business name/currency/language/timezone blank, then an invalid LLM choice
        let input = Cursor::new(b"\n\n\n\nnot-a-real-provider\n\n\n\n\n\n\n".to_vec());
        let mut output = Vec::new();
        let env = run_wizard(input, &mut output).unwrap();
        assert!(env.contains("GUSS_LLM__BASE_URL=http://localhost:11434/v1"));
    }
}
