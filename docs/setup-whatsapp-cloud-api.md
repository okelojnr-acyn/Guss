# Setting up the WhatsApp channel (official Cloud API)

Guss talks to WhatsApp through Meta's official **Cloud API**, not an
unofficial "WhatsApp Web" bridge (the kind that automates a logged-in
WhatsApp Web session). That's a deliberate choice: unofficial bridges are
faster to set up but violate WhatsApp's terms of service, and WhatsApp
actively detects and bans numbers that look automated. The Cloud API
path takes longer to set up (a Meta Business app, a webhook, a public
HTTPS URL) but carries none of that ban risk, and it's free for a
reasonable volume of conversations.

If you've read this and still want to try an unofficial bridge anyway
(accepting the ban risk yourself) - Guss doesn't ship one, but
`guss-channels`' `Channel`-shaped pattern (see `telegram.rs`/
`whatsapp.rs`) is the template to follow if you want to wire one up as
your own adapter. That's on you to build and isn't something this
project provides or supports.

## 1. Create a Meta app

1. Go to [developers.facebook.com/apps](https://developers.facebook.com/apps)
   and create a new app, type "Business".
2. Add the **WhatsApp** product to the app.
3. Under WhatsApp → API Setup, you'll get a **temporary access token**
   and a **test phone number** - enough to get started without owning a
   real business phone number yet.

## 2. Collect your credentials

From the WhatsApp → API Setup page:

- **Phone number ID** - shown on that page.
- **Temporary access token** - shown on that page (expires in 24h;
  generate a permanent one later via a System User once you're past
  testing - see Meta's docs for that step, it's account-specific).

From your app's Settings → Basic:

- **App secret** - click "Show" next to App Secret.

You also choose your own:

- **Verify token** - any string you make up (e.g. a random password).
  Meta sends this back to you during webhook setup to prove you own the
  endpoint.

## 3. Expose a public HTTPS URL

Meta's webhook needs to reach your machine over the public internet.
For local testing, a tunnel tool works well:

```bash
# using ngrok, for example
ngrok http 8080
```

This gives you a URL like `https://abc123.ngrok-free.app` that forwards
to Guss's webhook server (default port 8080, configurable via
`GUSS_CHANNEL__WHATSAPP_LISTEN_ADDR`). For a real deployment, use your
own domain with a real TLS certificate instead of a tunnel.

## 4. Configure and start Guss first

Meta's webhook verification handshake happens *before* you can save the
webhook URL in their dashboard, so start Guss before configuring Meta.

In your `.env`:

```
GUSS_CHANNEL__KIND=whatsapp
GUSS_CHANNEL__WHATSAPP_VERIFY_TOKEN=some-random-string-you-made-up
GUSS_CHANNEL__WHATSAPP_ACCESS_TOKEN=your-access-token
GUSS_CHANNEL__WHATSAPP_PHONE_NUMBER_ID=your-phone-number-id
GUSS_CHANNEL__WHATSAPP_APP_SECRET=your-app-secret
GUSS_CHANNEL__WHATSAPP_OWNER_PHONE_NUMBER=15551234567
GUSS_CHANNEL__WHATSAPP_LISTEN_ADDR=0.0.0.0:8080
```

`WHATSAPP_OWNER_PHONE_NUMBER` (international format, no `+` or spaces)
is where scheduled EOD/weekly/monthly reports get pushed. Leave it blank
to skip scheduled pushes - on-demand "report" still works.

```bash
cargo run -p guss
```

## 5. Register the webhook with Meta

In your app's WhatsApp → Configuration page:

1. **Callback URL**: `https://your-tunnel-or-domain/webhook`
2. **Verify token**: the same string you put in
   `GUSS_CHANNEL__WHATSAPP_VERIFY_TOKEN`
3. Click **Verify and save** - Meta sends a GET request that Guss
   answers automatically if the token matches.
4. Under **Webhook fields**, subscribe to `messages`.

## 6. Try it

Send a WhatsApp message to your test number from the phone number you
added as a tester (Meta restricts test numbers to approved recipients
until your app passes review). You should get a reply within a few
seconds.

## Going to production

Meta requires app review before you can message arbitrary numbers (not
just pre-approved testers) and before you get a permanent, non-test
phone number. That process is entirely on Meta's side - see
[developers.facebook.com/docs/whatsapp/cloud-api](https://developers.facebook.com/docs/whatsapp/cloud-api)
for their current requirements, which change over time.
