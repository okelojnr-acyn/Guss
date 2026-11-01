# Setting up the Telegram channel

Telegram is the fastest channel to set up - a free bot token, no app
review, and it supports voice notes out of the box. Good choice if you
don't want to deal with WhatsApp's Meta Business setup.

## 1. Create a bot

1. Open Telegram and message [@BotFather](https://t.me/BotFather).
2. Send `/newbot` and follow the prompts (pick a name and a username
   ending in `bot`).
3. BotFather replies with a token that looks like
   `123456789:AAhhhhhh-examplenotrealtoken`. Copy it.

## 2. Find your chat ID (for scheduled reports)

Scheduled EOD/weekly/monthly pushes need to know *where* to send them -
your own chat with the bot.

1. Message your new bot anything (e.g. "hi").
2. Visit `https://api.telegram.org/bot<YOUR_TOKEN>/getUpdates` in a
   browser (replace `<YOUR_TOKEN>` with your real token).
3. Find `"chat":{"id":...}` in the response - that number is your chat
   ID.

If you skip this step, on-demand "report" requests still work; only the
automatic scheduled pushes are disabled.

## 3. Configure Guss

In your `.env`:

```
GUSS_CHANNEL__KIND=telegram
GUSS_CHANNEL__TELEGRAM_BOT_TOKEN=123456789:AAhhhhhh-examplenotrealtoken
GUSS_CHANNEL__TELEGRAM_OWNER_CHAT_ID=987654321
```

## 4. Run it

```bash
cargo run -p guss
```

Guss starts long-polling Telegram - no public URL or port forwarding
needed, unlike WhatsApp's webhook model. Message your bot to try it.

## Voice notes

Record and send a voice note to your bot the same way you'd send one to
a friend - Guss downloads it, converts it with `ffmpeg`, and transcribes
it the same way as the CLI's `voice <path>` command, provided you've set
up local Whisper or another STT provider (see
[setup-elevenlabs-piper.md](setup-elevenlabs-piper.md)).
