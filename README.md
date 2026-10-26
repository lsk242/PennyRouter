# PennyRouter

[![Security](https://github.com/lsk242/PennyRouter/actions/workflows/security.yml/badge.svg)](https://github.com/lsk242/PennyRouter/actions/workflows/security.yml)
[![Docker](https://github.com/lsk242/PennyRouter/actions/workflows/docker.yml/badge.svg)](https://github.com/lsk242/PennyRouter/actions/workflows/docker.yml)
[![API: OpenAI compatible](https://img.shields.io/badge/API-OpenAI%20compatible-blue.svg)](docs/api.md)
[![Rust](https://img.shields.io/badge/built_with-Rust-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

> Every token at its best price.

PennyRouter is a blazing-fast, resource-frugal AI gateway. It speaks the
OpenAI API on the way in, routes each request to the best-matching backend
provider, and streams the answer back — while running comfortably on roughly
1 CPU and 1 GB of RAM. Its edge is not the routing call itself (a single
local lookup) but the continuously re-ranked provider index behind it, which
tracks real price, latency, throughput, and availability fluctuations that
static sort orders miss.

Status: minimal gateway (`GET /health`) + Docker setup in place;
OpenRouter passthrough not built yet. See [TODO.md](TODO.md) for the
iteration plan and [Operations](docs/operations.md) to run it.

## Constraints

- Rust implementation, built from scratch for performance.
- Docker support for local development.
- Ballpark target: ~1 CPU, ~1 GB RAM under high volume.
- Minimal added latency; support requests up to 1M tokens.
- OpenAI-compatible input; OpenRouter is the first backend target.
- The incoming request carries the backend key; no local key config for now.

## Docs

All documentation lives in [`docs/`](docs/README.md) (Markdown only):

- [Vision](docs/vision.md) — tagline, pitch, goals, non-goals.
- [Architecture](docs/architecture.md) — long-term design and repo layout.
- [Roadmap](docs/roadmap.md) — iteration detail with validation criteria.
- [API](docs/api.md) — OpenAI-compatible contract.
- [Routing](docs/routing.md) — provider routing and filter syntax.
- [Ranking index](docs/ranking-index.md) — the dynamic provider index.
- [Pricing](docs/pricing.md) — token price model without a classifier.
- [Performance](docs/performance.md) — budgets and benchmarking.
- [Operations](docs/operations.md) — Docker dev, config, observability.
- [Platform](docs/platform.md) — deployment track and delivery pipelines.
- [Future](docs/future.md) — explicitly deferred ideas.

## License

MIT — see [LICENSE](LICENSE).
