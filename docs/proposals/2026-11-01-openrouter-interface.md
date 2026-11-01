# Proposal: OpenRouter-native interface for PennyRouter

> Date: 2026-11-01 · Author: freeclaw · Status: Implemented (Iter 25)

## TL;DR

Don't DNS-spoof `openrouter.ai` as the default integration. Ship **explicit `baseURL` support** — it's already how the gateway is built — plus a small gateway fix for OpenRouter-native paths. Keep DNS intercept as a local-dev trick with its own TLS workaround. Opencode and other harnesses already support this via `provider.options.baseURL` / custom providers.

PennyRouter today is OpenAI-compatible on input and OpenRouter on output:

- Gateway exposes `POST /v1/chat/completions` and `POST /v1/responses` with routing, budget, quality floor, retry.
- Everything else falls through `fallback_proxy`: `{OPENROUTER_BASE_URL}{path}` (default `https://openrouter.ai/api` — see `crates/gateway/src/lib.rs:268`). This composes correctly for OpenAI-shaped paths (`/v1/models` → `https://openrouter.ai/api/v1/models`), but `docs/api.md` correctly notes it **doubles `/api`** for OpenRouter-only extras like `/api/v1/auth/key`, `/api/v1/credits`, `/api/v1/generation` → 404. That is the gap this proposal closes.

## 1. Would an OpenRouter-native interface extend features?

Yes, but selectively. OpenRouter normalizes to OpenAI *plus* its own surface. Its canonical server per `openapi.json` is `https://openrouter.ai/api/v1`; paths like `/chat/completions` become `https://openrouter.ai/api/v1/chat/completions` — the same bytes PennyRouter already forwards via `base /api + path /v1/...`.

Useful native additions beyond transparent passthrough:

| Group | Example `api/v1` path | Value to gateway client | Routing needed? |
|---|---|---|---|
| Chat + Responses | `/chat/completions`, `/responses` with `models[]`, `provider{order,allow_fallbacks}`, `plugins[]`, `route:fallback`, `user` | Already routed. Alias `/api/v1/...` to same handler so OpenRouter SDKs work without path rewrite | Yes — alias |
| Catalog | `/models`, `/models/{author}/{slug}/endpoints`, `/models/count`, `/providers` | Lets harness/agent discover eligibility live; catalog-sync already pulls these public endpoints | No — proxy with fixed doubling |
| Account/metering | `/key`, `/credits`, `/credits/coinbase`, `/generation`, `/generation/content` | Show remaining budget, cost-per-task, per-generation feedback — addresses the product-viability gap "show what task cost" | No |
| Images/audio/tools | `/images`, `/audio/*`, `/embeddings`, `/files`, `/tools/*`, `/rerank`, `/videos/*` | Pass-through for agentic harnesses; not a price differentiator | No |
| Workspaces/keys/org | `/keys`, `/auth/keys`, `/workspaces/*`, `/organization/*`, `/guardrails/*` | Sensitive — must *not* be proxied by default | Explicit allowlist |

Conclusion: the win is **(a) aliases for the two routed calls** + **(b) fixing the double-prefix proxy for account/catalog paths**. The full OpenRouter management plane is out-of-scope for the data-plane gateway (preserve the "dumb and fast" realtime path — see `docs/architecture.md`).

## 2. DNS replacement vs `base_url` config

### Explicit `base_url` (recommended default)

```yaml
# compose / env — already default in crates/gateway/src/lib.rs:268
OPENROUTER_BASE_URL=https://openrouter.ai/api
# or per-pinned provider (Iter 16 pattern): MODAL_BASE_URL, BASETEN_BASE_URL
```

Client points at the gateway `http://gateway.apps.lan` or `https://pennymux.example.com` directly. Zero spoofing, observability intact (`x-pennyrouter-provider`, `route="other"` metrics), deployable behind Caddy/Terraform Hetzner as today (`compose.prod.yaml` pulls GHCR).

