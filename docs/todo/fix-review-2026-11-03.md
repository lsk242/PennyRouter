# Todo — Fix list for the 2026-11-03 full review

> **Source:** [PennyRouter-full-review-2026-11-03.md](../review/PennyRouter-full-review-2026-11-03.md)
> (B01–B18, C01–C08, P01–P12, D01–D10, I01–I10, S01–S11, U01–U10).
> **Purpose:** every review finding becomes one agent-ready task here, in fix order.
> Nothing in this file is fixed yet. Do not edit the review to close findings —
> link each fix commit back to the task ID below.

## How to use this list

- Work top-down: F1 first, then F2… Each `## F*` group is one iteration:
  small, testable alone, valuable alone (see `AGENTS.md`).
- Task IDs keep the review IDs (`B01` fixes finding `B01`) so commits and
  the review cross-reference with zero mapping table.
- Every task names: problem, evidence (`file:line` from the review), the
  simplest fix that fits, acceptance (test/command), files, size (S ≤½ day,
  M 1–2 days, L >2 days), and blockers.
- **`Decide first: Qxx`** means: stop, write the options from the Q block
  below as a short proposal, and ask the user for a one-line pick before
  writing code. Do not guess. All other tasks need no decision — just do them.
- Failing test first where a bug is claimed dynamically (see F9 probes):
  if the probe proves the finding wrong, close the task as wontfix with the
  probe result linked, and say so in the commit.

## Principles (from the project goal — do not over-engineer)

- Simple, fast, solid, straightforward. Prefer the smallest change that
  restores the documented promise. One lookup on the request path, no
  network on the routing path (smart leg excluded), ~1 CPU / 1 GB budget.
- No new knobs, tables, semaphores, or frameworks unless a task below
  explicitly calls for one after a decision. Reuse existing helpers
  (`read_capped_upstream`, `normalize_pinned_base`, `ordered`, …).
- Keys never logged, never stored. `cargo test --workspace`,
  `cargo clippy --workspace --all-targets`, `cargo fmt --check` green
  before any fix is called done. Update `TODO.md`/`docs/roadmap.md`
  only when a fix iteration actually lands.

## Decisions that need user feedback first

The fixing agent must present each Q as a tiny proposal (context + options
+ recommendation) and wait for a one-word/yes-no answer. Recommended
options are marked ★.

| Q | Task(s) | Decision in one line |
|---|---|---|
| Q01 | B16 | `modal/` bare id: 400 `model_not_provided` ★ vs keep passthrough |
| Q02 | B08, P12 | `free_until` max-expiry + unknown-error nulls-last ★ vs keep documented intent |
| Q03 | C01, S06 | Move permit into SSE stream state ★ vs separate stream semaphore |
| Q04 | I02 | Queue-wait deadline 30s + 503 ★ vs indefinite wait, documented |
| Q05 | I03, U09 | Joint snapshot+ranking swap (`spawn_blocking` + one `Arc`) ★ if small, else doc-only |
| Q06 | P06, S04 | Decisions-route privacy: document chat-only ★ vs enforce (upstream-400 risk) |
| Q07 | P08 | Add `GET /ready` ★ vs docs-only static-menu note |
| Q08 | S01 | Smart leg: require any caller `Authorization` when operator key set ★ vs metrics-only |
| Q09 | S02, S08 | Keep public BYOK + document loudly ★ vs optional shared-secret gate |
| Q10 | S03 | Canonicalize path before deny check ★ vs allowlist rewrite vs accept |
| Q11 | B17 | Publish gate: fail on 0 models or >50% skipped (★, confirm numbers) vs warn-only |
| Q12 | B18 | Stale ranking: warn-only log ★ vs reject older than 2× sync interval |
| Q13 | B10 | Tiered pricing: document as limitation ★ vs bucketed hot-path pricing |
| Q14 | D07, P04 | Canonical routing deploy: prod compose `CATALOG_DB` ★ vs Terraform-only claim |
| Q15 | I01 | Memory envelope: measure first, then lower defaults or accept — numbers after load run |
| Q16 | I05 | Lock-held scans: refactor to `Arc` handles vs defer with perf trigger ★ defer |
| Q17 | P10 | p99: rerun load harness N=5; open latency iteration only if p99 overhead >2 ms |
| Q18 | C08, U01 | Adopt CRITICAL/HIGH/MEDIUM/LOW rubric for reviews ★ (docs-only, tiny) |

## Fix order at a glance

| Iteration | Contents | Gate |
|---|---|---|
| F1 | C01, C02, C06, C07, C08 (+ probes from U03) | none / Q03 |
| F2 | B01, B02 | none |
| F3 | B03, B04, B05, B06 | none |
| F4a | C03, C04, C05, B12 | none |
| F4b | B07, B08, B09, B17, B18, P12 | Q02, Q11, Q12 |
| F5 | B10, B11, B13, B14, B15, B16 | Q13, Q01 |
| F6 | S01–S11 (P06 decided via Q06; task in F7) | Q06, Q08–Q10 |
| F7 | P01–P12, D01–D10 | Q07, Q14 |
| F8 | I01–I10 | Q04, Q05, Q15–Q17 |
| F9 | U01–U10 evidence closure | mostly verification |

---

## F1 — Bound bypasses and hangs (fix first)

### C01 — SSE concurrency cap released at headers [CRITICAL → also S06]

- **Problem**: the upstream semaphore permit drops when SSE headers return,
  so `UPSTREAM_MAX_CONCURRENT` does not bound live streams and graceful
  shutdown can drain forever on trickling streams.
- **Evidence**: `crates/gateway/src/lib.rs:2452-2455` (`_permit`) vs
  `lib.rs:2505-2512` (stream returned without it); stream state holds only
  `(upstream, metrics, guard, failed)` at `lib.rs:2592-2594`; shutdown
  `crates/gateway/src/main.rs:26-53`; existing tests use a JSON mock, not SSE
  (`crates/gateway/tests/resilience.rs:54-64`).
- **Decide first**: Q03. Recommended (A): add the `UpstreamPermit` to the
  stream state tuple so it drops when the body ends or errors — one-line
  shape change, no new knob. (B) separate stream semaphore: rejected,
  extra state for the same promise.
- **Fix**: as Q03-A. Keep idle-timeout semantics unchanged. No total stream
  cap in this task (stream bytes stay unbounded by design; see I01).
- **Accept**: new test in `crates/gateway/tests/resilience.rs`: SSE upstream,
  `max_upstream=1`, second stream request sheds/queues while first is open,
  proceeds after first closes. `cargo test -p pennyrouter-gateway`.
  Probe result (2026-11-04): pre-fix the second request returned 200
  (slot leaked at headers); post-fix 503 while open, 200 after close
  (`open_sse_stream_holds_the_upstream_slot`, resilience 3/3). S06
  closed by the same test.
- **Files**: `crates/gateway/src/lib.rs`, `crates/gateway/tests/resilience.rs`. **Size: S.**

### C02 — Classifier size cap checked after unbounded download [HIGH → also S07]

- **Problem**: classifier reply is fully buffered before the 64 KiB check;
  a hostile configured upstream can force large allocation in 8 s.
- **Evidence**: `crates/gateway/src/lib.rs:1658-1664` (`bytes().await`) →
  check at `lib.rs:1715-1721`; cap `lib.rs:188-190`. Correct pattern already
  exists: `read_capped_upstream` (`lib.rs:2530-2562`).
- **Fix**: reuse `read_capped_upstream` for the classifier reply with
  `MAX_CLASSIFIER_BYTES`; map oversize to the existing `Invalid` → fallback
  path (`lib.rs:1706-1713`). No new abstraction.
- **Accept**: unit/integration test with a classifier mock streaming >64 KiB
  → request still completes via fallback tier, memory bounded (assert on
  error kind, not RSS). Add probe first (U03): oversized reply today must
  show the unbounded collect (log/response length) before fixing.
  Probe result (2026-11-04): pre-fix the oversized stream waited out
  the 2 s classifier timeout; post-fix it fails fast (~170 ms) via
  `Invalid` → fallback tier (`smart_classifier` 18/18 green).
- **Files**: `crates/gateway/src/lib.rs`, `crates/gateway/tests/smart_classifier.rs`. **Size: S.**

### C06 — Classifier truncation is quadratic [MED]

- **Problem**: `truncate_input` clones + re-serializes the whole array per
  dropped message; ~1M tiny messages (inside the 4 MiB cap) hang a worker
  holding an admission slot.
- **Evidence**: `crates/gateway/src/classifier.rs:54-71`; clones at
  `classifier.rs:31-38`, re-serialize at `classifier.rs:100-108`; permit held
  at `crates/gateway/src/lib.rs:1324-1330`.
- **Fix**: single pass — serialize once, drop oldest until under
  `max_input_chars` by total length (measure with `chars` on the one
  serialization). Keep oldest-drop semantics and the 32k default. No new API.
- **Accept**: probe first (U03): 200k tiny messages today times out /
  >2 s; after fix, classified or fallback within the classifier timeout.
  Existing `smart_classifier` tests stay green.
  Probe result (2026-11-04): 10k-message probe took 77.69 s pre-fix
  (quadratic confirmed), 0.02 s post-fix (linear).
- **Files**: `crates/gateway/src/classifier.rs`. **Size: S.**

### C07 — `split_model` is quadratic on slash-heavy ids [MED]

- **Problem**: unauthenticated `a/a/a/…` model strings (≤4 MiB admitted)
  cause O(segments × length) hashing whenever a router is configured.
- **Evidence**: `crates/gateway/src/routing.rs:3442-3460`; repeated at
  `routing.rs:890-891`, `routing.rs:1970-1977`; admission cap
  `crates/gateway/src/lib.rs:37-39`.
