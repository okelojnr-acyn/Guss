# Setting up Ollama (free, local, offline)

Ollama runs an LLM locally with no API key and no internet connection
needed once the model is downloaded. This is the easiest "truly free"
path, at the cost of needing a reasonably capable machine and a one-time
multi-gigabyte model download.

## 1. Install Ollama

- **Windows/Mac**: download from [ollama.com/download](https://ollama.com/download)
- **Linux**: `curl -fsSL https://ollama.com/install.sh | sh`

## 2. Pull a model

```bash
ollama pull llama3.1
```

`llama3.1` (8B) is a good default - capable enough for extraction tasks,
runs on most modern laptops. If your machine is limited, try a smaller
model like `llama3.2:1b` or `phi3`, though extraction accuracy may drop.

## 3. Confirm it's running

Ollama starts a local server automatically after install. Check it's
reachable:

```bash
curl http://localhost:11434/api/version
```

## 4. Configure Guss

In your `.env`:

```
GUSS_LLM__BASE_URL=http://localhost:11434/v1
GUSS_LLM__API_KEY=
GUSS_LLM__MODEL=llama3.1
```

No API key needed - leave `GUSS_LLM__API_KEY` blank.

## Running Guss and Ollama together in Docker

If you're using `docker compose`, start the optional Ollama service too:

```bash
docker compose --profile ollama up --build
```

Then point Guss at the Ollama *container* (not `localhost`, since
they're different containers):

```
GUSS_LLM__BASE_URL=http://ollama:11434/v1
```

You'll still need to pull a model into that container once:

```bash
docker compose exec ollama ollama pull llama3.1
```
