# Setting up Groq (free tier, fast, no local hardware needed)

Groq runs open models (Llama, Mixtral, etc.) on their own hardware and
offers a generous free tier - a good choice if your machine can't
comfortably run a local model, or you just want faster responses than
Ollama gives on modest hardware.

## 1. Create a free account

Sign up at [console.groq.com](https://console.groq.com).

## 2. Create an API key

In the Groq console, go to **API Keys** and create a new key. Copy it -
you won't be able to see it again.

## 3. Configure Guss

In your `.env`:

```
GUSS_LLM__BASE_URL=https://api.groq.com/openai/v1
GUSS_LLM__API_KEY=gsk_your_key_here
GUSS_LLM__MODEL=llama-3.1-8b-instant
```

`llama-3.1-8b-instant` is a good default for extraction tasks - fast and
cheap on Groq's free tier. Check
[console.groq.com/docs/models](https://console.groq.com/docs/models)
for current model names, since these change over time.

## Rate limits

Groq's free tier has rate limits (requests/tokens per minute). For a
single small business logging a handful of transactions a day, this is
normally not a concern - you'd only hit it if testing with a rapid burst
of messages.