- **Fix**: stop splitting at the first `/` — candidates are bounded by
  catalog model ids. Simplest correct shape: try progressively shorter
  prefixes but hash each head once (iterate segment ends left-to-right
  against `snapshot.models`, longest first) instead of re-hashing the
  whole shrinking string. Keep longest-prefix semantics identical.
- **Accept**: probe first (U03): slash-heavy 100k-segment model string
  today exceeds 2 s; after fix <100 ms and routes/404s identically.
  `crates/gateway/tests/routing.rs` + unit `split_model` tests green.
  Probe result (2026-11-04): 95.39 s pre-fix, 0.03 s post-fix
  (`split_model_stays_linear`, routing 44/44 green).
- **Files**: `crates/gateway/src/routing.rs`. **Size: S–M.**

### C08 — `spawn_reload_task` panics on interval 0; no severity rubric [LOW]

- **Problem**: documented `0 disables` panics inside `tokio::time::interval`;
  only `main`'s guard saves the binary. Reviews also lack a shared rubric.
- **Evidence**: `crates/gateway/src/lib.rs:447-455` (doc), `lib.rs:464-467`
  (interval), guard `crates/gateway/src/main.rs:19-24`.
- **Fix**: guard inside `spawn_reload_task` itself (return a no-op handle on
  zero) so embedders/tests cannot panic; keep `main` guard. Rubric:
  **Decide first**: Q18 — add the 4-level rubric + dedup-by-root-cause rule
  to the header template of future review docs (docs-only, with F7).
- **Accept**: unit test `spawn_reload_task` with `Duration::ZERO` does not
  panic and never ticks. `cargo test -p pennyrouter-gateway --lib`.
  Probe result (2026-11-04): pre-fix panics (proven via stash);
  post-fix no-op handle, reload suite 6/6 green.
- **Files**: `crates/gateway/src/lib.rs`. **Size: S.**
  Decided (2026-11-04, Q18): rubric added to `docs/review/METHOD.md`
  as the header template (Critical/High/Medium/Low + dedup-by-root-cause);
  docs-only, no test applicable.

---

## F2 — Smart classifier correctness

### B01 — Classifier default URL missing `/v1` [HIGH]

- **Problem**: with only `CLASSIFIER_MODEL` set, the classifier leg 404s and
  smart silently falls back forever in the documented default deployment.
- **Evidence**: `crates/gateway/src/lib.rs:204-207` (base reused verbatim),
  default `lib.rs:376-377` (`https://openrouter.ai/api`), leg URL built at
  `lib.rs:1650` as `{base}/chat/completions` vs gateway's own
  `{base}/v1/…` at `lib.rs:1391-1396`.
- **Fix (simplest)**: derive the classifier chat URL once in
  `ClassifierConfig::from_env`: strip a trailing `/v1` with the existing
  `normalize_pinned_base` helper (`lib.rs:435-441`), then append
  `/v1/chat/completions` at call time. Explicit
  `CLASSIFIER_BASE_URL=…/v1` keeps working (stripped then re-added).
  Update `.env.example` comment to state the contract.
- **Accept**: new `from_env` unit tests (default base, `/v1` base, bare
  host) + a mock-classifier integration test asserting the request path is
  `…/v1/chat/completions` (closes I10b for this path).
- **Files**: `crates/gateway/src/lib.rs`, `.env.example`,
  `crates/gateway/tests/smart_classifier.rs`. **Size: S.**

### B02 — Smart ids double-prefix on native alias [HIGH]

- **Problem**: `POST /api/v1/chat/completions` with a smart id forwards to
  `…/api/api/v1/…` and 404s.
- **Evidence**: smart early-return passes the unstripped path
  (`crates/gateway/src/lib.rs:1360-1366`) while the normal path strips at
  `lib.rs:1391-1394`; forward at `lib.rs:2461`.
- **Fix**: run the smart branch through the same strip: pass the already
  normalized `/v1/…` path into `smart_completion` for both chat and
  responses handlers. No new branch.
- **Accept**: integration test: smart id via `/api/v1/chat/completions`
  reaches the mock upstream exactly once at `/v1/chat/completions`
  (extend `crates/gateway/tests/openrouter_native.rs`).
- **Files**: `crates/gateway/src/lib.rs`. **Size: S.**

---

## F3 — Sticky and fallback routing

### B03 — Cross-model sticky pick is arbitrary, not tier-ordered [HIGH]

- **Problem**: after the first request, a `low`/`think`/`lowest`/`decision`
  session gets whichever offering of the pinned provider `HashMap` order
  yields — including the most expensive model/tag — and it changes on reload.
- **Evidence**: `Pin` = provider+class only
  (`crates/gateway/src/routing.rs:102-107`); sticky `find` at
  `routing.rs:1470-1484`, `routing.rs:1574-1589`, `routing.rs:1682-1696`;
  fresh ordering `min_by` at `routing.rs:1486-1502`.
- **Fix**: in each sticky branch, filter candidates to the pinned
  provider+class (as today), then pick with the *same comparator* the fresh
  path uses (`min_by` cost for `low`/`lowest`/`decision`, quality comparator
  for `think`) instead of `.find()`. For `decide_free`/`decide_pinned`,
  reuse `ordered()` on the pinned subset (their fresh path already does).
  No change to `Pin`, session keys, or preview semantics — but re-run the
  inspector tests: preview sticky must still match decide (see D05 tests).
- **Accept**: unit tests: pin a cheap provider, insert a pricier second
  model of the same provider+class, assert the sticky pick is the cheaper
  offering (and stable across a snapshot swap). Existing session tests green.
- **Files**: `crates/gateway/src/routing.rs`. **Size: M.**

### B04 — Fallback leg unreachable after a forwarded retry [MED]

- **Problem**: when two free providers both fail retryably, the configured
  `/fallback/<tier>` never serves; retry-leg 429s also never cool down.
- **Evidence**: early return `crates/gateway/src/lib.rs:1463-1479` skips the
  fallback block `lib.rs:1487-1539` (its own comment at `lib.rs:1482-1486`
  promises otherwise); decisions twin `lib.rs:1212-1241` vs `lib.rs:1245-1299`;
  retry 429 not throttled (cf. `lib.rs:1406-1425`, `lib.rs:1516-1531`).
- **Fix**: fall through instead of returning when the retry response is
  retryable and a fallback target exists; share the 429 `note_throttled`
  block for all three legs (small local helper, no new state).
- **Accept**: integration tests: two free providers 429 → client gets the
  paid fallback pick (chat + decisions); single-provider case unchanged.
- **Files**: `crates/gateway/src/lib.rs`,
  `crates/gateway/tests/virtual_models.rs`, `crates/gateway/tests/decisions.rs`. **Size: M.**

### B05 — `decide_free` sticky ignores throttle cool-down [MED]

- **Problem**: a just-429'd pinned free provider is re-hit and its 429
  returned, despite fresh free alternatives.
- **Evidence**: sticky branch `crates/gateway/src/routing.rs:1791-1819`
  lacks the fresh branch's `skip_throttled` (`routing.rs:1830-1832`);
  legacy `openrouter/free` gets no `retry_body`
  (`crates/gateway/src/lib.rs:1371`, `lib.rs:2053-2059`).
- **Fix**: apply `skip_throttled` to the pinned subset before the sticky
  pick (same helper, same all-throttled-still-serves rule as preview).
  Mirror part of B03's subset handling — do these two together.
- **Accept**: unit test: pin provider, throttle it, next free request picks
  the fresh alternative; all-throttled case still serves the pin.
