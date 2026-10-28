# Python live integration check

Opt-in check that calls OpenRouter once directly and once through a
running PennyRouter gateway, then compares the two responses (shape, not
wording). Never runs in CI — it needs a real backend key.

No dependencies: stdlib only (`urllib`), so no `requirements.txt` and no
venv — any Python 3 will do.

## Run (from this folder)

```sh
cp -n .env.example .env   # once; .env is gitignored, never commit it
# edit .env: set OPENROUTER_API_KEY
./openrouter_compare.py
```

The gateway must already be running (from the repo root):

```sh
cargo run -p pennyrouter-gateway   # or: docker compose up --build
```

Without a key the script prints `SKIP` and exits 0. Each run spends two
tiny `max_tokens=16` calls.

## Config

Folder `.env`, repo-root `.env`, or real env vars — real env wins, and
folder `.env` beats root `.env`:

- `OPENROUTER_API_KEY` — backend key (required, else skip)
- `OPENROUTER_BASE_URL` — default `https://openrouter.ai/api`
- `GATEWAY_URL` — default `http://localhost:3000`
- `OPENROUTER_TEST_MODEL` — default `openai/gpt-oss-20b`
- `TEST_TIMEOUT_SECONDS` — default `60`
