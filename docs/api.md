# API

OpenAI-compatible surface. Existing OpenAI clients should work by changing
only the base URL and model string.

## Endpoint (Iter 1–2)

`POST /v1/chat/completions` — accepts the standard OpenAI chat-completions
body (including `stream: true` from Iter 2), forwards to the backend, and
returns the backend response unchanged in shape.

## Fallback proxy (Iter 8)

Any path without a custom handler is proxied to the configured upstream
untouched — same method, path, query, headers, and body — and answered
with the upstream response as-is. The upstream URL is simply
`{base}{path}`, so OpenAI-shaped client paths compose with the default
base (`https://openrouter.ai/api` + `/v1/models` just works).

Every fallback call emits a log line with its method and full path, so
no client call ever goes silent:

```json
{"fallback_proxy":true,"method":"GET","path":"/v1/models"}
```

The 1M-token body cap, timeouts, and error mapping apply to fallback
calls exactly as to chat completions; no routing or rewriting applies.
Metrics count them under `route="other"` (labels stay bounded — the
path lives only in logs).

## Endpoint status

| Client path | Behavior |
|---|---|
| `POST /v1/chat/completions` | Custom handler: guard, provider routing, streaming |
| `POST /v1/responses` | Same routed handler as chat (Iter 14): model rewrite, budget, floor, retry, rejections |
| `POST /api/v1/chat/completions`, `POST /api/v1/responses` | OpenRouter-native aliases (Iter 25): same routed handler as `/v1/…`; the upstream path is rewritten to `/v1/…` before forwarding |
| `modal/<model>`, `baseten/<model>` on either routed endpoint | Provider-pinned direct route (Iter 16): stripped model forwarded to that provider's upstream, OpenRouter fields dropped, caller auth unchanged |
| `GET /` | Landing page: binary version + build hash, config status, links; always served locally (never proxied) |
| `GET /ui` | Debug UI (Iter 17): read-only snapshot, ranking, metrics page; registered only with `GATEWAY_UI=1`, otherwise falls through to fallback |
| `GET /v1/models` | Curated menu (Iter 13): exactly the six virtual ids, served locally |
| `POST /v1/embeddings`, `POST /v1/completions`, `/v1/audio/*`, `/v1/images/*`, `/v1/moderations`, `/v1/files`, `/v1/batches`, `/v1/fine_tuning/*`, `/v1/vector_stores` | Proxied + logged |
| OpenRouter extras `/api/v1/auth/key`, `/api/v1/credits`, `/api/v1/generation`, `/api/v1/providers`, `/api/v1/models` | Proxied without doubling `/api` (Iter 25): native paths compose against the upstream root (configured base minus its `/api` prefix), so they reach OpenRouter as-is. Reads proxy; management writes (`/api/v1/keys`, `/api/v1/auth/key(s)`, `/api/v1/credits/coinbase`, `/api/v1/workspaces/*/budgets`) answer 403 `management_proxy_disabled` unless `ALLOW_MANAGEMENT_PROXY=1`. Note `GET /api/v1/models` returns the full upstream catalog, unlike the curated six-id `GET /v1/models` menu. |
| Ollama-native `/api/chat`, `/api/generate`, `/api/tags`, `/api/show`, `/api/embed`, `/api/ps`, `/api/version` | Proxied as-is to the configured upstream + logged. OpenRouter hosts no such paths (upstream 404, still logged); with an Ollama upstream they pass straight through. No Ollama↔OpenAI translation yet. |
| Anything else | Proxied as-is + logged |

## Auth

The incoming request carries the backend key (standard `Authorization:
Bearer <key>` header). The gateway forwards it upstream. No local key
configuration exists for now.

## Model + filters

The `model` field carries the OpenRouter model plus optional suffix filters
(see [routing.md](routing.md)). Unknown or absent filters fall back to the
default route — never an error. The `lowest` filter instead rewrites to
the cheapest paid offering catalog-wide (no quality floor) and fails
closed when none is eligible. `openrouter/pair/free/fallback/<tier>`
serves free first, then exactly one pick from the named tier when
free is exhausted. 429 responses carry `Retry-After` plus the
upstream's `X-RateLimit-*` headers when present. A `modal/` or `baseten/` prefix instead
pins the request to that provider's upstream (see [routing.md](routing.md)):
the prefix is stripped, the rest of the model id passes through verbatim,
and the caller's `Authorization` header is forwarded unchanged — one
caller token serves all upstreams (pure BYOK). From Iter 5, routed requests are
forwarded with the stripped model id plus a forced `provider.order`,
and responses carry the chosen provider in `x-pennyrouter-provider`.
From Iter 7, `openrouter/free` routes to any free offering (rewritten
to its backend id) or passes through untouched when nothing is free.
From Iter 13, the virtual catalog `openrouter/pair/[privacy/]<free|low|think>`
is the stable menu (see [routing.md](routing.md)): `free` = zero-cost,
`low` = cheapest basic, `think` = best quality, `decision` =
cheapest decisions-capable (plus a `decision/free` zero-cost scope);
`privacy/` variants additionally enforce zero-retention provider
fields. Virtual requests
retry once on another provider when the first attempt is retryable
(429/502/503/504); anything else keeps exactly-once semantics.
From Iter 14, virtual requests that cannot be served fail closed
with an OpenAI-shaped error instead of leaking the virtual id
upstream: `budget_exceeded` (400) when no candidate fits
`provider.max_price`, `tools_unsupported` (400) when a tool-bearing
request has no tool-capable endpoint (disable `tools` or pick a
tool-capable route), `quality_floor_unmet` / `no_eligible_offering`
(503 + `Retry-After: 1`) when the tier floor or eligibility leaves
nothing to serve. `POST /v1/responses` shares this whole contract.

## Limits

- Requests up to 1M tokens must be supported (guarded from Iter 2: reject
  oversize early, before contacting the backend).
- Timeouts and upstream failures map to OpenAI-shaped error bodies with an
  appropriate HTTP status; the gateway never leaks backend internals beyond
  what the backend itself returns.

## Health check

`GET /health` — no auth, `200 OK` with body `{"status":"ok"}`. Exists for container
orchestration and the Docker build pipeline's smoke check (see
[platform.md](platform.md)). It is outside the OpenAI-compatible surface.

## Compatibility rule

Realtime behavior stays a strict superset of what a direct OpenRouter call
accepts: anything valid against OpenRouter directly must also work through
the gateway (filters aside).