- **Files**: `crates/gateway/src/routing.rs`. **Size: S** (fold into B03's PR).

### B06 — Direct sticky checks only the ordered head for budget/tools [LOW]

- **Problem**: a fitting second offering of the pinned provider+class is
  ignored; the session needlessly re-pins elsewhere.
- **Evidence**: head-only check `crates/gateway/src/routing.rs:920-942`
  vs fresh filter `routing.rs:953-968`.
- **Fix**: filter the pinned subset by `budget_allows`/`tool_allows`
  *before* `ordered()`, then take the head (mirror the fresh path exactly).
- **Accept**: unit test: two offerings, head over-cap, second fits →
  sticky keeps the provider on the fitting offering.
- **Files**: `crates/gateway/src/routing.rs`. **Size: S** (same PR as B03/B05).

---

## F4a — Catalog write integrity

### C03 — Discount >1 becomes permanently free [HIGH]

- **Problem**: a bad/imported `promo_discount=2.0` publishes as permanent free.
- **Evidence**: no upper check `crates/catalog/src/process.rs:94-101`;
  promos correctly reject >1 (`crates/catalog/src/price.rs:145-149`);
  clamp → cost 0 (`price.rs:75`); published free
  (`crates/catalog/src/rank.rs:431-444`).
- **Fix**: reject/clamp at extraction like promos: out-of-range discount →
  `None` (unknown) instead of storing. One-line bound + test. Do not touch
  the `effective_cost` clamp (defense in depth stays).
- **Accept**: `process` test: discount 2.0/−0.5/NaN never yields cost 0/free;
  `cargo test -p pennyrouter-catalog`.
- **Files**: `crates/catalog/src/process.rs`, `crates/catalog/tests/process.rs`. **Size: S.**

### C04 — Fetch extra-model merge unvalidated; one bad extra aborts sync [HIGH]

- **Problem**: a malformed/mismatched `EXTRA_MODEL_IDS` response kills the
  whole sync (FK error) instead of being skipped like other bad endpoints.
- **Evidence**: only `data` checked (`crates/catalog/src/fetch.rs:60-81`;
  comment `fetch.rs:62-63` overclaims); sync requires `data.endpoints` and
  splits keys (`crates/catalog/src/sync.rs:54-58`, `sync.rs:75-84`,
  `sync.rs:111`); FK `crates/catalog/src/store.rs:100`. Same hole for any
  HTTP-200 malformed endpoints body (`fetch.rs:95-102`).
- **Fix**: validate before push: require `data.id == requested` and
  `data.endpoints` is an array; otherwise `skipped.push(id)` and continue —
  same failure shape as the existing endpoint 404 path. Fix the comment.
- **Accept**: `fetch` tests: mismatched id / missing endpoints / non-JSON
  200 → skipped count +1, sync succeeds for the rest.
- **Files**: `crates/catalog/src/fetch.rs`, `crates/catalog/tests/fetch.rs`. **Size: S.**

### C05 — Non-transactional sync destroys prior data on failure [HIGH]

- **Problem**: re-sync into an existing DB deletes a model's endpoints,
  then a mid-insert error leaves it empty (data loss). Same for
  `run_processing`/judging half-updates.
- **Evidence**: delete-then-insert `crates/catalog/src/sync.rs:85-115`;
  autocommit only (`crates/catalog/src/store.rs:177-193`); in-place updates
  `crates/catalog/src/process.rs:32-46`, `crates/catalog/src/judge.rs:254-263`.
  Fresh work DB (`crates/catalog/src/main.rs:134-135`) hides it for the
  binary path only.
- **Fix (simplest)**: wrap each model's delete+inserts in one rusqlite
  transaction (the store already owns the connection; add a tiny
  `transaction` helper used by sync/process/judge writes). No new tables,
  no WAL change. Do this after the U03 data-loss probe so the probe can
  serve as the regression test.
- **Accept**: probe test: re-sync with a poisoned endpoint keeps the old
  endpoints after the error; row counts unchanged.
  Probe result (2026-11-04): pre-fix the poisoned re-sync left the
  model endpoint-less (delete committed, insert aborted); post-fix
  `failed_resync_keeps_prior_endpoints` keeps all 3 (gather green).
- **Files**: `crates/catalog/src/store.rs`, `sync.rs`, `process.rs`,
  `judge.rs`, `crates/catalog/tests/sync.rs` (or `gather.rs`). **Size: M.**

### B12 — Model-lookup errors swallowed, endpoint errors abort [MED]

- **Problem**: one bad endpoint payload fails the whole processing run,
  while a broken model lookup silently marks every endpoint `unknown`
  (decision-tier misrouting with no signal).
- **Evidence**: `crates/catalog/src/process.rs:24` (`?` abort) vs
  `process.rs:25-31` (`.ok()` → `None` cache).
- **Fix**: make both fail loudly the same way: propagate the lookup error
  (drop `.ok().flatten()`). If a full abort is too strict for live runs,
  count-and-report skipped models in `ProcessSummary` — but never silently
  `None`. Pick failing-loud; the sync container already keeps the last
  good artifact.
- **Accept**: test: corrupt model payload → run errors (or reports the
  skip); no silent all-unknown output.
- **Files**: `crates/catalog/src/process.rs`, `crates/catalog/tests/process.rs`. **Size: S.**

---

## F4b — Ranking and publish correctness

### B07 — Free-offer dedup downgrades permanent free to expiring [MEDIUM]

- **Problem**: if a promo-free row precedes the permanent-zero row for the
  same provider+tag, the permanent row is dropped and the gateway expires it.
- **Evidence**: first-wins dedup `crates/catalog/src/rank.rs:422-429` vs
  per-row static-zero check `rank.rs:439-459`; expiry consumed at
  `crates/gateway/src/routing.rs:454`.
- **Fix**: dedup by best offer, not first: permanent (`valid_until=None`)
  wins over expiring; among expiring, keep the latest `valid_until`
  (pairs with B08/Q02). Small pure-function change in the rank builder.
- **Accept**: `rank` test with both row orders → permanent survives either
  way. **Files**: `crates/catalog/src/rank.rs`, `crates/catalog/tests/rank.rs`. **Size: S.**

### B08 — `free_until` is last-in-file, not latest expiry [MEDIUM]

- **Problem**: overlapping free windows ending Apr then Feb publish Feb;
  the gateway stops serving free two months early.
- **Evidence**: overwrite loop `crates/catalog/src/price.rs:190-201`;
  gateway expiry `crates/gateway/src/routing.rs:448-455`.
- **Decide first**: Q02 (bundled with P12). Recommended (A): return
  `max(ends_at)` over active 100%-off windows. (B) keep last-wins as
  documented intent — then B07's fix alone stands and this task becomes a
  doc clarification in `docs/pricing.md`.
- **Accept** (A): `price` test: Apr+Feb windows → Apr. Probe result
  (2026-11-04): pre-fix published Feb (last-in-file); post-fix Apr in
  both file orders (`free_until_returns_latest_expiry`). Key-check
  follow-up (same day): per-key daily free caps are orthogonal —
  `GET /api/v1/key` reports `free_model_daily_requests`
  used/limit/remaining per key, enforced upstream as 429s, which the
  free leg already converts to retry + fallback; no gateway change,
  one doc sentence in `docs/routing.md`. "No free-until date" means
  no time-boxed promo window, never unlimited freeness. **Files**:
  `crates/catalog/src/price.rs`. **Size: S.**

### B09 — Manifest counts unverified, can diverge from DB [MEDIUM]

- **Problem**: manifests carry caller-guessed counts; inflated counts still
  verify and are displayed as truth.
- **Evidence**: counts written unverified `crates/catalog/src/artifact.rs:69-98`;
  sha-only verify `artifact.rs:103-111`; summary counting
  `crates/catalog/src/sync.rs:71`, `sync.rs:116`; displayed
  `crates/catalog-ui/src/main.rs:140-148`; ground truth
  `crates/catalog/src/store.rs:277-287`.
- **Fix**: in `publish()`, ignore the passed counts and count the DB
  (`Catalog::counts()`) before writing the manifest. Callers keep passing
  them; no signature churn beyond one internal call.
- **Accept**: test: duplicate-model sync input → manifest counts equal
  `counts()`. **Files**: `crates/catalog/src/artifact.rs`. **Size: S.**

### B17 — Half-empty catalog publishes unconditionally [High]

- **Problem**: a sync with most models skipped still publishes; the
  gateway then serves a gutted menu (old B-1, still open).
- **Evidence**: `crates/catalog/src/fetch.rs:49-54`,
  `crates/catalog/src/main.rs:119-122` (print only), `main.rs:151-158`
  (unconditional publish); `docs/review/bug-todo-2026-10-31.md:30-43`.
- **Decide first**: Q11. Recommended (A): fail the cycle (non-zero exit,
  no publish, last-good keeps serving) when models published == 0 or
  skipped > 50% of fetched ids; thresholds are constants, not new env
  knobs. (B) warn-only + `pennyrouter_…` log line. Get the numbers confirmed.
- **Accept**: test: 0-model and >50%-skipped fixtures → no publish /
  non-zero exit; normal fixture publishes. Probe result (2026-11-04):
  pre-fix both published (exit 0); post-fix both refuse with
  "refusing to publish", last-good survives, no new DB, no work DB
  (`sync_cli` 3/3). **Files**:
  `crates/catalog/src/main.rs`, `crates/catalog/tests/…`. **Size: S–M.**

### B18 — No staleness gate on ranking [Medium]

- **Problem**: any hash-valid ranking is accepted forever; a wedged sync
  leaves month-old ordering live with no signal.
- **Evidence**: `created_at` only (`crates/catalog/src/rank.rs:310-317`);
  hash-only verify `crates/gateway/src/routing.rs:3316-3363`.
- **Decide first**: Q12. Recommended (B first): log a warning (and count
  it) when the accepted ranking's age exceeds 2× `SYNC_INTERVAL`;
  rejecting (A) is a follow-up only if the user wants strictness.
- **Accept** (B): reload test with an old `created_at` → warning path
  taken, routing unchanged. **Files**: `crates/gateway/src/routing.rs`
  (+ `lib.rs` if counting). **Size: S.**

### P12 — Unknown error rate ranks as perfect; Quality drops latency [Medium]

- **Problem**: unmeasured endpoints beat measured 0.1%-error ones; equal
  quality/price falls to alphabetical instead of faster.
- **Evidence**: `crates/catalog/src/rank.rs:73-78` (`unwrap_or(0.0)`) vs
  nulls-last `rank.rs:47-66`; missing `latency()` at `rank.rs:94-98`
  (present `rank.rs:84-93`). Intent comment `rank.rs:68-69`.
- **Decide first**: Q02 (same vote as B08). Recommended (A): unknown error
  sorts last like cost/score/latency; append `latency()` to Quality as the
  final signal tie-break before identity. Deterministic-bytes property
  must be re-proven (existing rank tests pin it).
- **Accept**: `rank` tests: unknown vs 0.1% → measured wins; equal
  quality/price → lower latency first. Probe result (2026-11-04):
  both probes failed pre-fix; post-fix green including the updated
  `measured_error_beats_unknown` contract test (old neutral-unknown
  rule overturned by Q02-A). Deterministic-bytes rank suites green.
  **Files**:
  `crates/catalog/src/rank.rs`, `crates/catalog/tests/rank.rs`. **Size: S.**

---

## F5 — Catalog hygiene

### B11 — Sync never prunes removed models [MEDIUM]

- **Problem**: re-sync into an existing DB keeps upstream-removed models
  (and their rankings) forever.
- **Evidence**: replace-only `crates/catalog/src/sync.rs:53-72`; endpoints
  deleted only for present models (`sync.rs:85`); ranked from endpoints
  (`crates/catalog/src/rank.rs:348-364`).
- **Fix**: after a successful full sync into a reused DB, delete models
  absent from the input (and their endpoints) inside the same transaction
  as C05. Library-level correctness; the fresh-DB binary path is unaffected.
- **Accept**: resync test: model dropped upstream → gone from DB + ranking.
- **Files**: `crates/catalog/src/sync.rs`. **Size: S** (after C05).

### B13 — Read-only opens skip schema validation [LOW-MEDIUM]

- **Problem**: a v1 DB opened read-only fails as `no such column` instead
  of a clear schema-version error.
- **Evidence**: `crates/catalog/src/store.rs:107-110` (no checks);
  `crates/catalog/src/bin/catalog-rank.rs:75`; query
  `store.rs:268-274`; version error exists only in migrate
  (`store.rs:130-141`, name-only).
- **Fix**: `open_read_only` runs the same `schema_version` check as the
  gateway loader and returns the Schema error. No auto-migrate on the read
  path. Column-type validation: skip (over-engineering) unless a real
  mis-typed DB shows up.
- **Accept**: test: v1 fixture DB → Schema error, not sqlite column error.
- **Files**: `crates/catalog/src/store.rs`. **Size: S.**

### B14 — Catalog-ui link and price display bugs [LOW]

- **Problem**: model ids with `& + % #` break the endpoints filter link;
  `4.4e-8` prices render as `0.0000000` (looks free).
- **Evidence**: raw href `crates/catalog-ui/src/main.rs:173-177`, parse
  `main.rs:196`; format `main.rs:225-230`; real price
  `crates/catalog/tests/process.rs:42`.
- **Fix**: percent-encode the id in the href (tiny local encoder — no new
  dependency); format prices with adaptive precision (`{v:.7}` only ≥1e-7,
  else scientific). Both are one-function edits.
- **Accept**: catalog-ui test: id `a&b` round-trips the filter; `4.4e-8`
  renders non-zero. **Files**: `crates/catalog-ui/src/main.rs`. **Size: S.**

### B15 — Endpoints-dir id mapping is lossy [LOW]

- **Problem**: `stem.replace("__","/")` corrupts ids containing `__` and
  the payload's own `model_id` is never checked.
- **Evidence**: `crates/catalog/src/sync.rs:22-36` (derive), `sync.rs:86-95`
  (unchecked filing).
- **Fix**: prefer the payload's `model_id` when present; fall back to the
  filename only otherwise, and error on mismatch (same skipped-shape as
  C04). No format migration for existing fixture dirs.
- **Accept**: test: payload id wins; mismatched file/id → skipped, no FK
  failure. **Files**: `crates/catalog/src/sync.rs`. **Size: S.**

### B16 — Empty pinned remainder: 400 vs passthrough [Medium]

- **Problem**: `modal/` (bare prefix) is proxied untouched to the default
  upstream; the bug-todo asked for 400 `model_not_provided`, and a test
  currently asserts the passthrough.
- **Evidence**: `crates/gateway/src/routing.rs:3255-3261`;
  `crates/gateway/tests/providers.rs:262-273`;
  `docs/review/bug-todo-2026-10-31.md:74-82`.
- **Decide first**: Q01. Recommended (A): strict — `modal/` and `baseten/`
  with empty/whitespace remainder → 400 `model_not_provided`
  (OpenAI-shaped), matching virtual fail-closed; update the one test.
  (B) keep passthrough and close the todo item as wontfix-documented.
- **Accept** (A): test asserts 400 + code; non-empty pinned routes unchanged.
  Probe result (2026-11-04): pre-fix 200 passthrough; post-fix 400
  `model_not_provided` for `modal/`, `baseten/`, `modal/ ` with zero
  upstream hits (`empty_remainder_fails_closed`, providers 7/7). The
  empty check runs before `split_pinned` so whitespace can't slip
  through as a model id.
- **Files**: `crates/gateway/src/routing.rs` (+ handler in `lib.rs`),
  `crates/gateway/tests/providers.rs`. **Size: S.**

### B10 — Tiered (long-context) pricing ignored [MEDIUM]

- **Problem**: `pricing.overrides` (≥272k tokens at 2×) is ignored, so
  long-context picks are underpriced ~2× in every orientation.
- **Evidence**: `crates/catalog/src/process.rs:35-38`, `process.rs:56-63`;
  fixture `crates/catalog/tests/fixtures/endpoints/openai__gpt-6.1-terra-pro.json`.
- **Decide first**: Q13. Recommended (A): documentation only — record the
  limitation in `docs/pricing.md` (overrides ignored, base price used).
  The gateway prices by blended profile, not per-request token counts;
  exact bucketed pricing needs a length estimate on the hot path, which
  violates the simple/fast goal. (B) bucketed pricing: rejected unless
  the user explicitly wants it.
- **Accept** (A): doc line + link audit. **Files**: `docs/pricing.md`. **Size: S.**
  Decided C (2026-11-04, Q13): hardcap instead of a doc limitation —
  sync indexes each endpoint's first override tier
  (`price_override_tokens`, min `min_prompt_tokens`, established
  ALTER-column pattern, no version bump); the gateway estimates
  input tokens (chars/4 over message text, documented heuristic) and
  skips over-tier offerings across all 7 pick sites incl. sticky
  re-pin, failing closed with `context_exceeded` (400) when nothing
  fits. No truncation ever. Proven: `extract_override_tokens` +
  `override_threshold_indexed_from_payload` + migration assert (catalog);
  `context_estimate_and_cap` (unit) +
  `over_threshold_request_skips_capped_offering` +
  `all_capped_request_fails_closed` (gateway); `docs/pricing.md` +
  `docs/api.md` match the code.

---

## F6 — Security hardening

Order inside F6: S01 first (only HIGH), rest are independent S-sized items.
Anything needing a product choice waits on its Q.

### S01 — Unauthenticated operator-funded classifier spend [HIGH, exploitable-conditional]

- **Problem**: with `CLASSIFIER_API_KEY` set, any caller (even garbage
  auth) burns operator funds on the classifier leg; only global
  concurrency bounds it.
- **Evidence**: `crates/gateway/src/lib.rs:1640-1649` (no caller-auth
  check), key selection `lib.rs:1803-1844`.
- **Decide first**: Q08. Recommended (A): when the operator key would be
  used, require the request to carry a non-empty `Authorization` header,
  else 401 (OpenAI-shaped) before the leg. This does not validate the key
  (upstream still does); it stops anonymous burn with one `if`.
  (B) metrics-only alerting. (C) per-key rate limit — rejected, new state.
- **Accept**: test: smart request without auth + operator key → 401, zero
  classifier calls; with auth → classified. **Files**:
  `crates/gateway/src/lib.rs`, `crates/gateway/tests/smart_classifier.rs`. **Size: S.**
  Decided custom (2026-11-04, Q08): BYOK-only — `CLASSIFIER_API_KEY`
  removed entirely, no stored key ever. Nothing operator-funded left
  to gate, so no 401: the leg uses the caller key
  (`classifier_leg_forwards_caller_key`, env key proven ignored) with
  an `x-pennyrouter-classifier-key` per-request override
  (`classifier_key_header_overrides_caller_key`), plus a
  `classifier_auth` unit test. Upstream still validates keys.

### S02 — No gateway authentication; binds 0.0.0.0 [MEDIUM, exploitable-conditional]

- **Problem**: any network caller can proxy with own BYOK, invoke
  smart/preview, read `/metrics` and landing config. Routing is by design;
  the abuse/cost/DoS surface is ungated. (Gateway twin of old S-1;
  catalog-ui already binds 127.0.0.1 with a warning.)
- **Evidence**: `crates/gateway/src/main.rs:9`;
  `crates/gateway/src/lib.rs:509-549`; mitigations
  `terraform/hetzner/main.tf:61-67` (TLS + CIDRs) vs compose-only risk
  `compose.prod.yaml:14-21`.
- **Decide first**: Q09. Recommended (A): keep public BYOK, state it
  loudly in `docs/operations.md` (firewall/TLS required beyond LAN).
  (B) optional `GATEWAY_AUTH_TOKEN`: if set, all routes require it —
  one env check in the existing middleware; fail closed when set.
  Pick A alone unless multi-tenant exposure is planned.
- **Accept**: (A) docs line; (B) tests for allow/deny per route group.
- **Files**: docs (A) or `crates/gateway/src/lib.rs` + tests (B). **Size: S.**
  Decided A (2026-11-04, Q09): public BYOK kept, stated loudly in
  `docs/operations.md` (Debug UIs firewall line + Public surface
  note). No token gate.

### S03 — Management-write deny is a raw-path denylist [MEDIUM, hardening]

- **Problem**: percent-encoded/double-slash variants bypass the 5-pattern
  deny (upstream normalization decides); non-budget workspace writes proxy.
- **Evidence**: `crates/gateway/src/lib.rs:2193-2216`; plain forms tested
  `crates/gateway/tests/openrouter_native.rs:345-409`.
- **Decide first**: Q10. Recommended (A): canonicalize once before the
  check (percent-decode, collapse `//`, strip trailing `/`) — ~15 lines,
  same denylist. (B) allowlist of read shapes: bigger, risks breaking
  native SDK reads. (C) accept: upstream enforces account perms anyway.
- **Accept**: tests: `%2e%2e`, `//`, encoded variants → 403 like plain forms.
- **Files**: `crates/gateway/src/lib.rs`. **Size: S.**
  Decided B (2026-11-04, Q10): denylist replaced by `is_proxy_allowed`
  — reads pass, POST only allowlisted inference shapes
  (embeddings/completions/images/audio/moderations, either spelling),
  everything else 403. Canonicalize-once folded in
  (`canonical_path`: decode, collapse `//`, strip trailing `/`).
  Proven: `management_write_bypass_variants_denied`,
  `unknown_write_denied_by_default`,
  `allowlisted_inference_writes_proxy` + `proxy_allowlist_predicate`
  unit test; all prior deny/read/flag tests unchanged green.

### S04 — Privacy decisions ids get no zdr enforcement [MEDIUM → product P06]

Same root as P06; decided by Q06, fixed (or documented) there. Security
acceptance either way: a test or doc line proving `/v1/decisions` privacy
behaviour matches `docs/api.md:72-78`. No duplicate work — track under P06.
Decided with P06 (2026-11-04, Q06-B): `privacy_decision_fails_closed`
is the security regression test (400 + zero upstream hits).

### S05 — Caller key + history forwarded to foreign classifier base [MEDIUM, conditional]

- **Problem**: without `CLASSIFIER_API_KEY`, a foreign
  `CLASSIFIER_BASE_URL` receives the caller's `Authorization` plus ≤32k
  chars of history. Pinned modal/baseten forwarding is by model-choice
  consent (tested) and stays.
- **Evidence**: `crates/gateway/src/lib.rs:1634-1657`; warning already in
  `.env.example:83-91`; pinned behaviour
  `crates/gateway/tests/providers.rs:93-121`.
- **Fix**: no code change by default (documented operator choice). Add one
  startup `eprintln!` warning when a non-OpenRouter classifier base is
  configured without `CLASSIFIER_API_KEY`, mirroring the env comment.
  Do not strip the key (BYOK legs need it).
- **Accept**: unit test on the warning predicate; docs unchanged otherwise.
- **Files**: `crates/gateway/src/lib.rs`. **Size: S.**
  Decided away (2026-11-04, Q08): no operator key exists to be
  missing, so the warning, its predicate, and its test were removed;
  the caller key flows by model-choice consent (documented in
  `.env.example`, `docs/operations.md`, `docs/routing.md`).

### S06 — SSE permit drop [MEDIUM availability] → fixed as C01

No separate task. Decided by Q03, fixed by C01; F6 acceptance: C01's SSE
concurrency test is the security regression test. Close S06 when C01 lands.

### S07 — Classifier buffer bound [LOW-MEDIUM] → fixed as C02

No separate task. F6 acceptance: C02's oversized-reply test doubles as the
security regression test. Close S07 when C02 lands.

### S08 — Unauthenticated UI/metrics/landing disclosure [LOW-MEDIUM, conditional]

- **Problem**: with `GATEWAY_UI=1`, catalog/pricing/classifier config and
  metrics are unauthenticated; `/metrics` and `/` always are.
- **Evidence**: `crates/gateway/src/lib.rs:539-545`, `lib.rs:620-652`,
  `lib.rs:1035-1043`, `lib.rs:555-598`.
- **Fix**: decided by Q09. (A) docs-only: operations page already says
  localhost-behind-firewall for catalog-ui; add the same line for the
  gateway UI + metrics. (B) covered by the Q09-B token gate. No separate
  auth system for the UI (over-engineering).
- **Accept**: docs line (A) or gate test (B). **Files**: `docs/operations.md` (A). **Size: S.**
  Decided A (2026-11-04, Q09): covered by the same Debug UIs
  firewall line (gateway `/ui` + `/metrics` behind firewall, like
  catalog-ui). No UI auth system.

### S09 — Base URLs lack scheme/host validation [LOW, hardening]

- **Problem**: `OPENROUTER/MODAL/BASETEN/CLASSIFIER` bases accept
  `http://` and internal hosts. Operator-config only; no per-request SSRF
  (verified: hosts fixed, redirects never followed with credentials).
- **Evidence**: `crates/gateway/src/lib.rs:275-282`, `lib.rs:375-413`,
  `lib.rs:2461`.
- **Fix**: at startup, warn (not reject) when a base is plain `http://`
  to a non-loopback host. Rejection would break local-dev mocks; a
  warning keeps dev simple and prod honest. No URL parser dependency:
  `starts_with` checks suffice.
- **Accept**: unit test on the warn predicate (http+LAN → warn; https or
  127.0.0.1 → silent). **Files**: `crates/gateway/src/lib.rs`. **Size: S.**

### S10 — Non-hop-by-hop headers forwarded to serving upstream [LOW, hardening]

- **Problem**: `Cookie`, `X-Api-Key`, `X-Forwarded-For`, … ride to
  whichever upstream serves, including pinned foreign hosts. Hop-by-hop,
  Connection-nominated, and `Proxy-Authorization` stripping are correct.
- **Evidence**: `crates/gateway/src/lib.rs:2357-2374`, `lib.rs:2472-2478`;
  correct parts `lib.rs:2323-2352`,
  `crates/gateway/tests/hardening.rs:114-152`.
- **Fix**: document, do not strip: OpenAI-compatible clients legitimately
  send custom headers; blanket-stripping breaks them. Add one line to
  `docs/api.md` (non-hop-by-hop headers forward verbatim; don't put
  unrelated secrets in headers). No code change.
- **Accept**: doc line + link audit. **Files**: `docs/api.md`. **Size: S.**

### S11 — 64-bit non-cryptographic session hash; preview echoes it [LOW, hardening]

- **Problem**: pins keyed by seeded `DefaultHasher`; `/ui/preview` returns
  the hash for any supplied key. Impact is pin-sharing on collision only.
- **Evidence**: `crates/gateway/src/routing.rs:139-151`, `routing.rs:689-695`;
  echo `crates/gateway/src/lib.rs:667-671`.
- **Fix**: do nothing in code (high-entropy BYOK keys make collisions
  impractical; a KDF would slow every request for no real gain). Record
  the accepted risk in `docs/routing.md` session paragraph so it is a
  decision, not an accident. Revisit only if keys become low-entropy.
- **Accept**: doc line. **Files**: `docs/routing.md`. **Size: S.**

---

## F7 — Product and docs alignment (docs-only unless noted)

One docs PR. Every line below is a wording fix; the only code-adjacent
items are P08 (Q07) and the ledger updates. Link audit must pass
(`docs/README.md` stays the index).

### P01 — Vision still says "no request classifier" [High]

- **Fix**: `docs/vision.md:35` → classifier exists as the opt-in smart leg
  (Iter 26); non-goal becomes "no classifier on non-smart ids".
- **Accept**: grep `vision.md` clean; link audit. **Size: S.**

### P02 — `future.md` deferred list contradicts shipped scope [High]

- **Fix**: move judge (offline, Iter 15) and tier-classifier (Iter 26) to a
  "shipped since" note in `docs/future.md:11-15`, `future.md:24-32`; keep
  parallel fan-out + realtime judge deferred (verified absent).
- **Accept**: `future.md` lists only truly deferred items as deferred. **Size: S.**

### P03 — Pricing/routing headers say "no classifier" [Medium]

- **Fix**: `docs/pricing.md:5,8,10` and `docs/routing.md:3` → classifier
  picks tiers, never dollars (link the Smart classifier section,
  `docs/routing.md:230-272`). One sentence each.
- **Accept**: no "until a classifier exists" left. **Size: S.**

### P04 — "Continuously re-ranked / real latency, throughput, availability" overstates freshness [High]

- **Problem**: ranking uses cost/tier/score + file-based trends only; no
  throughput/uptime signals; hourly sync + 60s reload, not continuous.
- **Evidence**: `README.md:15-17`, `docs/vision.md:18-20` vs
  `crates/catalog/src/rank.rs:28-36`, `rank.rs:480,506`,
  `docs/ranking-index.md:67`, `docker/catalog-entrypoint.sh:9,28`.
- **Fix**: reword to what ships: "periodically re-ranked (hourly sync,
  60s reload) on price, judged quality, and recorded error/latency
  trends". Delete throughput/availability claims. If Q14-A later makes
  prod routing real, this wording becomes true as written.
