# API

OpenAI-compatible surface. Existing OpenAI clients should work by changing
only the base URL and model string.

## Endpoint (Iter 1–2)

`POST /v1/chat/completions` — accepts the standard OpenAI chat-completions
body (including `stream: true` from Iter 2), forwards to the backend, and
returns the backend response unchanged in shape.

## Auth

The incoming request carries the backend key (standard `Authorization:
Bearer <key>` header). The gateway forwards it upstream. No local key
configuration exists for now.

## Model + filters

The `model` field carries the OpenRouter model plus optional suffix filters
(see [routing.md](routing.md)). Unknown or absent filters fall back to the
default route — never an error.

## Limits

- Requests up to 1M tokens must be supported (guarded from Iter 2: reject
  oversize early, before contacting the backend).
- Timeouts and upstream failures map to OpenAI-shaped error bodies with an
  appropriate HTTP status; the gateway never leaks backend internals beyond
  what the backend itself returns.

## Health check

`GET /health` — no auth, minimal `200 OK` body. Exists for container
orchestration and the Docker build pipeline's smoke check (see
[platform.md](platform.md)). It is outside the OpenAI-compatible surface.

## Compatibility rule

Realtime behavior stays a strict superset of what a direct OpenRouter call
accepts: anything valid against OpenRouter directly must also work through
the gateway (filters aside).
