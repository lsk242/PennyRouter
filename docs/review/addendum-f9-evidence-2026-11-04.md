# Addendum: F9 evidence closure (2026-11-04)

Dated addendum to `PennyRouter-full-review-2026-11-03.md` (that file is
frozen; corrections land here, never there). Covers U03–U10 plus the
follow-up findings U04/U05 filed as B19–B22 (fixed same day).

## U03 — dynamic-claim probes

Each probe failed pre-fix and is now a regression test. Before-numbers
are also recorded in their fix tasks in
`../todo/fix-review-2026-11-03.md`.

| Claim | Pre-fix probe result | Post-fix | Regression test |
|---|---|---|---|
| C02 oversized classifier reply | waited out the 2 s classifier timeout | ~170 ms fail-fast via `Invalid` → fallback | `smart_classifier.rs` oversized-stream test (18/18) |
| C06 truncation quadratic | 10k messages: 77.69 s | 0.02 s | `truncate_input` linear test |
| C07 split_model quadratic | 95.39 s | 0.03 s | `split_model_stays_linear` |
| C08 zero-interval panic | panicked (proven via stash) | no-op handle, never ticks | reload suite (6/6) |
| C05 poisoned re-sync | model left endpoint-less | all 3 endpoints kept | `failed_resync_keeps_prior_endpoints` |
| I01 64× virtual RSS | — | pending Q15 load run | — |

## U04 — judge providers review

Scope: `judge_baseten.rs` (89), `judge_modal.rs` (135),
`judge_openai.rs` (140), `judge_catalog.rs` (513), plus the
`catalog-process.rs` CLI call chain (caps are enforced there, so the
chain is in scope). Checklist: correctness, spend caps, auth
handling, sampling determinism.

Findings (filed as B19–B21, fixed same day — see F10 in the todo):

- **B19 [MEDIUM]**: `--sample-rate`/`--max-calls`/`--max-spend-usd`
  were parsed, validated, then ignored without `--judges` — a bare
  `--judge-backend baseten --max-calls 10` ran a full uncapped live
  rebuild. The probe caught it calling the real Baseten API despite
  `--sample-rate 0`. Docs (`operations.md`) already promised caps on
  every live run; the fix aligns code with docs.
- **B20 [LOW]**: sampling hashed only `(model, provider)`, so
  same-provider tag variants sampled together, contradicting the
  "deterministic per endpoint" claim. Fixed by chaining the stable
  `endpoint_tag` (not the DB id — re-sync reassigns ids, which would
  break cross-rebuild determinism). Bucket values shift once for all
  rows; determinism holds going forward.
- **B21 [LOW]**: catalog modal entries needed an explicit `/v1`
  suffix while `from_env` appends it — a root-style entry 404'd
  mid-rebuild (loud, DB-safe via the C05 transaction). `build_judge`
  now normalizes via `api_root` (idempotent).

Verified OK: secrets travel env→constructor only (names in config,
never values; errors name variables, not values); no `Debug` on
judge structs; 60 s client timeouts; prompt version pinned
(`prompt_version` must equal `PROMPT_VERSION`); unknown backends
rejected at load; caps checked pre-write; FNV sampling deterministic
per key; spend line per live rebuild.

Notes (not bugs): judge replies have no size cap (offline batch,
configured upstream — unlike the request-path classifier, C02);
`chat_body` embeds the full endpoint payload, so flat
`cost_per_call_usd` estimates vary with payload size; no judge retry
(a transient failure aborts the run loudly).

## U05 — metrics + routing hot sections

Scope: `metrics.rs` (622) in full; routing preview/pinned/decisions
paths re-checked against the F3 sticky/fallback checklist.

Finding (filed as B22, fixed same day):

- **B22 [MEDIUM-LOW]**: `Route::Decisions` was observed into slot 6
  but both `render()` loops listed only the six older routes —
  decisions traffic counted, never exported. Both loops now include
  it, with a render unit test.

Verified OK: fixed label slots (no cardinality leak); cumulative
buckets with `+Inf` = count; poison-safe status mutex; classifier
index math consistent across report/summary/render; in-flight
guards symmetric; `LOG_DROPPED` exact. Preview mirrors
`skip_throttled` (throttled display-last, throttled pin loses to
fresh alternatives, never stores); `route_decision_fallback`
serves `decision` only and fails other targets closed; pinned-model
fallback parses through the same `VirtualModel.fallback` dispatch.
`/ui/preview` labels as `other` (bounded; accepted).

## U06 — never-opened material checklist