- **Accept**: claims match `ranking-index.md`. **Size: S.**

### P05 — "Every token at its best price" is a blended approximation [Medium]

- **Fix**: keep the tagline; add one honest sentence in
  `docs/pricing.md:35-39` (balanced 50/40/5/5 profile; exact per-request
  pricing deferred) and link it from `README.md:9` context if needed.
- **Accept**: no doc promises per-token optimum. **Size: S.**

### P06 — Privacy is caller-overridable field injection [High → also S04]

- **Problem**: `privacy/` guarantees only "fields set unless caller set
  them"; decisions route enforces nothing.
- **Evidence**: `docs/api.md:72-78` vs
  `crates/gateway/src/routing.rs:2014-2022`, test `routing.rs:4700-4708`,
  decisions gap `routing.rs:2029-2042`.
- **Decide first**: Q06. Recommended (A/docs): state exactly what ships —
  chat-only, caller-wins, no provider-side filtering yet (matches
  `docs/routing.md:124-126`). (B/code): enforce on decisions too —
  only after verifying the decisions upstream tolerates `provider`
  fields (Iter 27 deliberately avoided unknown fields; do not guess).
- **Accept**: (A) `api.md` + `routing.md` wording identical to code;
  (B) integration test with privacy decisions id. **Size: S.**
  Decided B-variant (2026-11-04, Q06): no field injection (still
  unverified upstream) — instead `POST /v1/decisions` fails
  `privacy/*` closed (`privacy_unsupported_on_decisions`, 400) before
  routing, so the payload never leaves the gateway on an unkeepable
  promise. Pinning a model directly stays the explicit escape hatch.
  Proven by `privacy_decision_fails_closed` (both scopes, zero
  upstream hits); `docs/api.md` + `docs/routing.md` match the code.

