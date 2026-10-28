#!/usr/bin/env python3
"""Live integration check: direct OpenRouter call vs. via PennyRouter.

Calls POST /v1/chat/completions once directly against OpenRouter (baseline)
and once through a running PennyRouter gateway with the same body, then
compares: both must return 200 with a well-formed non-streaming chat
completion. Compares shape, not wording — LLM output differs per call.

Needs a real backend key, so it is opt-in and never runs in CI.
Run it from this folder:
  1. cp -n .env.example .env   # once; .env is gitignored, never commit it
  2. add OPENROUTER_API_KEY=sk-or-... to .env (real env vars also work)
  3. from the repo root: cargo run -p pennyrouter-gateway
     (or: docker compose up --build)
  4. ./openrouter_compare.py

Without a key it skips (exit 0). Uses a tiny max_tokens to keep the two
calls cheap. Stdlib only.

Config (folder .env, repo-root .env, or env var; env wins, folder beats root):
  OPENROUTER_API_KEY   backend key (required, else skip)
  OPENROUTER_BASE_URL  default https://openrouter.ai/api
  GATEWAY_URL          default http://localhost:3000
  OPENROUTER_TEST_MODEL default openai/gpt-oss-20b
  TEST_TIMEOUT_SECONDS default 60
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

_PARENTS = Path(__file__).resolve().parents
ROOT = _PARENTS[3] if len(_PARENTS) > 3 else Path(__file__).resolve().parent
HERE = Path(__file__).resolve().parent
PROMPT = "Reply with exactly one word: ok"
# Cloudflare (error 1010) blocks urllib's default UA, so identify as a
# browser with our own token appended. The gateway forwards this upstream.
USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36 PennyRouter-Check/1.0"
)


def load_dotenv(path):
    """Minimal KEY=VALUE loader (comments/#, quotes, and `export ` handled)."""
    values = {}
    try:
        text = path.read_text()
    except FileNotFoundError:
        return values
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        if line.startswith("export "):
            line = line[len("export ") :].strip()
        key, _, raw = line.partition("=")
        key, raw = key.strip(), raw.strip()
        if len(raw) >= 2 and raw[0] == raw[-1] and raw[0] in "'\"":
            raw = raw[1:-1]
        values[key] = raw
    return values


def post_chat(base_url, api_key, model, timeout):
    body = json.dumps(
        {
            "model": model,
            "messages": [{"role": "user", "content": PROMPT}],
            "max_tokens": 16,
            "stream": False,
        }
    ).encode()
    req = urllib.request.Request(
        base_url.rstrip("/") + "/v1/chat/completions",
        data=body,
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
            "User-Agent": USER_AGENT,
        },
        method="POST",
    )
    start = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            raw = res.read().decode()
            return res.status, (time.monotonic() - start) * 1000, raw, None
    except urllib.error.HTTPError as error:
        detail = error.read().decode(errors="replace")[:500]
        return error.code, (time.monotonic() - start) * 1000, None, detail
    except Exception as error:  # noqa: BLE001 - reported, not raised
        return 0, (time.monotonic() - start) * 1000, None, repr(error)


def check_shape(raw):
    """Well-formed non-streaming completion? Returns content or an error."""
    try:
        payload = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return None, "body is not JSON"
    if payload.get("object") != "chat.completion":
        return None, f"object is {payload.get('object')!r}"
    try:
        message = payload["choices"][0]["message"]
    except (KeyError, IndexError, TypeError):
        return None, "missing choices[0].message"
    content = message.get("content")
    if not isinstance(content, str) or not content.strip():
        # Reasoning models can spend a tiny token budget on reasoning,
        # leaving content null — their reasoning text still proves a
        # well-formed completion.
        content = message.get("reasoning") or ""
    if not isinstance(content, str) or not content.strip():
        return None, "empty completion content"
    return content.strip(), None


def main():
    config = load_dotenv(ROOT / ".env")
    config.update(load_dotenv(HERE / ".env"))  # folder-local wins over root
    for key in (
        "OPENROUTER_API_KEY",
        "OPENROUTER_BASE_URL",
        "GATEWAY_URL",
        "OPENROUTER_TEST_MODEL",
        "TEST_TIMEOUT_SECONDS",
    ):
        if os.environ.get(key):
            config[key] = os.environ[key]
    api_key = config.get("OPENROUTER_API_KEY", "")
    if not api_key or "redacted" in api_key.lower() or "example" in api_key.lower():
        print("SKIP: set OPENROUTER_API_KEY in .env or the environment")
        return 0
    openrouter = config.get("OPENROUTER_BASE_URL", "https://openrouter.ai/api")
    gateway = config.get("GATEWAY_URL", "http://localhost:3000")
    model = config.get("OPENROUTER_TEST_MODEL", "openai/gpt-oss-20b")
    timeout = float(config.get("TEST_TIMEOUT_SECONDS", "60"))

    failures = []

    status, ms, raw, detail = post_chat(openrouter, api_key, model, timeout)
    print(f"direct   -> {openrouter}: http={status} {ms:.0f}ms")
    if status != 200:
        failures.append(f"direct call failed: http={status} detail={detail}")
    else:
        content, problem = check_shape(raw)
        print(f"  content: {content!r}" if content else f"  BAD: {problem}")
        if problem:
            failures.append(f"direct response malformed: {problem}")

    status, ms, raw, detail = post_chat(gateway, api_key, model, timeout)
    print(f"gateway  -> {gateway}: http={status} {ms:.0f}ms")
    if status == 0:
        failures.append(
            f"gateway unreachable at {gateway} ({detail}); "
            "start it with `cargo run -p pennyrouter-gateway` or `docker compose up --build`"
        )
    elif status != 200:
        failures.append(f"gateway call failed: http={status} detail={detail}")
    else:
        content, problem = check_shape(raw)
        print(f"  content: {content!r}" if content else f"  BAD: {problem}")
        if problem:
            failures.append(f"gateway response malformed: {problem}")

    if failures:
        print("FAIL:")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("PASS: direct and gateway calls both returned well-formed completions")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
