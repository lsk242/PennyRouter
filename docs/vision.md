# Vision

## Tagline

Recommended: **Every token at its best price.**

Alternates considered:

- The penny-wise AI gateway.
- Route smart. Spend pennies.

## Elevator pitch

PennyRouter is a blazing-fast, resource-frugal AI gateway. It speaks the
OpenAI API on the way in, routes each request to the best-matching backend
provider, and streams the answer back — while running comfortably on roughly
1 CPU and 1 GB of RAM. Its edge is not the routing call itself (a single
local lookup) but the continuously re-ranked provider index behind it, which
tracks real price, latency, throughput, and availability fluctuations that
static sort orders miss.

## Goals

- Be the cheapest way to reach any model without thinking about providers:
  the default route should be good enough that most callers never pass a
  filter.
- Add near-zero latency on top of the backend; never be the bottleneck.
- Run high volume on minimal resources (~1 CPU, ~1 GB RAM ballpark).
- Support requests up to ~1M tokens (bytes/4 estimate, 4 MiB cap).
- Stay OpenAI-compatible on input so existing clients work unchanged.
- Build everything from scratch in Rust, optimized for performance.

## Non-goals (for now)

- No classifier on non-smart ids — routing is lookup-based, not
  content-based, except the opt-in smart leg (`openrouter/pair/smart[/max]`,
  Iter 26), where a decision model picks the tier before lookup dispatch.
- No local serving-key management — the caller supplies the backend key.
- No catalog-routed backends beyond OpenRouter yet — Modal/Baseten
  serve via provider-pinned direct routes (Iter 16), not the index.
- No realtime-judge/parallel fan-out features yet — see [future.md](future.md).