### P07 — Speed/latency/throughput/region tags silently no-op [Medium]

- **Fix**: `docs/routing.md:26-27` vocabulary → mark those tags
  "parsed, currently ignored" inline (as `routing.md:63-67` already
  admits) so the first mention stops advertising them. No code change;
  implementing latency tags is explicitly out of scope (no live signal).
- **Accept**: first-mention wording matches §Status. **Size: S.**

### P08 — Virtual menu advertises tiers that 503 on empty catalog [High]

- **Problem**: `/v1/models` always 200s the 14-id menu; `/health` always
  ok; no readiness signal, so clients discover the failure at chat time.
- **Evidence**: `crates/gateway/src/lib.rs:1046-1069`, `lib.rs:144-146`,
  routes `lib.rs:509-535`; menu `crates/gateway/src/routing.rs:279-294`.
- **Decide first**: Q07. Recommended (A): add `GET /ready` → 200 only
  when a catalog snapshot is loaded (`Router::is_ready` already exists,
  Iter 23), 503 otherwise; `/health` stays liveness. ~20 lines + test.
  (B) docs-only: state the menu is static. Do not make `/health` depend
  on the catalog (breaks container checks).
- **Accept** (A): tests: no-router → 503, loaded → 200; compose
  healthchecks unchanged. **Files**: `crates/gateway/src/lib.rs`,
  `crates/gateway/tests/ui.rs` (or `health.rs`). **Size: S.**

