# Performance

The product is built from scratch in Rust to be fast on tiny resources.

## Targets (ballpark, not hard limits)

- Footprint: ~1 CPU, ~1 GB RAM under high volume.
- Added latency: minimal gateway overhead on top of backend time; the
  gateway must never be the bottleneck. (Numeric budget set at Iter 3 from
  measured baseline.)
- Throughput: sized for high volume within the footprint above.
- Request size: up to 1M tokens.

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

## Rules

- No network calls on the request path except the single backend forward.
- No per-request work that can be precomputed into the index.
- Stream responses; never buffer a full completion to make a decision.
- Preserve SSE bytes and ordering through the gateway.
- Bound queue depth and concurrent upstream requests; shed load explicitly.
- No per-request catalog DB reads; realtime routing uses the in-memory
  snapshot.
- Timeouts distinguish connect, first-byte, idle, and total time.
