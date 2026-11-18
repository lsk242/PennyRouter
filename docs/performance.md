# Performance

The product is built from scratch in Rust to be fast on tiny resources.

## Targets (ballpark, not hard limits)

- Footprint: ~1 CPU, ~1 GB RAM under high volume.
- Added latency: minimal gateway overhead on top of backend time; the
  gateway must never be the bottleneck. (Numeric budget set at Iter 3 from
  measured baseline.)
- Throughput: sized for high volume within the footprint above.
- Request size: up to ~1M tokens (bytes/4 estimate, 4 MiB cap).

## Method (from Iter 3)

- Load harness comparing gateway vs. direct-to-backend latency
  (histogram, not just averages) under a fixed Docker resource profile.
- Every later iteration re-runs the baseline and must show no regression
  before merging.
- Realtime path stays allocation-lean: parse, one index lookup, forward,
  stream. All heavy work (sync, ranking) is periodic and off-path.
- Guards account incrementally for very large or streamed payloads; they
  reject oversize work before contacting the backend.
- Benchmarks retain fixtures, raw outputs, and binary/config hashes; result
  files refuse to overwrite existing evidence.
- Measured 2026-11-04 (Q15/Q17): N=5 `compare.sh` at defaults (20
  sessions × 15s, 20ms mock, 1 CPU/1 GB): json overhead median p50
  +0.28ms / p99 +0.43ms, sse +0.20ms / +0.11ms, 0 errors; gateway
  peak RSS 15.0–15.5 MiB flat across runs (r2–r5 `stats.jsonl`).
  Virtual routed load (`openrouter/pair/low`, 20 sessions × 30s,
  scored fixture catalog): 4163 requests, 0 errors, peak RSS 11 MiB
  (local release binary). No buffer growth, no leak signature — no
  memory lever pulled; I05 lock work stays deferred behind numbers.
- Chat/streaming workloads and the overhead report landed in Iter 3
  (`tests/load/compare.sh` + mock backend); the health-only scaffold
  (`tests/load/run.sh`) remains for session/latency smoke runs.

## Rules

- No network calls on the request path except the single backend forward.
- No per-request work that can be precomputed into the index.
- Stream responses; never buffer a full completion to make a decision.
- Preserve SSE bytes and ordering through the gateway.
- Bound queue depth and concurrent upstream requests; shed load explicitly.
- No per-request catalog DB reads; realtime routing uses the in-memory
  snapshot.
- Timeouts distinguish connect, first-byte, idle, and total time.