For each: consistent-with-code yes/no + line.

| Material | Verdict |
|---|---|
| `docs/architecture.md` | yes — `region` at :87 is an index-schema field list, not a routing claim |
| `docs/platform.md` | yes — Hetzner-selected, ballpark, secrets match `terraform/`, `performance.md`, `api.md` |
| `docs/cloud-providers.md` | yes — comparison doc; "six" counts providers, not models |
| `docs/catalog-sync-architecture.md` | yes — no code contradictions |
| `docs/catalog-sync-roadmap.md` | yes — Phase 4 Baseten URL/model match `judge_baseten.rs` constants |
| `docs/catalog-sync-todo.md` | yes — checklist, no live claims |
| `proposals/2026-11-01-openrouter-interface.md` | yes (historical) — "six virtual ids" describes Iter-13 state in a dated proposal |
| `proposals/2026-11-02-smart-classifier.md` | **fixed**: status was "Proposed (not yet implemented)" after Iter 26 shipped it → "Implemented (Iter 26)" |
| `proposals/2026-11-03-artificial-analysis-index.md` | yes — Proposed, no code claims |
| `proposals/2026-11-03-minindex-filter.md` | yes — Proposed, no code claims |
| `tests/integration/` | yes — `OPENROUTER_API_KEY` in local `.env`, skips without key |
| `tests/live/` | yes (historical) — "six virtual ids" is a checked-off dated observation; `router-smoke.sh` asserts one id, not a count |
| `tests/load/` | yes — harness matches `compose.perf.yaml` caps |
| `compose.perf.yaml` | yes — 1 CPU / 1 GB matches the footprint target |
| `terraform/` (non-hetzner) | yes — `options.md` evaluation stubs; `terraform/README.md` matches Hetzner-selected |
| D06 env grep | yes — every `env::var`/`env_flag`/entrypoint read appears in `operations.md` or `.env.example` (verified 2026-11-04; see D06) |

## U07 — uncited claims, verified

- (a) "Tests use a bare-host mock base" — **confirmed**:
  `crates/gateway/tests/smart_classifier.rs:262`
  (`classifier_config(&format!("http://{clf}"))`, mock from
  `spawn_mock` at :199–204, no `/v1` path). B01 stands.
- (b) "No `packages.json` exists" — **confirmed**:
  `find /home/lsk/workspace/PennyRouter -name packages.json -not
  -path '*/node_modules/*'` returns nothing.
- (c) Tiered-pricing fixture — **confirmed**: exact path
  `data.endpoints[].pricing.overrides[].min_prompt_tokens`
  (= 272000) in
  `crates/catalog/tests/fixtures/endpoints/openai__gpt-6.1-terra-pro.json`.
  B10 stands.
- (d) Appendix test counts — **verified** on three untouched files:
  `health.rs` 1, `passthrough.rs` 4, `streaming.rs` 3
  (`cargo test -p pennyrouter-gateway --test <f> -- --list`).

## U08 — unproven negatives

- (a) Deadlock — **no cycle by inspection**, recorded in
  `docs/architecture.md`: requests hold `snapshot`/`ranking` reads
  while separately taking `sessions` or `throttled` (never nested
  in each other); reload takes the write locks sequentially and
  never touches `sessions`/`throttled`. Writer starvation stays a
  latency risk (I05), not a deadlock.
- (b) Throttle bound — **closed** by I06's cap test
  (`throttle_map_never_exceeds_cap`).
- (c) "No load results beyond 2026-10-29" — **confirmed**:
  `ls tests/load/results` shows `20261027T090047Z` and
  `compare-20261029T212247Z` as the newest entries.

## U09 / U10

- U09: the clarifying sentence is in `docs/operations.md`
  (last-good retention vs joint atomicity are different
  properties); full closure waits on the Q05 vote.
- U10: closed by the fixing workflow — every fix names its tests;
  `cargo test --workspace` + clippy + fmt green (see gates below).

## Deferred with reason (not Q-gated)

- **I07**: no change. The sketched "early `try_send` probe" is not
  implementable on `std::sync::mpsc::SyncSender` (no fullness API
  on stable); changing channel types is exactly the rewrite the
  task forbids without P10 numbers. Revisit when P10 profiles it.
- **I04 load leg**: refactor behavior-verified by the full gateway
  suite; the load-compare leg rides the P10 run per the task's own
  accept parenthetical.

## Gates (2026-11-04)

`cargo test --workspace`, `cargo clippy --workspace --all-targets`
(zero warnings), `cargo fmt --check` — all green.