### DNS-transparent intercept ("replace hostname so client won't notice")

- `echo "10.x.x.x openrouter.ai" >> /etc/hosts` or CoreDNS rewrite inside k8s/Tailscale.
- Client keeps `https://openrouter.ai/api/v1/...` unchanged — attractive for closed binaries that hardcode the host.

Costs:

- **TLS:** client expects a cert for `openrouter.ai`. Options: (1) run gateway on plain `http://` and downgrade client to `http://openrouter.ai` — works on LAN, leaks beyond LAN, many SDKs refuse non-TLS by default; (2) gateway terminates TLS with its own CA and install that CA on every client host (Caddy `tls internal` + `mkcert`); (3) obtain a real `openrouter.ai` cert — impossible without DNS ownership / ACME DNS-01. So there is no zero-config TLS.
- **Blast radius:** `auth/key`, `credits/coinbase`, workspace writes now flow through the gateway — needs allowlisting; query redaction already exists via `redact_query` in `crates/gateway/src/lib.rs`.
- **Detectability:** `HTTP-Referer` / `X-OpenRouter-Title` headers and fingerprinting may differ.

**Decision:** don't make DNS-spoof the product story. Document it under `docs/operations.md` as a dev-only trick.

## 3. Do clients like Opencode allow endpoint override?

Yes. Verified via `https://opencode.ai/docs/providers/` and `https://opencode.ai/docs/models/`:

- Per-provider `options.baseURL` (alias `endpoint` for Bedrock):
  ```json
  { "$schema": "https://opencode.ai/config.json",
    "provider": { "anthropic": { "options": { "baseURL": "https://api.anthropic.com/v1" } } } }
  ```
- Custom OpenAI-compatible provider for any gateway (what PennyRouter needs):
  ```json
  { "provider": { "pennyrouter": {
        "npm": "@ai-sdk/openai-compatible",
        "name": "PennyRouter local",
        "options": { "baseURL": "http://127.0.0.1:3000/v1" },
        "models": { "openrouter/pair/free": {"name": "Pair free"} }
  } } }
  ```
- Supports `blacklist`/`whitelist`, per-model `options` and `variants` (e.g. `reasoningEffort`, `thinking.budgetTokens`) — unaffected by pointing `baseURL` at PennyRouter.

The same pattern exists for LiteLLM (`openai` with `api_base`), Vercel AI SDK, and the OpenAI SDK (`baseURL`). PennyRouter needs no client fork — users create a `pennyrouter` provider id pointing at the gateway. See also `docs/review/PennyRouter-product-viability-review.md:186` for the existing Opencode example.

## 4. Design — two small iterations, not one big rewrite

### Iter A — Path aliases (no ranking change)

1. Register aliases to the existing routed handler (`crates/gateway/src/lib.rs:app_with_state`):
   `POST /api/v1/chat/completions` and `POST /api/v1/responses` → `routed_completion` same as `/v1/...` (Iter 14 contract: rewrite, budget, floor, retry, `x-pennyrouter-provider`).
2. Keep `GET /v1/models` curated (six virtual ids `openrouter/pair/*` — Iter 13). Add `GET /api/v1/models` as a proxy passthrough to upstream (real catalog) to avoid surprising native OpenRouter clients that expect the full list.

Tests: reuse `tests/virtual_models.rs` against both prefixes; smoke `curl http://gateway/api/v1/chat/completions`.

### Iter B — Fix fallback doubling

Current `fallback_proxy` (`crates/gateway/src/lib.rs:830`) does `format!("{base}{path}")`. Change to:

```
if path starts_with "/api/" => upstream = "https://openrouter.ai" + path   // strip gateway's /api base
else                        => upstream = OPENROUTER_BASE_URL + path
```