### P09 — Menu/count and status drift [Medium → ledger in D01–D03]

Fixed by D01–D03 below (single source of truth pass). No separate work.

### P10 — p99 latency claim unproven [High, verification]

- **Problem**: "blazing-fast" claims rest on a single 2026-10-29 run with
  an unconfirmed +13.10ms p99 regression vs Iter 3.
- **Evidence**: `README.md:11`, `docs/vision.md:27` vs
  `docs/roadmap.md:264-271`, `docs/review/bug-todo-2026-10-31.md:155-164`,
  `docs/performance.md:18-19`.
- **Decide first**: Q17 (threshold only). Task: rerun
  `tests/load/compare.sh` N=5 at the 1 CPU/1 GB profile, record median
  p50/p99 in `docs/roadmap.md` + `tests/load/results/`, close the bug-todo
  P0. If p99 overhead >2ms median, open a latency iteration (suspects
  already listed: I04 parses, I07 metrics alloc) — do not fix blind.
- **Accept**: 5 result dirs + roadmap numbers updated; claim kept or
  reworded to match. **Files**: `docs/roadmap.md`, `docs/performance.md`. **Size: M** (runs, not code).
  Decided/confirmed (2026-11-04, Q17): 5/5 runs green, medians above,
  bug-todo P0 closed, no latency iteration (bar unchallenged). "Blazing-fast"
  claims stand on these numbers.

### P11 — "1M tokens" is a byte heuristic [Medium]

- **Fix**: `docs/vision.md:29`, `docs/api.md:119-120`, `README.md:32` →
  "requests up to ~1M tokens (bytes/4 estimate, 4 MiB cap)". Code comment
  already honest (`crates/gateway/src/lib.rs:33-39`). No tokenizer
  dependency (over-engineering for a guard).
- **Accept**: wording matches the guard. **Size: S.**

### D01 — TODO/roadmap: Iters 19–20 marked open, code shipped [MEDIUM]

- **Fix**: check the boxes (`TODO.md:129-138`), change roadmap headers
  "(in progress)" → "(done)" (`docs/roadmap.md:424,440`), keep the
  Shipped/Proven text. Verify against `git log` (Iter19/Iter20 commits)
  before ticking — ledger follows git, not memory.
- **Accept**: TODO/roadmap agree with `git log --oneline`. **Size: S.**

### D02 — Roadmap: Iters 21–24 "(in progress)" vs TODO [x] [MEDIUM]

- **Fix**: same pass as D01 for `docs/roadmap.md:452,467,482,496` vs
  `TODO.md:139-160`. Rule going forward (add one line to `TODO.md`
  header): TODO checkboxes are the status source; roadmap headers mirror
  them in the same commit.
- **Accept**: no "(in progress)" header over a "Shipped:" body. **Size: S.**

### D03 — Six vs fourteen virtual models [LOW]

- **Fix**: replace stale "six" at `docs/api.md:44`,
  `docs/roadmap.md:275,293,527` and the `models_list` comment
  (`crates/gateway/src/lib.rs:1046-1048`) with "fourteen curated ids +
  pinned-model/fallback forms". Historical Iter 13 text keeps "six"
  only inside its dated Landed note — mark it "(six at the time)".
- **Accept**: grep `six` shows only the dated note. **Size: S.**

### D04 — Fallback tier table omits `decision` [MEDIUM]

- **Fix**: `docs/routing.md:31-36,80-88` + `docs/api.md:60-62` → fallback
  targets are `low|think|lowest|decision` (code
  `crates/gateway/src/routing.rs:1002-1033`); note unknown targets fail
  closed with `no_eligible_offering` at request time (parse accepts any
  non-empty string, `routing.rs:251-254` — documented, not changed).
- **Accept**: table == `decide_tier` arms. **Size: S.**

### D05 — Preview plain-request caveat hidden [MEDIUM]

- **Problem**: `/ui` preview assumes no tools/budget/only-ignore; a real
  capped request can fail closed while preview shows a pick.
- **Evidence**: `crates/gateway/src/routing.rs:2197-2201`, `routing.rs:2241`;
  docs `docs/routing.md:211-228`, `docs/operations.md:169-180`; tests only
  plain (`crates/gateway/tests/ui.rs:381-618`).
- **Fix**: one sentence in both doc spots + the inspector `<p>` in
  `ui_page` ("plain-request view: tools/budget/only-ignore not applied").
  Adding constraint inputs to the preview is explicitly out of scope.
- **Accept**: wording present; `ui_page_contains_inspector` test updated.
- **Files**: `docs/routing.md`, `docs/operations.md`,
  `crates/gateway/src/lib.rs`, `crates/gateway/tests/ui.rs`. **Size: S.**

### D06 — Operations Configuration omits implemented env [HIGH]

- **Fix**: add `CLASSIFIER_*` (7 vars), `CATALOG_IMAGE`, `SYNC_INTERVAL`,
  `FETCH_OPENROUTER`, `EXTRA_MODEL_IDS`, `RELOAD_INTERVAL_SECS` to
  `docs/operations.md:73-96`, values/defaults copied from
  `.env.example:45-98` and `ClassifierConfig::from_env`
  (`crates/gateway/src/lib.rs:193-239`). `.env.example` stays canonical;
  operations links it instead of duplicating defaults that drift.
- **Accept**: every env read in `lib.rs`/`main.rs`/entrypoint appears in
  operations or `.env.example` (grep checklist in F9/U06). **Size: S.**

### D07 — Compose drops env; prod gateway is passthrough [HIGH]

- **Problem**: no compose file forwards `CLASSIFIER_*`, `UPSTREAM_MAX_*`,
  `RELOAD_INTERVAL_SECS`, `ALLOW_MANAGEMENT_PROXY`; prod omits
  `CATALOG_DB`, so prod routing is off while operations claims it.
- **Evidence**: compose files (grep empty) vs `.env.example:45-49,89-95`;
  honored at `crates/gateway/src/lib.rs:379-386,416,447-455`;
  admission `docs/roadmap.md:204-205`.
- **Decide first**: Q14. Recommended (A): forward the missing vars in all
  three compose files; prod gets `CATALOG_DB=/catalog` + the shared
  volume (mirror `compose.dev.yaml`), and `operations.md:85-88` becomes
  true. (B) Terraform-only routing: mark `compose.prod.yaml`
  passthrough-only in docs and delete the routing claim. Do not leave
  both stories.
- **Accept**: `make prod-config` renders; `docker compose config` shows
  the vars; a compose smoke with `CLASSIFIER_MODEL` set reaches the
  classifier panel. **Files**: `compose.yaml`, `compose.prod.yaml`,
  `compose.dev.yaml`, `docs/operations.md`, `docs/roadmap.md`. **Size: M.**
  Decided A (2026-11-04, Q14): all three files forward the full
  gateway env with code defaults; prod mounts `CATALOG_DB=/catalog`
  on the shared volume (separate sync ships artifacts there; empty =
  standby). Verified: `make prod-config` OK, all three render,
  `CLASSIFIER_MODEL=test-model` flows through `config`; the gateway
  honoring it is covered by `smart_classifier` 21/21 (no live docker
  boot in this environment).

### D08 — Entrypoint env undocumented (reverse gap) [MEDIUM]

- **Fix**: document `MODELS/ENDPOINTS_DIR/OUT_DIR/TRENDS/ASSIST/PROMOS`
  in `.env.example` (comment block by the sync vars) after
  `docker/catalog-entrypoint.sh:5-22`; JUDGE vars already consistent
  (verified) — leave them.
- **Accept**: every entrypoint read has an `.env.example` line. **Size: S.**

### D09 — `future.md` judge section stale [LOW] → fixed by P02

Same edit as P02 (one `future.md` pass). No duplicate work.

### D10 — Review pipeline compact-vs-full contradiction [process]

- **Problem**: the synthesis input marked all scopes incomplete/empty
  while full handoffs were complete — a consumer of summaries alone
  would under-report.
- **Fix**: process only: future multi-agent reviews must attach the full
  per-scope files (or refs) to synthesis, and the reviewer must diff
  counts (tasks here were built from full text). Record this rule in
  `docs/review/bug-todo-…` style header of the next review; no code.
- **Accept**: next review's method note states full-text synthesis. **Size: S.**

---

## F8 — Resilience and improvement hardening

Do only after F1 lands (shared `lib.rs` regions). Anything marked
"measure first" needs its numbers before code is written.

### I01 — Memory envelope can exceed 1 GB at defaults [HIGH, inference]

- **Problem**: 64 concurrent virtual requests × (4 MiB request + retry/
  fallback clones + 16 MiB response) can pass the ~1 GB budget.
