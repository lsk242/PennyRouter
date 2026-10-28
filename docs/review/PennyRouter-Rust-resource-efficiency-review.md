# PennyRouter Rust and resource efficiency review

**Purpose:** Identify the highest-value and most aggressive resource optimizations for PennyRouter, including changes that could justify rewriting parts of its stack. The target remains a personal, free-first, low-cost coding-harness router.

**Review date:** 28 October 2026 UTC.  
**Source:** [lsk242/PennyRouter](https://github.com/lsk242/PennyRouter), pinned to [`39356da2af67759751c819e48b98b61b6187fdef`](https://github.com/lsk242/PennyRouter/commit/39356da2af67759751c819e48b98b61b6187fdef), declared version 0.11.0. `main` matched this revision when checked.  
**Related documents:** PennyRouter security and bug review; PennyRouter product viability and improvement review, delivered separately.

## Main conclusion

**There is substantial room to remove unnecessary work and bound resource use without replacing Rust's mainstream HTTP stack.** The strongest opportunities are application-level: body buffering and JSON representation, request lifetimes, routing scans/sorts, snapshot construction, catalog publication, and logging. The current source does not support the stronger claim that Axum, Tokio, reqwest or rustls is the principal bottleneck.

I would undertake a focused internal redesign before a transport rewrite:

1. Establish bounded request, stream and memory lifetimes.
2. Replace whole-request JSON materialization with a validated minimal envelope and borrowed raw payloads.
3. Compile the catalog into a compact, immutable routing generation with precomputed candidate order and cost components.
4. Keep responses pull-driven and release all request resources correctly on completion or cancellation.
5. Make measurement trustworthy, then test runtime, HTTP/2, allocator and build-profile alternatives.

If those changes still leave material CPU or memory overhead in transport machinery, a Hyper-based gateway is a reasonable controlled experiment. A wholesale migration to a specialized runtime is a much higher-risk last step.

**No CPU, latency, allocation, memory, throughput or binary-size gains were measured in this review.** “Observed” below describes source behavior. Proposed benefits are hypotheses to test, not benchmark results. No repository code, build scripts, dependencies, tests or load harnesses were executed or installed.

## What maximum efficiency should mean here

The relevant objective is the least total resource use for correct, completed coding tasks within the user's spending and quality policy. Raw requests per second is secondary for a personal router. A fast request that loses a tool call, repeats a paid attempt, chooses an ineligible model or silently exceeds a budget is a regression.

Separate the resource goals:

- **Idle cost:** resident memory, worker threads, sockets, background wakeups and disk growth when nobody is coding.
- **Active cost:** CPU time and allocated bytes per request, memory per live stream, context switches and network bytes.
- **Tail behavior:** slow clients, stalled providers, concurrent large prompts, cancelled sessions and catalog refresh overlap.
- **Operational cost:** startup, image downloads, rebuilds, logs, catalog maintenance and debugging effort.

For personal use, benchmark one active coding session first, then a small parallel-agent workload. Stress testing much higher concurrency is useful to discover limits, but should not dictate an unnecessarily complicated architecture.

The reviewed lockfile contains Axum 0.8.9, Tokio 1.54.1, Hyper 1.12.0, hyper-util 0.1.21, reqwest 0.12.28, bytes 1.12.1, serde_json 1.0.154, rusqlite 0.37.0 and rustls 0.23.46. The Docker builder uses a moving `rust:1-bookworm` tag, so the exact compiler is not pinned by those crate versions. [Cargo.lock](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/Cargo.lock), [gateway Dockerfile](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/docker/Dockerfile)

## Source grounded opportunity map

Priority reflects likely practical value and correctness, not a measured ranking of CPU samples.

| Priority | Observed source behavior | Resource opportunity | Main caution |
|---|---|---|---|
| P0 | Uploads have a byte cap but no upload deadline or aggregate admission cap | Bound memory retained by incomplete and concurrent requests | Limits must cover the full body/stream lifetime |
| P0 | Successful non-SSE responses and error bodies are collected without a byte bound | Stream or cap before collecting | Preserve headers and visible failure semantics |
| P0 | Stream failures return clean EOF; metrics finish at response headers | Correct lifetime and terminal-state accounting | A faster truncated answer must not count as success |
| P1 | Routed requests become a full `serde_json::Value`, then a new serialized buffer | Avoid a large tree and unnecessary copies | Validate JSON and preserve unknown fields/policy |
| P1 | Free routing scans all offerings and can deep-clone them | Precompute a qualified free candidate index | Expiry and budget checks remain live |
| P1 | Candidate ordering allocates and sorts, even when only the first result is needed | Precompute order or select a minimum in one pass | Keep deterministic and correct selection semantics |
| P1 | Catalog/ranking are separate locked objects; decisions allocate under locks | Publish one immutable routing generation | Do not retain old generations for whole streams |
| P1 | Catalog ingestion issues individual writes; publication reads whole DB files | Transactional batches, streaming hashes and bounded retention | Never weaken artifact durability or integrity |
| P2 | Per-request JSON logs use `println!`; counters include a mutex/map | Bounded asynchronous logging and simpler metric updates | Spending records cannot be dropped like debug logs |
| P2 | Default runtime, connection-pool and release settings are largely untuned | Measure right-sized workers, pools, features and build profiles | Smaller or fewer does not always mean faster |

Primary source locations: [HTTP lifecycle](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs), [routing](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs), [metrics](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/metrics.rs), [catalog store](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/store.rs).

## First bound resource ownership

### Admission before expensive body work

`read_capped_body` appends received chunks into a `Vec` until 4,000,000 bytes. There is no deadline around the upload and no aggregate request/memory admission gate. The upstream timeout begins later. A client can therefore retain buffers while uploading very slowly, and multiple valid large requests multiply memory even on loopback. [lib.rs lines 196–293](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L196-L293)

The cap is approximately 3.815 MiB, not an exact one-million-token limit. JSON structure, tool schemas, encoding and model tokenization prevent `bytes / 4` from being a reliable context or billing guard.

For scale intuition only, 32 simultaneous maximum-size raw input buffers contain about 122 MiB; 128 contain about 488 MiB. These are simple byte calculations, **not RSS measurements or safe upper bounds**. They exclude vector capacity, JSON trees, rewritten output, sockets, TLS, stacks, catalog data and allocator overhead.

Use a request-count gate plus a byte-budget gate before allocating substantial memory. Count the upload phase, not only upstream calls. Use bounded or no waiting; an unlimited queue of waiting requests is still resource consumption. Reject or shed load explicitly. Reserve known content lengths conservatively and enforce actual received bytes; for unknown lengths, avoid a scheme in which many partial uploads each hold some permits while all wait forever for more.

Tokio's owned semaphore permits are one building block. Its documentation also notes fair ordering, so a large multi-permit request can hold up smaller requests. Choose separate size classes or simple conservative reservations if that tradeoff matters. [Tokio semaphore](https://docs.rs/tokio/latest/tokio/sync/struct.Semaphore.html)

### Hold permits until the resource is actually released

A response future completes when it returns response headers and a body, not necessarily when the stream finishes. The current in-flight guard drops at that earlier point. Any future concurrency gate implemented in the same scope would repeat that mistake. [tracking middleware](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L139-L170)

Put active-stream ownership in the returned body or a lifetime wrapper. Release it on normal completion, transport failure, timeout and client disconnect. Release upload/parse memory reservations as soon as their buffers are gone, while keeping active-upstream and spending reservations until their own obligations are resolved. One coarse permit held forever is safe but unnecessarily restrictive; separate resources should have explicit lifetimes.

Distinguish upload idle/total time, connection time, response-header time, first body byte, stream idle time and maximum request/session duration. Currently `first_chunk` wraps `send()`, and the nonstream `total` timeout is a later, separate interval; it is not one wall-clock deadline from acceptance. SSE is allowed to run indefinitely while chunks continue. [timeouts and forwarding](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L35-L58), [forwarding implementation](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L299-L416)

Slow downstream clients need coverage too: an idle timer only running while the body is polled does not establish an independent wall-clock deadline while the consumer stops polling. Test a stalled reader, not just a stalled upstream. Cancellation should stop local reads and release resources promptly; it cannot guarantee that a remote provider stopped generating or billing immediately.

Add graceful shutdown with bounded draining. Do not discard unresolved spending reservations merely because the process is stopping.

## Reduce request copies and parsing

### Current data path

For a routed request, the source performs this sequence:

```text
network chunks
  -> append/copy into Vec<u8>
  -> parse the entire document into serde_json::Value
  -> replace model/provider fields
  -> serialize the entire document into a second Vec<u8>
  -> pass that owned buffer to reqwest
```

The original input, parsed tree and rewritten buffer can overlap during transformation. In the rewritten branch, the original `buffered` binding is not explicitly consumed/dropped before `forward_upstream(...).await`; its source-level lifetime can extend until the handler returns. Confirm allocation lifetimes in an optimized build, but make ownership explicit rather than depending on compiler optimization. Unknown models also pay full JSON parsing when the router is configured, before selection returns `None`. [request path](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L203-L224), [JSON rewrite](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L429-L446)

Moving the final `Vec` into reqwest is not evidence of another full payload copy. Likewise, passing `Bytes` to an Axum body is not inherently a deep copy. Optimize the observed materialization points rather than counting every type conversion as a copy.

### Recommended representation

Parse a small validated envelope containing routing and policy fields: model, provider restrictions, output limit, stream flag, session identity, and the fields needed to establish capability/budget eligibility. Keep large `messages`, `tools`, schemas and other untouched values as borrowed raw JSON. Preserve extension fields rather than silently dropping them.

Serde's `RawValue` supports borrowing a validated JSON value from an input slice and writing its original representation without constructing a full nested value tree. It still requires scanning/validation, and producing one contiguous rewritten request generally still copies the outgoing bytes. The expected benefit is fewer allocations and less repeated parsing/serialization, not magical zero-cost forwarding. [serde_json RawValue](https://docs.rs/serde_json/latest/serde_json/value/struct.RawValue.html)

Implement this in stages:

1. Make the transform consume the input buffer and explicitly release unused input before network waiting.
2. Replace full nested `Value` storage with raw payload borrowing while retaining a correct top-level map/envelope.
3. Reuse one output allocation sized from a bounded estimate. A validated `Content-Length` can inform capacity after admission, but must not trigger unbounded reservation.
4. Consider segmented bodies referencing raw input slices only if profiles show the final copy matters. Segments retain the original backing allocation and can add framing/syscall overhead; fewer copied bytes does not guarantee lower peak memory or latency.

Define duplicate-key handling, escaping, unknown fields, deep nesting and number semantics. Reject ambiguous policy fields rather than letting parser differences create a bypass. Do not replace JSON parsing with substring search or unchecked SIMD parsing over untrusted requests.

`Bytes` supports cheap sharing/slicing, and `BytesMut` supports an owned mutable buffer that can be frozen. These can simplify ownership, but changing `Vec` to `BytesMut` alone does not eliminate the append copy, nor does it remove TLS/kernel copying. A tiny slice can retain a large backing buffer. [Bytes](https://docs.rs/bytes/latest/bytes/struct.Bytes.html), [BytesMut](https://docs.rs/bytes/latest/bytes/struct.BytesMut.html)

### Streaming requests is a separate contract decision

The generic fallback currently buffers every request, even paths that require no body rewriting. Streaming those bodies could reduce memory substantially. However, forwarding before the body is fully validated changes the existing reject-before-upstream behavior. A strict paid inference route must establish policy and reserve budget before any billable forwarding.

Keep a bounded, minimal parse for strict inference. Allow streaming passthrough only on explicitly supported routes with documented limits and authorization. For a coding-only router, declining unsupported upload/audio/image APIs can be simpler and safer than optimizing a universal proxy. Do not “optimize” Responses requests by bypassing the very budget policy the product promises.

## Make response handling bounded and faithful

The SSE path already uses a pull-driven `unfold` and forwards `Bytes` chunks without an explicit unbounded producer queue. Preserve that strength. Hyper's body contract supports backpressure by advancing only when polled. Network stacks still have internal buffers, so this is not proof of a fixed whole-system memory bound. [SSE implementation](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L369-L395), [Hyper streaming bodies](https://docs.rs/hyper/latest/hyper/body/)

Improvements:

- For large successful non-SSE responses, forward bounded chunks instead of `upstream.bytes()`. If a small response must be inspected, collect only under an explicit cap.
- For upstream errors, cap bytes **before** collecting/parsing. The current 200-character displayed snippet is created after collecting the full body, so it does not bound memory. Its character-count check also scans the whole text; after limiting input, inspect only the prefix needed to decide whether to append an ellipsis.
- Preserve required end-to-end headers, including content encoding and retry information. Do not forward compressed bytes while discarding their encoding. Strip hop-by-hop headers deliberately.
- Propagate a stream error as an error, not successful EOF. Do not concatenate a fresh retry into an already delivered model/tool stream.
- Track header latency, first-byte latency and completion latency separately. Record cancelled/truncated streams independently from successful ones.
- If extracting usage from SSE, maintain only a bounded partial-event buffer and a small usage accumulator while forwarding original bytes in order. Chunks are not necessarily events. Bound unusually large event frames; do not reconstruct the entire completion just to price it.

These changes are prerequisites for credible performance measurement. [nonstream/error collection](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L337-L487)

Avoid eager read-ahead tasks unless a benchmark demonstrates a benefit. If one is needed, bound the queue in bytes as well as item count. A queue of ten chunks has no useful memory guarantee if chunk sizes are unbounded. Default to immediate interactive streaming rather than aggressive coalescing that improves throughput by delaying visible tokens.

## Compile routing instead of rebuilding decisions

The current hot path is more than one hash lookup:

- `decide_free` scans every model offering, computes effective cost and collects candidates on every request.
- Its fresh path clones the free offerings, including owned strings, then finds a precision class and sorts candidates.
- Normal/sticky routes build candidate vectors and order them even when they only consume the first item.
- `ordered` uses repeated `position` searches and `Vec::remove` to reconcile published rankings, producing quadratic work in candidate count in the worst case.
- `inline_cost` calls `balanced_profile()`, whose source constructs all three profiles in a `Vec` before returning the first. It is called inside sorting comparisons. Release optimization may remove some allocation, but the representation is needlessly indirect.
- Offline ranking repeatedly parses endpoint payload JSON and resolves price signals inside sort comparisons.

Evidence: [free selection](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L342-L423), [cost and ordering](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L536-L665), [profile construction](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/price.rs#L13-L53), [offline comparator inputs](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/rank.rs#L350-L419).

### A better hot data layout

Build one immutable `RoutingGeneration` offline or during background reload:

```text
generation identity and validity
  model lookup -> compact model IDs
  offering array -> compact endpoint/provider IDs
  approved capability and price facts
  precomputed cost components/profile scores
  candidate order per supported policy/precision group
  qualified free-candidate order with expiry timestamps
```

Use enums for small closed fields such as orientation/tier/known precision and intern repeated identities where measurements justify it. Store raw provider JSON only in the catalog layer, not in every hot decision object. Represent prices with checked units and suitable exact arithmetic for caps; a faster invalid price comparison is not useful.

For fixed candidate sets, pre-sort once and walk until an eligible endpoint is found. For an unranked fallback that needs only one winner, a linear minimum selection avoids constructing a complete sorted output. Cache constant workload profiles directly. Precompute parsed tags and comparator keys once during rank generation.

Do not precompute every combination of arbitrary context sizes, privacy options and budgets: that can explode index size. Keep a short preordered list with cheap dynamic predicates. Expiry, quota, remaining budget and request capability checks must still run when the request arrives. A generation's ordering cannot make an expired free offer valid.

For the actual personal scope, restricting the catalog to the approved coding pool may save more memory and complexity than inventing a sophisticated index over thousands of irrelevant models.

## Shorten lock scope and bound sessions

The router uses `RwLock<Snapshot>`, a separate ranking `RwLock`, and one session `Mutex<HashMap<...>>`. Sorting/filtering occurs while read guards are held; the sticky path can hold the session guard through candidate selection. There is no evidence here of a guard intentionally crossing a network `await`, but long synchronous critical sections still serialize callers and delay refresh. [router state and decision](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L122-L131), [sticky path](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L245-L339)

First shorten critical sections and remove per-request sort work. A brief standard mutex can be appropriate for a few personal sessions; switching every lock to an asynchronous mutex is not automatically faster. Do not shard or build a lock-free session map before measuring contention.

Publish catalog, price facts and ranking as one coherent generation rather than separate swaps. A short `RwLock<Arc<RoutingGeneration>>` is the simple option. `ArcSwap` is a candidate for read-mostly contention, not a mandatory dependency; its documentation warns that reader strategy and reclamation affect behavior. [ArcSwap](https://docs.rs/arc-swap/latest/arc_swap/)

Release the generation reference before waiting on the network. Copy/own only the small decision metadata needed for forwarding and accounting, otherwise long streams can keep old catalogs alive across several refreshes. Include old-plus-new generation memory in the refresh budget.

Session state needs a bounded entry count **and** expiry/eviction. The current cap is 16,384 entries, and keys retain raw authorization strings plus model names. Use an opaque keyed identity and actual conversation ID, avoid retaining raw credentials, and store the selected model/endpoint with policy generation. Revalidate eligibility on every reuse. A small default sized for personal sessions is more defensible than retaining stale state indefinitely. [session key and cap](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L95-L131), [pin insertion](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L264-L329)

## Reduce catalog CPU memory and disk churn

Catalog work is off the inference path, which is good, but it still consumes the same laptop or small VM.

**Batch database work.** The reviewed sync/process callers perform repeated inserts and updates without an enclosing transaction. Reuse prepared statements inside a transaction for a complete bounded import or defined batch; commit before publication. Do not disable durability globally to win a benchmark. SQLite documents the difference between transaction throughput and individual statement throughput, and rusqlite provides explicit transaction support. [sync caller](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/main.rs#L81-L110), [store writes](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/store.rs#L139-L216), [SQLite FAQ](https://www.sqlite.org/faq.html#q19), [rusqlite transactions](https://docs.rs/rusqlite/0.37.0/rusqlite/struct.Transaction.html)

**Avoid all-input materialization where it is unnecessary.** `load_endpoints_dir` reads every endpoint file into memory, `all_endpoints` collects all rows including raw payloads, and gateway loading reparses payloads to obtain tags. Iterate files/rows, project only required fields and normalize tags once. A tiny catalog may not justify every change, but these are direct levers when it grows. [sync loading](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/sync.rs#L17-L36), [gateway loading](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L154-L183)

**Publish without full-file heap copies.** Catalog publication and verification use `fs::read` on the entire database. Hash with a bounded read buffer; publish a completed temporary artifact atomically. Do not hash and then reopen a mutable file that could change between verification and use. Preserve integrity, schema validation and last-good recovery. [artifact publication](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/artifact.rs#L32-L74)

**Bound retained artifacts.** Timestamped content changes can generate new hash-named artifacts even when useful pricing facts are unchanged. Keep a deliberate small last-good history, avoid rewriting unchanged semantic content, and prune only after the new generation is verified and active. Do not remove artifacts still needed by active readers or rollback. A current-generation manifest avoids scanning/sorting an ever-growing directory on every refresh.

**Consider a gateway-only compiled artifact.** The gateway currently depends on the catalog crate and opens SQLite at load time. Splitting a small shared schema/policy crate from catalog tooling could let it load a compact routing artifact without linking its SQLite reader. This may reduce binary/startup complexity, but exact size/RSS savings require measurement; Rust already removes some unused code. Start with a simple versioned serialized format. Memory mapping, archived zero-copy structures and custom binary layouts add validation/alignment/versioning risks and are justified only if the artifact is large enough to matter.

Run rebuilds in a bounded worker/process outside async request execution. Coalesce repeated file notifications and limit refresh frequency while honoring expiry. A separate scheduled job avoids leaving a full maintenance process awake for a tiny personal deployment; keeping it separate also protects inference from synchronous DB/hash work.

## Networking runtime and observability tuning

### Keep the existing shared client

`AppState` creates one reusable reqwest client. Preserve connection pooling rather than constructing a new client for each request. Set redirect, timeout and pool behavior deliberately; the current builder silently falls back to a default client if configured construction fails, which can lose intended timeout settings. A startup configuration failure should be visible. [client construction](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L77-L95)

Reqwest 0.12.28 documents a 90-second idle pool timeout and no default maximum idle connections per host. These are idle-pool controls, not active-request concurrency limits. A small personal pool can reduce retained sockets, but over-aggressive eviction causes new DNS/TCP/TLS work and slower next requests. Test cold and warm connections separately. [reqwest client settings](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html)

### Evaluate HTTP version and TLS rather than assuming

The gateway disables reqwest default features and enables only `rustls-tls`; it does not explicitly enable `http2`, and the reviewed lockfile has no `h2` package. Reqwest documents HTTP/2 as a separate default feature. Confirm the compiled feature graph and negotiated protocol in the future benchmark before claiming multiplexing. [gateway manifest](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/Cargo.toml), [reqwest features](https://docs.rs/crate/reqwest/0.12.28/features)

Test HTTP/1.1 pooling against explicit HTTP/2 support for concurrent streams. Multiplexing can reduce connection count but adds connection/stream flow-control state and can concentrate failure or contention. Do not blindly enlarge flow-control windows; they affect buffering. One personal stream may gain little.

Keep certificate and hostname validation. Do not disable TLS, DNS verification or safe header handling as a speed trick. HTTP/3, custom DNS resolvers, TCP tuning and kernel-bypass ideas are later experiments only if the target deployment shows a specific bottleneck. For a local harness hop, loopback avoids unnecessary remote hosting; an optional Unix socket is useful only if the harness actually supports it.

### Right size the executor

The server uses the default multithreaded Tokio runtime. Benchmark an explicitly small worker count and a current-thread variant under the target CPU quota. Current-thread can reduce scheduling machinery, but a large synchronous parse or blocked log write would then stall all requests. It does not automatically remove `Send`/`Sync` requirements from Axum's types. [server entry point](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/main.rs), [Tokio runtime builder](https://docs.rs/tokio/latest/tokio/runtime/struct.Builder.html)

For genuinely expensive parsing/rebuild work, a bounded blocking worker can protect streaming responsiveness. Do not call `spawn_blocking` for every tiny operation. Tokio warns that its blocking queue does not provide application backpressure and already-started blocking tasks cannot simply be aborted. Bound submissions and keep work finite. [spawn_blocking behavior](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)

### Keep observability cheap without losing accountability

Each request creates a JSON log object and calls `println!`; fallback calls also log the full path/query. Standard `println!` locks stdout per call, so a slow logging destination can become part of the request's cost. Use structured events with fixed fields and a bounded writer, sample routine success logs if appropriate, and count dropped diagnostic events. Never log prompts, credentials or raw query secrets. Do not treat a lossy debug-log queue as the authoritative spending ledger. [logging source](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L141-L169), [Rust println](https://doc.rust-lang.org/std/macro.println.html)

Metrics already use fixed route categories and relaxed atomics, useful choices. Potential further changes are fixed status slots instead of a mutex-protected map, and one noncumulative histogram increment per request with cumulative exposition at scrape time. Measure first: an array uses more fixed memory, and sharded counters/cache-line padding trade memory for less contention. Keep exact status/error meaning and stream-lifetime metrics before chasing counter speed. [metrics implementation](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/metrics.rs)

## Build allocator and packaging experiments

The workspace has no custom release profile. Test one change at a time: normal optimized release, thin LTO, codegen-unit choices, and separate throughput-oriented versus size-oriented optimization. Strip release symbols only while retaining a matching debug artifact for diagnosis. Cargo explicitly advises experimentation: `opt-level=3` is not always faster than `2`, and size modes are not always smallest. `panic=abort` changes failure behavior; it is not a free runtime optimization. [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)

Pin the compiler and base-image identity for comparisons. Use CPU-specific builds only for a known deployment target; `target-cpu=native` is inappropriate for an unrestricted portable release. Profile-guided optimization is an advanced candidate after a representative workload exists, with held-out cases to detect overfitting. [Rust PGO](https://doc.rust-lang.org/rustc/profile-guided-optimization.html)

Compare the system allocator with a supported mimalloc or jemalloc integration only after reducing allocation volume. Measure peak RSS, post-burst retained memory, tail latency, fragmentation and idle behavior, not just allocation microbenchmarks. An allocator can improve throughput while retaining more pages. Do not add several allocator dependencies or accept platform maintenance costs without a repeatable benefit. [mimalloc](https://github.com/microsoft/mimalloc), [Rust jemallocator integration](https://github.com/tikv/jemallocator)

Avoid unbounded global buffer pools. Keep size classes and a strict byte cap, discard oversized buffers, and ensure old prompt/credential contents cannot escape through reuse. Reusing a huge buffer for a tiny request can make idle RSS worse.

The existing multistage non-root Docker image is a reasonable baseline. Smaller/distroless or static-musl variants may reduce image transfer/storage, but image megabytes are not runtime RSS. Verify dynamic libraries, resolver behavior, certificates, diagnostics and target performance before changing the base. A native local binary plus an on-demand catalog updater may be the simplest low-overhead personal deployment. Do not add sidecars or distributed state just to report smaller gateway-only numbers.

## Which rewrites are worth considering

| Option | Recommendation | What would justify it |
|---|---|---|
| Keep Axum/Tokio/reqwest/rustls, rewrite body/routing internals | **Preferred first** | Directly addresses observed redundant work with limited transport risk |
| Replace full `Value` parsing with Serde raw/borrowed envelopes | **High-value prototype** | Lower allocation and parse CPU on large real request shapes, with equivalent validation |
| Evaluate sonic-rs or simd-json | **Conditional parser experiment** | Parsing still dominates after representation changes; correctness and target-CPU tests pass |
| Replace Axum/reqwest layers with a focused Hyper/hyper-util transport | **Conditional partial rewrite** | Profiles attribute meaningful remaining cost to removed layers, not to application work |
| Adopt Pingora | **Not the default personal choice** | The project develops genuine proxy-engine requirements that its framework solves better |
| Move to a thread-per-core specialized runtime such as Monoio | **Last-stage experiment** | Scheduler/syscall overhead is measured as material and target/platform constraints are acceptable |
| Handwritten HTTP, TLS, unsafe JSON or custom lock-free memory reclamation | **Reject as an initial strategy** | No evidence here warrants the security and maintenance burden |

Sonic-rs offers lazy field access, while simd-json is a SIMD-oriented parser. Benchmark complete parse/validate/rewrite behavior, including any required input mutation or copying, not vendor throughput figures. Do not use unchecked APIs on unvalidated payloads. [sonic-rs](https://github.com/cloudwego/sonic-rs), [simd-json](https://github.com/simd-lite/simd-json)

A Hyper rewrite can use concrete body types and explicit protocol settings, but also makes PennyRouter responsible for more connection management, TLS integration, URI handling, headers, cancellation, error normalization and tests. Hyper already underlies the existing stack, so replacing wrappers does not automatically remove the underlying work. [Hyper bodies](https://docs.rs/hyper/latest/hyper/body/), [hyper-util pooled client](https://docs.rs/hyper-util/latest/hyper_util/client/legacy/struct.Client.html)

Pingora is a Rust proxy framework with HTTP/1 and HTTP/2 support; its scale-oriented features are not proof that it has the smallest personal idle footprint. Monoio uses a thread-per-core model and its own I/O abstractions, bringing compatibility/platform tradeoffs. Neither project's published benchmarks establish a PennyRouter improvement. [Pingora](https://github.com/cloudflare/pingora), [Monoio](https://github.com/monoio-rs/monoio)

Use a common transport interface and the same conformance corpus to compare implementations. Keep the current implementation as a rollback path until the replacement wins under equal limits and passes every policy, streaming and cancellation test. Do not retain a more complex rewrite merely because it was difficult to build.

The most radical alternative is to remove the local HTTP hop by embedding the policy engine in a harness integration. That could remove proxy transport work, but would sacrifice the current drop-in endpoint and couple the project to harness APIs and release cycles. Treat it as a different product boundary, not an obviously superior optimization. A small reusable policy core could support both forms without immediately committing to either rewrite.

## Repair measurement before trusting optimization claims

The repository has a useful comparison scaffold, but it cannot yet substantiate an extreme-efficiency claim.

### Specific issues in the current harness

1. **Default workload does not exercise configured routing.** `chat_workload.py` requests `openai/gpt-oss-20b`; the shipped catalog fixtures contain other model IDs, and `compare.sh` does not pass a different model. It therefore does not establish the cost of provider rewriting or free-model scanning.
2. **Routing startup is ambiguous.** Compose starts gateway/catalog without routing readiness; startup-only loading can leave routing disabled. A previously populated volume can change the next run's behavior.
3. **Keep-alive claims are not established.** The mock subclasses `BaseHTTPRequestHandler` without overriding `protocol_version`, whose documented default is HTTP/1.0. The comments about keep-alive are insufficient evidence of persistent mock connections.
4. **Truncated SSE can pass.** The client success predicate accepts a `chat.completion` substring **or** `[DONE]`; a partial stream can satisfy it without a terminal event.
5. **Tiny canned payloads miss the costly cases.** The default prompt is “Say ok”, output is tiny, and streams are short. No long coding history/tool schema/patch workload is represented by that default.
6. **The paths differ.** Direct traffic uses host loopback; gateway traffic crosses a container boundary and reaches the host mock differently. The difference includes topology, not just Rust processing.
7. **The measurements are incomplete.** Direct phases precede gateway phases, the workload is closed-loop, the generator/mock can become bottlenecks, and the comparison run does not record CPU/allocation/RSS/stream cancellation. A closed-loop client reduces offered load when the server slows, masking overload unless separately tested. Failed/timed-out requests do not enter its latency samples, so better-looking success percentiles can coexist with worse reliability. The separate `run.sh` does sample Docker stats, but for its health workload, not this inference comparison.
8. **Subtracting p99s is not per-request p99 overhead.** `p99(gateway) - p99(direct)` is a comparison of distribution quantiles, not the quantile of matched request overhead. Report it accurately.

Sources: [comparison runner](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/tests/load/compare.sh), [workload client](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/tests/load/chat_workload.py), [mock](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/tests/load/mock_backend.py), [report calculation](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/tests/load/compare.py), [health-load runner](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/tests/load/run.sh), [Python HTTP server protocol default](https://docs.python.org/3/library/http.server.html#http.server.BaseHTTPRequestHandler.protocol_version).

The checked `tests/load/results` directory contains only its placeholder, not stored performance results. Repository descriptions of previous local runs are not independent measurements made by this review.

### A reproducible safe benchmark plan

This is a proposed plan, **not authorization to execute it or a claim that it ran**. Use a disposable, resource-capped environment, dummy keys and synthetic/redacted prompts. Block unintended external egress; point every upstream at a verified local mock. Review scripts before running them, especially cleanup/Compose commands. Real-provider trials require separate authorization and a small explicit spending cap.

**Freeze the baseline.** Record commit, compiler, target triple, lockfile hash, enabled features, profile/flags, executable hash, image digest, kernel, CPU model, CPU quota/affinity, memory limit, allocator and complete nonsecret configuration. Pin the routing artifact and assert the chosen model/endpoint at the mock. Record routing-ready state rather than only `/health`.

**Use three layers.** Microbench pure envelope parsing and routing with inputs constructed outside the timed section; use end-to-end mock HTTP tests for bodies/pools/streams; then measure realistic harness tasks separately. A pure lookup win does not establish a coding-session win.

**Exercise a workload matrix.** The following sizes and concurrency points are proposed test cases, not observed user behavior:

| Dimension | Cases |
|---|---|
| Request bodies | 1 KiB, 16 KiB, 256 KiB, 1 MiB, near 4,000,000-byte cap; escaped/non-ASCII strings and many small fields |
| Coding content | Short prompts, long histories, large tool schemas, tool results, long patches, thinking/tool continuations |
| Routing | Disabled passthrough, unknown model, known model, sticky hit/miss, free route, expired promo, no eligible candidate |
| Catalog | Small approved personal pool, realistic expanded pool, deliberately large synthetic stress set |
| Concurrency | 1, 4, 16, then controlled saturation; include long-lived streams rather than only fast requests |
| Upstream behavior | Warm/cold connection, HTTP/1.1 and HTTP/2, delayed headers, delayed first byte, irregular SSE chunks, huge errors, reset/stall |
| Downstream behavior | Fast reader, throttled reader, abandoned upload, midstream disconnect, cancellation during shutdown |
| Maintenance | Idle, valid reload, rejected/corrupt reload, repeated refresh, burst then idle |

After representative traces exist, replay their measured size/turn/stream-duration distributions. Do not call an arbitrary uniform mix “realistic”. Preserve privacy when collecting traces; field sizes and timing are often enough without storing source code.

**Measure the right quantities.** Record CPU microseconds per successful request and per input MiB, allocations/count/bytes, peak live heap, peak RSS, cgroup memory including relevant kernel/page-cache effects, thread/socket/FD counts, context switches, bytes in/out and disk/log growth. Report idle and post-burst retention. For streams, measure RSS delta against controlled counts of identical live streams, duration and byte rate; an average slope is not a universal per-stream constant.

Record response-header latency, first body byte, first useful token/tool event, completion latency, inter-chunk gaps and cancellation-to-release time. Report p50/p95/p99 with sample counts and run variability. Track errors and policy rejections separately; rejected work must not inflate useful throughput. Use profiling/allocation instrumentation in separate runs where it perturbs timing.

**Control the experiment.** Warm up deliberately; alternate or randomize direct/baseline/candidate order; use the same network placement and mock workload. Keep generator and mock outside the gateway's CPU/memory budget and monitor that neither saturates. Run both session-like closed-loop and bounded open-loop offered-load tests. Repeat until differences can be distinguished from noise, retaining raw observations and configurations.

**Make correctness the gate.** Compare full JSON/tool events and terminal semantics, not substrings. Test the security review regressions: policy preservation, exact endpoint/price constraints, compressed headers, stream failures, promo expiry, stale catalog, malformed input and resource release. Include hard-budget tests under concurrent retries and missing usage. Never make a candidate look faster by dropping accounting, validation, TLS or backpressure.

## Recommended implementation order and stopping rules

**Stage 1 — Resource correctness.** Add admission, upload/response bounds, complete stream ownership, cancellation and accurate metrics; repair measurement. Accept when slow/malicious/failing cases stay within configured limits and all resource reservations return to a correct terminal state. This is valuable regardless of benchmark speed.

**Stage 2 — Remove redundant work.** Simplify constant profiles, consume/release buffers explicitly, adopt minimal raw envelopes, precompute candidate lists and shorten lock scope. Accept only with equivalent routing/policy outcomes and measured improvement in the relevant allocation/CPU or memory metric.

**Stage 3 — Shrink persistent state.** Bound sessions/logs/artifact history, batch catalog writes, stream hashes and publish coherent generations. Measure both steady state and reload peaks. For the personal model pool, prefer a simple representation if a more elaborate one has negligible benefit.

**Stage 4 — Tune the retained stack.** Compare worker counts, HTTP versions/pools, release profiles and allocators one at a time. Keep changes only when they improve the chosen primary metric without unacceptable tail-latency, memory, portability or maintenance regressions.

**Stage 5 — Prototype one rewrite.** If profiling still identifies a material wrapper/runtime bottleneck, compare a minimal Hyper transport or one alternative parser/runtime against the optimized baseline. Reuse all invariants and tests. Stop the rewrite if the benefit is within noise, only appears at irrelevant concurrency, or depends on removing safeguards.

Do not define “maximum” as endless microoptimization. Once the router meets its selected footprint/latency budget under realistic use and maintains a safe margin under stress, further complexity needs a concrete operational payoff. At that point, avoiding wasted model turns and preserving cache/tool correctness may save the user more than another low-level tuning pass.

## Review boundaries

This report combines source inspection, an independent Astra static review and current primary Rust/library documentation. Source-level operation counts and byte arithmetic are not measured allocation counts or memory profiles. Compiler optimizations, allocator behavior, feature unification, platform networking and real workload distributions can change the magnitude of any proposed gain.

No project code, repository script, dependency installation, Rust test suite, Docker build, profiler, load test or paid inference call was executed. No implementation, commit, PR or deployment was made. The benchmark plan and stack changes remain proposals requiring a separately authorized execution task.

**Bottom line:** the best aggressive redesign is a bounded streaming gateway with a compact precompiled policy index and minimal request parsing. Remove unnecessary application work first. Replace the HTTP/runtime stack only if a fair benchmark shows that the remaining cost really lives there.