Apply to `GET /api/v1/models`, `/api/v1/auth/key` (returns 200 live when called directly — noted in `docs/api.md`), `/api/v1/credits`, `/api/v1/generation`, `/api/v1/providers`, etc. Keep the current body-cap (1 MiB), timeouts, `FORWARDED_RESPONSE_HEADERS`, no retry/routing, `route="other"`, log `fallback_proxy:true` with redacted query.

Explicitly **deny by default**: `POST /api/v1/keys`, `.../credits/coinbase` payments, `POST /workspaces/*/budgets` unless `ALLOW_MANAGEMENT_PROXY=1`.

### What not to do

- No Ollama ↔ OpenAI translation (keep `docs/api.md` row: proxied as-is; 404 on OpenRouter upstream is expected).
- No per-request classifier; no judge on the realtime path (preserve Iter 15/17 separation — see `docs/architecture.md`).

## 5. SSL / plain HTTP guidance

- **Prod:** gateway behind Caddy TLS as today (`terraform/hetzner/templates/cloud-init.yaml`). Clients use `https://gateway.example.com/v1` — no spoofing needed.
- **LAN / DNS-spoof:** run Caddy `tls internal` for the `openrouter.ai` SAN and distribute the CA to lab hosts, or run `http://openrouter.ai` with `OPENROUTER_BASE_URL=http://openrouter.ai/api` and set client env `OPENAI_API_BASE=http://openrouter.ai/api/v1` — document both, recommend the CA path.

## 6. Config example (intended everyday flow after fix)

```jsonc
// ~/.config/opencode/opencode.json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "pennyrouter": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "PennyRouter (gateway)",
      "options": { "baseURL": "http://gateway.apps.lan/v1" },
      "models": {
        "openrouter/pair/free": { "name": "Pair free — free-first coding" },
        "openrouter/pair/low": { "name": "Pair low — cheapest basic" },
        "openrouter/pair/think": { "name": "Pair think — best quality" }
      }
    }
  },
  "model": "pennyrouter/openrouter/pair/free"
}
```

No key in file — `/connect` still provides the OpenRouter BYOK key forwarded upstream (gateway is BYOK today). For native OpenRouter SDKs, the same gateway address with `baseURL: http://gateway.apps.lan/api/v1` works once Iter A aliases land.

## 7. Risks & open questions

- **Path explosion:** OpenRouter has 80+ paths (`openapi.json` server `https://openrouter.ai/api/v1` plus management surface). Allowlist the proxy; deny management writes by default (mitigates the product-viability concern about sensitive code policies).
- **Performance:** aliases reuse one Axum handler → still a single lookup, no regression to the Iter 12 `+0.26 ms p50` baseline; management paths stay off the hot path.
- **Version skew:** `openapi.yaml` is large (~750 kB); pin a snapshot in `docs/review/openapi-YYYY-MM-DD.json` for CI diff.

## 8. Acceptance criteria

- `POST /api/v1/chat/completions` through gateway equals `POST /v1/chat/completions` on virtual ids: same provider choice, same 503 fail-closed, same `Retry-After: 1`.
- `GET /api/v1/credits` and `/api/v1/auth/key` through gateway reach OpenRouter and return the same status/body as a direct call (no double `/api/api`).
- An Opencode custom provider pointing at the gateway runs one disposable coding task (stream + one tool round-trip) end-to-end.

## References

- `docs/api.md` — current endpoint table and the documented double-`/api` gap.
- `docs/architecture.md` — dumb-and-fast realtime path, immutable snapshots.
- `docs/routing.md` — virtual catalog `openrouter/pair/*` contract.
- `crates/gateway/src/lib.rs` — `OPENROUTER_BASE_URL` default, `normalize_pinned_base`, `fallback_proxy`, `forward_upstream`.
- OpenRouter: `https://openrouter.ai/api/v1` (`openapi.json` servers field), `https://openrouter.ai/docs/api_reference/overview.md`.
- Opencode: `https://opencode.ai/docs/providers/` (`baseURL`), `https://opencode.ai/docs/models/` (custom provider + variants).