- **Evidence**: `crates/gateway/src/lib.rs:37-39`, `lib.rs:83-86`,
  `lib.rs:47-48`, clones `lib.rs:1371`, `lib.rs:1445`, parses
  `crates/gateway/src/routing.rs:1937-1940`; budget
  `docs/performance.md:5-12`.
- **Decide first**: Q15. Task order: (1) load-harness run at defaults
  with concurrent virtual buffered traffic, record peak RSS;
  (2) then pick the smallest lever that fits: lower buffered cap for
  routed retries, or lower default concurrency — numbers decide.
  Do not pre-tune constants without the run.
- **Accept**: recorded RSS at defaults + chosen lever implemented with
  a saturation test. **Files**: `tests/load/`, then `lib.rs`. **Size: M.**
  Decided by numbers (2026-11-04, Q15): peak RSS 15.0–15.5 MiB flat
  across the N=5 runs (1 GB profile) + 11 MiB under virtual routed
  load — no saturation, no lever pulled, no code change. Harness now
  samples `stats.jsonl` per run (`compare.sh`). Side finding:
  fixture catalogs serve no virtual tier unjudged
  (`quality_floor_unmet` — no tiers without judgments); the virtual
  run used recorded judgments + one load-only Tencent tier (in the
  results dir, not committed to fixtures).

### I02 — Queue wait has no deadline [MEDIUM]

- **Problem**: queued requests wait indefinitely for an upstream slot;
  retry/fallback legs re-enter the same unbounded wait.
- **Evidence**: no queue field `crates/gateway/src/lib.rs:60-80`; wait
  `lib.rs:2297-2313`; re-entry `lib.rs:1453-1459`, `lib.rs:1498-1504`.
- **Decide first**: Q04. Recommended (A): overall queue deadline 30s
  (reuse the first-chunk budget constant, no new env var), then shed
  503 + `Retry-After: 1` via the existing load-shed path.
- **Accept**: test: saturated mock → queued request 503s in ~30s
  (test with a shortened injected timeout, not a real 30s sleep).
- **Files**: `crates/gateway/src/lib.rs`,
  `crates/gateway/tests/resilience.rs`. **Size: S.**
  Decided A (2026-11-04, Q04): the queued `acquire_owned` waits at
  most `timeouts.first_chunk` (30s default, no new knob), then sheds
  through the existing load-shed path (all 7 `None` arms already do).
  Retry/fallback legs share the deadline on re-entry. Proven by
  `queued_request_sheds_after_deadline` (hung pre-fix, 503+Retry-After
  in ~300ms with an injected 300ms budget).

### I03 — Reload blocks a worker; snapshot+ranking not jointly atomic [MEDIUM]

- **Problem**: synchronous catalog load on a Tokio task can stall a
  1-CPU runtime; requests can see new snapshot + old ranking.
- **Evidence**: `crates/gateway/src/lib.rs:460-479`;
  `crates/gateway/src/routing.rs:785-792`; promise `lib.rs:457-459`
  vs best-effort note `routing.rs:781-784`. Last-good retention itself
  is verified OK (U09 — different property, keep it).
- **Decide first**: Q05. Recommended (A): `spawn_blocking` for the load +
  swap one `Arc<(Snapshot, Option<Ranking>)>` so both change together —
  only if the diff stays small (Router internals already RwLock both).
  (B) keep two swaps, fix `lib.rs` wording to "snapshot and ranking
  refresh independently, last-good each".
- **Accept**: reload test proves no mixed pair is observable (A) or
  docs match (B); corrupt-artifact last-good test stays green.
- **Files**: `crates/gateway/src/routing.rs`, `crates/gateway/src/lib.rs`. **Size: M.**
  Decided A-conditional (2026-11-04, Q05): the joint-`Arc` swap
  failed the smallness test (14 lock-reader sites + ctors + tests ≈
  100 lines), so instead: (1) the reload task runs `Router::reload`
  via `spawn_blocking` — blocking SQLite/file I/O never stalls the
  1-CPU async worker; (2) `reload` loads snapshot + ranking before
  either swap — no I/O between swaps, and a failed reload swaps
  nothing at all (previously a stale ranking still swapped the fresh
  snapshot). The residual is a lock-handoff instant, documented in
  `docs/operations.md` as before. Proven by the strengthened
  `stale_ranking_is_rejected_and_keeps_last_good` (fails on the old
  order via stash, passes now) with the reload suite 7/7 green.

### I04 — Hot-path repeated full-body parses/clones [MEDIUM]

- **Problem**: up to 4 JSON parses + eager clones per routed request,
  against the parse-once performance contract.
- **Evidence**: `crates/gateway/src/lib.rs:1342,1351` (`raw_model` ×2,
  `lib.rs:2382-2388`), `lib.rs:2053-2059`, `routing.rs:1937-1940`,
  eager clone `lib.rs:1436-1445`; contract `docs/performance.md:20-21`.
- **Fix**: parse once in `routed_completion`, pass `model`/`is_virtual`
  facts into `route_inner` (which already parses for constraints —
  reuse that `Value` instead of re-parsing). Clone `retry_body` lazily,
  only after the first response proves retryable. No new types beyond a
  small param bundle.
- **Accept**: behaviour tests byte-identical (passthrough/streaming/
  virtual suites green); load compare shows no p50 regression (P10 run).
- **Files**: `crates/gateway/src/lib.rs`, `crates/gateway/src/routing.rs`. **Size: M.**

### I05 — Lock-held catalog scans can starve reload writes [MEDIUM, inference]

- **Problem**: whole-catalog `Vec`s built under the snapshot read lock;
  `store_pin` write taken while the guard lives.
- **Evidence**: `crates/gateway/src/routing.rs:1417-1466`,
  `routing.rs:1822`, `routing.rs:1049-1066`, `routing.rs:1507-1515`;
  writer `routing.rs:796-798`. No deadlock verified.
- **Decide first**: Q16. Recommended: defer. Trigger to revisit: P10/I01
  load run or a reload-latency metric shows stall. If triggered, the fix
  is Arc snapshot handles dropped before session/throttle locks (pairs
  with I03-A). Do not refactor speculatively.
- **Accept**: deferred note recorded here; or, if triggered, same routing
  tests + reload-under-load test. **Size: M if done.**
  Decided defer (2026-11-04, Q16): 2026-11-04 numbers show no stall
  (p99 ≤0.52ms, RSS flat) — trigger stands: a future P10/I01 run or
  reload-latency metric showing stall reopens this.

### I06 — Throttle map can grow past its cap [LOW]

- **Problem**: `MAX_THROTTLES=4096` is not enforced when nothing expired.
- **Evidence**: `crates/gateway/src/routing.rs:423-425` vs
  `routing.rs:658-665`; sessions do it correctly (`routing.rs:721-730`).
  "Bounded by catalog pairs" is unproven (U08).
- **Fix**: mirror `store_pin`: reap expired first; if still full, skip
  inserting (routing continues, just unthrottled-new-pair). Six lines.
- **Accept**: unit test: fill with live entries → len never exceeds cap.
- **Files**: `crates/gateway/src/routing.rs`. **Size: S.**

### I07 — Metrics/logging per-request allocation [LOW]

- **Problem**: per-request `HashMap` lock + 14-bucket loop + JSON log
  `String` built before the drop check.
- **Evidence**: `crates/gateway/src/metrics.rs:102-132`;
  `crates/gateway/src/lib.rs:1093-1103`; channel safety
  `lib.rs:92-105` (keep).
- **Fix**: only if P10 numbers point here: check channel fullness before
  formatting the log line (early `try_send` probe), leave metrics as-is
  unless profiled. No lock-free rewrite without evidence.
- **Accept**: load compare (P10) before/after; log-drop counter still exact.
- **Files**: `crates/gateway/src/lib.rs`. **Size: S.**

### I08 — Publish rename not crash-durable; temp leaks [LOW]

- **Problem**: no parent-dir fsync after rename; failed writes leak
  `.tmp-<pid>-<nanos>` files.
- **Evidence**: `crates/catalog/src/artifact.rs:41-62`, tmp naming
  `artifact.rs:48-54`.
- **Fix**: `sync_all` the parent dir after rename (one call); remove the
  temp file on any error path (`Drop` guard or explicit remove — pick
  explicit remove, simpler). Collision risk disappears with the guard's
  cleanup; do not rename the scheme.
- **Accept**: test: forced rename failure leaves no `.tmp-*` behind.
- **Files**: `crates/catalog/src/artifact.rs`. **Size: S.**

### I09 — Ranking version not content-idempotent [LOW]

- **Problem**: every periodic rebuild mints a new versioned file pair
  even when nothing changed (version hashes `created_at`).
- **Evidence**: `crates/catalog/src/rank.rs:465-466`, claim
  `rank.rs:540-549`, caller `crates/catalog/src/bin/catalog-rank.rs:88-103`.
- **Fix**: hash the document with `created_at` zeroed for the version
  (keep the field in the stored doc). Rebuilds of identical inputs then
  produce the identical version and can skip publishing (existing
  newest-manifest discovery already tolerates it).
- **Accept**: test: two rebuilds, same inputs → same version; changed
  input → different version. **Files**: `crates/catalog/src/rank.rs`. **Size: S.**

### I10 — Behaviours with no test (a)–(e) [MEDIUM, coverage]

Fold into their parent tasks; this entry is the checklist so none is lost:

- (a) `env_flag` true/yes variants (`lib.rs:499-507`) → add cases to the
  `lock_env` UI tests.
