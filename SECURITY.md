# Security Policy

## Reporting a vulnerability

Please report security issues privately via
[GitHub Security Advisories](https://github.com/okelojnr-acyn/Guss/security/advisories/new)
rather than a public issue. Include what you found, how to reproduce it,
and what you think the impact is. We'll acknowledge reports as soon as
we can and keep you updated as a fix is worked on.

## Scope and what's already mitigated

Guss is a self-hosted binary you run against your own credentials, so
most of its security surface is in how it handles those credentials and
the webhooks it receives, rather than a multi-tenant hosted service:

- **WhatsApp webhooks** are verified via HMAC-SHA256 against the
  `X-Hub-Signature-256` header and the configured Meta app secret before
  any payload is trusted (`crates/guss-channels/src/whatsapp.rs`) - an
  unsigned or incorrectly-signed request is rejected, not processed.
- **API keys, bot tokens, and app secrets** are read from `.env`/environment
  variables only and are never logged or included in report output.
  `.env` is gitignored; `.env.example` contains no real values.
- **Webhook redelivery** (both Telegram and WhatsApp redeliver on
  timeout) is deduplicated by provider message ID
  (`Store::mark_message_processed`) before any processing happens, so a
  replayed webhook can't double-log an event.

If you find a gap in any of the above, or something not covered by it,
that's exactly what this policy is for.

## Supported versions

This project is pre-1.0 (`0.1.0` across all crates). Security fixes land
on `main`; there's no older release branch to backport to yet.
