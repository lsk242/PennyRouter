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
| `GET /v1/models` | Proxied + logged (verified live: 200 with model list) |
| `POST /v1/embeddings`, `POST /v1/completions`, `/v1/responses`, `/v1/audio/*`, `/v1/images/*`, `/v1/moderations`, `/v1/files`, `/v1/batches`, `/v1/fine_tuning/*`, `/v1/vector_stores` | Proxied + logged |
| OpenRouter extras `/api/v1/auth/key`, `/api/v1/credits`, `/api/v1/generation` | No `/v1` alias exists, so these don't compose through the gateway (`{base}{path}` would double `/api`); call OpenRouter directly (verified live: 200). A gateway call is still logged, then answers upstream 404. |
| Ollama-native `/api/chat`, `/api/generate`, `/api/tags`, `/api/show`, `/api/embed`, `/api/ps`, `/api/version` | Proxied as-is to the configured upstream + logged. OpenRouter hosts no such paths (upstream 404, still logged); with an Ollama upstream they pass straight through. No Ollama↔OpenAI translation yet. |
| Anything else | Proxied as-is + logged |

## Auth

The incoming request carries the backend key (standard `Authorization:
Bearer <key>` header). The gateway forwards it upstream. No local key
configuration exists for now.

## Model + filters

The `model` field carries the OpenRouter model plus optional suffix filters
(see [routing.md](routing.md)). Unknown or absent filters fall back to the
default route — never an error. From Iter 5, routed requests are
forwarded with the stripped model id plus a forced `provider.order`,
and responses carry the chosen provider in `x-pennyrouter-provider`.
From Iter 7, `openrouter/free` routes to any free offering (rewritten
to its backend id) or passes through untouched when nothing is free.

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