- (b) `ClassifierConfig::from_env` (`lib.rs:208-239`) → B01's new tests.
- (c) Compose env forwarding → D07 acceptance (`docker compose config`).
- (d) `free/fallback/{think,lowest,decision}` integration → extend B04's
  fallback tests beyond `low` (`crates/gateway/tests/virtual_models.rs:671`
  covers `low` only today).
- (e) Preview under tools/budget → closed by D05's documented caveat
  (explicitly not adding constraint inputs).
- **Accept**: each letter closed by its parent task's test/docs. **Size: S total.**

---

## F9 — Evidence closure (U01–U10)

These tasks produce verdicts, not fixes. Each ends in one of: confirmed
(fix task already exists above), refuted (close with evidence), or
needs-decision (promote to a Q). Run U03 first — F1/F4a depend on it.

### U03 — Reproduce the dynamic claims before fixing them

- **Task**: throwaway probes (kept as tests where cheap): oversized
  classifier reply (C02), 200k-message truncation timing (C06),
  slash-heavy model timing (C07), `Duration::ZERO` reload panic (C08),
  poisoned re-sync data loss (C05), 64× virtual RSS (I01).
- **Accept**: each probe's before-number recorded in its fix task;
  passing probes become the regression tests named there.
- **Files**: `crates/gateway/tests/…`, `crates/catalog/tests/…` (probes
  that pass post-fix stay; pure timing probes may live in `tests/load/`).
  **Size: M.**

### U01 — No severity rubric across scopes → Q18

Docs-only; decided in Q18, applied from the next review onward. Also
adopt: one finding = one root cause; duplicates merge with severities
preserved, never averaged.

### U02 — Bug-vs-intent calls → Q02 (+ Q01, Q06)

`free_until` last-wins, unknown-error-as-perfect, caller `zdr:false`,
empty-pinned passthrough. All four are decided by Q02/Q01/Q06 votes;
until then they stay open by design. No code in this task.

### U04 — Catalog judge providers never reviewed (877 lines)

- **Task**: read-only review of `crates/catalog/src/judge_baseten.rs`,
  `judge_modal.rs`, `judge_openai.rs`, `judge_catalog.rs` (correctness,
  spend caps, auth handling, sampling determinism). Output: findings
  appended as a dated addendum under `docs/review/` (do not edit the
  2026-11-03 review), each new bug becoming a B-task here.
- **Accept**: addendum exists; zero-findings is an acceptable outcome if
  stated with scope. **Size: M.**

### U05 — Partial reads: `metrics.rs`, full `routing.rs`

- **Task**: read `crates/gateway/src/metrics.rs` (622 lines) fully and
  re-verify `routing.rs` hot sections not covered (preview, pinned,
  decisions paths) against the same checklists as F3. File addendum
  findings like U04.
- **Accept**: addendum exists (or explicit "no new findings"). **Size: M.**

### U06 — Never-opened material

- **Task**: one checklist pass: `docs/architecture.md`, `platform.md`,
  `cloud-providers.md`, `catalog-sync-*.md`, `docs/proposals/` (4),
  root `tests/` (integration/live/load), `compose.perf.yaml`, remaining
  `terraform/`. For each: consistent-with-code yes/no + line. Contradictions
  become D-tasks; the env-var grep from D06 is part of this pass.
- **Accept**: checklist recorded (here or addendum), new D-tasks filed. **Size: M.**

### U07 — Uncited claims in the review itself

- **Task**: verify or retract, one line each: (a) "tests use a bare-host
  mock base" (B01 masking claim) — name the test file/line or soften B01;
  (b) "no packages.json exists" — record the `find` command used;
  (c) tiered-pricing fixture `:1` (B10) — cite the exact JSON key path;
  (d) Appendix test counts — spot-check three files against `cargo test
  --list`. Corrections go in the addendum (U04), never by editing the review.
- **Accept**: four verdict lines. **Size: S.**

### U08 — Unproven negatives

- **Task**: (a) deadlock: lock-order note for snapshot→sessions→throttled
  in `docs/architecture.md` (one paragraph, from I05's evidence) — proof
  by inspection, no soak test; (b) throttle bound: closed by I06's cap
  test; (c) "no load results beyond 2026-10-29": `ls tests/load/results`
  output recorded in P10's task (U07-style citation).
- **Accept**: three verdict lines. **Size: S.**

### U09 — Reload last-good vs joint atomicity "contradiction"

Not a contradiction: last-good retention (verified OK) and joint
snapshot+ranking atomicity (I03) are different properties. Closed by
I03's Q05 vote + one clarifying sentence wherever both are documented.
No separate fix.

### U10 — No test suite ran during the review

Closed by the fixing workflow itself: every fix task above names its
tests, and `cargo test --workspace` + clippy + fmt are the merge gate
(per `AGENTS.md`). The review's static [Inference] items stay labelled
until their U03 probes land.

---

## F10 — U04/U05 follow-up findings (filed 2026-11-04, fixed same day)

### B19 — Live-judge caps silently ignored without `--judges` [MEDIUM]

- **Problem**: `--sample-rate`/`--max-calls`/`--max-spend-usd` are
  parsed and validated, then ignored unless `--judges` is also
  passed — a bare `--judge-backend baseten --max-calls 10` runs a
  full uncapped live rebuild. Docs promise caps on every live run.
- **Evidence**: `crates/catalog/src/bin/catalog-process.rs:181-191`
  (`entry.is_some()` gates both `select_endpoints` and
  `plan_live_calls`); docs `docs/operations.md:156-166`.
- **Fix**: apply policy whenever live: always `select_endpoints`;
  always `plan_live_calls` with an uncosted entry when no catalog
  (max_calls enforced, spend cap fails closed on unknown cost —
  the existing `plan_live_calls` semantics).
- **Accept**: CLI probe: `--sample-rate 0` live run judges 0 with no
  network; `--max-calls 1` on 4 endpoints fails closed pre-write.
- **Files**: `crates/catalog/src/bin/catalog-process.rs`,
  `crates/catalog/tests/process_cli.rs`. **Size: S.**

### B20 — Judge sampling ignores endpoint tag [LOW]

- **Problem**: `sampled()` hashes only `(model_id, provider)`, so
  same-provider tag variants (e.g. `openai` vs `openai/flex`) always
  sample together — the "deterministic per endpoint" doc claim is
  wrong. (Hashing the DB `id` instead would break determinism
  across rebuilds: re-sync reassigns ids.)
- **Evidence**: `crates/catalog/src/judge_catalog.rs:356-374`.
- **Fix**: chain the stable `endpoint_tag` into the FNV hash.
- **Accept**: unit test: same pair, different tags, split at some
  rate in (0, 1); same input re-samples identically.
- **Files**: `crates/catalog/src/judge_catalog.rs`. **Size: S.**

### B21 — Catalog modal base_url needs `/v1`, env path adds it [LOW]

- **Problem**: `ModalJudge::from_env` appends `/v1` via `api_root`,
  but `build_judge` passes catalog `base_url` through verbatim — a
  catalog entry mirroring `MODAL_JUDGE_URL` (deployment root) 404s
  mid-rebuild. Loud failure, DB-safe (C05 transaction), still a
  footgun.
- **Evidence**: `crates/catalog/src/judge_catalog.rs:474-482` vs
  `crates/catalog/src/judge_modal.rs:44-54`.
- **Fix**: apply `api_root` in `build_judge` (idempotent for
  `/v1`-suffixed entries; make it `pub(crate)`).
- **Accept**: existing `api_root` unit test + full suite; catalog
  root-style and suffixed URLs both normalize.
- **Files**: `crates/catalog/src/judge_catalog.rs`,
  `crates/catalog/src/judge_modal.rs`. **Size: S.**

### B22 — Metrics render omits the decisions route [MEDIUM-LOW]

- **Problem**: `Route::Decisions` is observed into slot 6 but both
  `render()` loops list only the six older routes — decisions
  traffic is counted and never exported.
- **Evidence**: `crates/gateway/src/metrics.rs:409-416`,
  `metrics.rs:437-444` vs `metrics.rs:29,68`.
- **Fix**: add `Route::Decisions` to both loops.
- **Accept**: unit test: observe decisions + render contains
  `route="decisions"` request and latency series.
- **Files**: `crates/gateway/src/metrics.rs`. **Size: S.**

## Appendix — decision log (fill in as Qs are answered)

Copy this table; one row per answered Q. Keeps the next agent from
re-asking. Delete nothing; supersede by adding a row.

| Q | Task(s) | Answer (A/B/C) | Date | Commit |
|---|---|---|---|---|
| Q03 | C01, S06 | A (permit in stream state) | 2026-11-04 | |
| Q02 | B08, P12 | A (max-expiry + nulls-last) | 2026-11-04 | |
| Q11 | B17 | A (fail on 0 or >50% skipped) | 2026-11-04 | |
| Q08 | S01 | custom: BYOK-only classifier key (caller Authorization; optional `x-pennyrouter-classifier-key` header override); CLASSIFIER_API_KEY removed, no stored key ever | 2026-11-04 | |
| Q12 | B18 | A (reject over-age rankings) | 2026-11-04 | |
| Q01 | B16 | A (strict 400 model_not_provided) | 2026-11-04 | |
| Q02 | B08, P12 | | | |
| Q03 | C01, S06 | | | |
| Q04 | I02 | | | |
| Q05 | I03, U09 | | | |
| Q06 | P06, S04 | | | |
| Q07 | P08 | | | |
| Q08 | S01 | | | |
| Q09 | S02, S08 | | | |
| Q10 | S03 | | | |
| Q11 | B17 | | | |
| Q12 | B18 | | | |
| Q13 | B10 | | | |
| Q14 | D07, P04 | | | |
| Q15 | I01 | | | |
| Q16 | I05 | | | |
| Q17 | P10 | | | |
| Q18 | C08, U01 | | | |





