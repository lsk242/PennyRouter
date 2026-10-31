# PennyRouter — Full Review (Iter 18, 2026-10-31)

**Reviewer:** automated review (Hermes) on `kittlab86/PennyRouter` fork, branch `docs/review-2026-10-31`, pinned base `lsk242/PennyRouter@main` (47529c6, Iter 18 live OpenRouter fetch). This is a *new* full review, not a re-report of the three existing `docs/review/*` studies — those are respected as prior art and referenced where their fixes have landed.

**Method:** read-only static review of `crates/gateway` + `crates/catalog` + `crates/catalog-ui`, all `docs/*.md`, `compose*.yaml`, `docker/Dockerfile*`, `terraform/hetzner`, `.github/workflows/*`, `Makefile`, `AGENTS.md`, `TODO.md`, and existing reviews. Checked `39356da` (old pin) → current diff, verified F01–F11 + A01–A05 landing in Iter 11–18. No cargo build/run, no live keys, no cloud apply.

**Verdict:** The gateway is coherent for a personal, free-first router on OpenRouter + BYOK Modal/Baseten. Iter 11 closed the High-severity provider-constraint and upload-holding bugs, and Iter 13–16 made the product promise concrete (6 virtual ids, fail-closed budgets, provider-pinned routes). Remaining risk is not a single High but a trail of Medium correctness, operability, and product-completeness gaps that matter exactly because the core routing now works.

---

## 1. Bugs and correctness issues (still open or new)

### B1 — Virtual `GET /v1/models` is authoritative but stale after `RELOAD_INTERVAL_SECS=0`
The gateway can be configured load-once (`RELOAD_INTERVAL_SECS=0`) for deterministic tests. In that mode the curated 6-model menu is correct at boot, but so is the *ranking/free tier* behind it. If `CATALOG_DB` was empty at boot (fresh prod volume), virtual chat routes fail closed (503) while `/v1/models` still advertises those ids as available. A client that caches the menu will retry the same virtual ids against a gateway that will never serve them. **Fix:** either gate `/v1/models` on catalog readiness (return 503 with Retry-After until first snapshot loads) or document and health-check the "catalog not ready" state separately from gateway health.

### B2 — `route_pinned` model-id remainder can be empty
`route_pinned` strips `modal/` or `baseten/` and forwards the remainder verbatim. An incoming `model: "modal/"` or `"baseten/"` yields `""` after strip and is forwarded as an empty model field to the provider's `/v1/chat/completions`. That upstream's error shape is then proxied. The gateway should 400 early with `model_not_provided` instead of proxying an empty model. The existing 5 `tests/providers.rs` cases cover missing prefix but not zero-length remainder.

### B3 — `providers.rs` direct routes drop caller `provider` fields inconsistently
For pinned direct routes, `route_pinned` explicitly drops the OpenRouter `provider` object (correct for foreign upstreams). But `zdr`/`data_collection` are OpenRouter-specific privacy flags, not Modal/Baseten flags — the gateway correctly does *not* inject them on direct routes now, yet it also does not reject a caller that sets them expecting privacy on those providers. Document that direct routes are privacy-unaware and consider a warning/error when `provider.zdr` accompanies a `modal/` id.

### B4 — Catalog `RELOAD_INTERVAL_SECS` of 0 still spawns a background task that immediately exits, then never retries a transient first-load failure
If the first `catalog-resolver` load fails (corrupt volume on fresh boot with `RELOAD_INTERVAL=60`), the Iter 11 retry on subsequent ticks recovers. With `RELOAD=0` (load-once), the gateway stays on the empty snapshot until next restart — identical to B1's trigger. This is intentional for determinism, but operations docs do not call out that `RELOAD=0` opts into "never recovers from first-load failure" — worth a sentence in `docs/operations.md` and the `.env.example` comment.

### B5 — `price.rs` promo expiry is wall-clock, but gateway free-check is also wall-clock, with no clock-skew contract
`free_until` in the ranking document and the gateway's `is_free(now)` both use system time, as intended. The catalog-sync and gateway can run on different hosts (shared volume vs Hetzner VM). If their clocks diverge > promo window margin (some promos are hours, not days), a freshly published artifact can be considered expired immediately. Low-severity now because promos are low-volume, but worth a clock-skew note and `valid_until_unix` delta logging at publish.

### B6 — Upstream body bound (16 MiB) is per-collection, but `streaming.rs` idle timeout is per-chunk, so a slow trickle within the bound can hold a slot for 60s × many chunks
This is by design after Iter 11 (total vs idle split), but the *stream* path has no aggregate byte bound — a provider that streams 1 GiB slowly (1 KiB chunks every 50s, under idle 60s) can hold an upstream slot and downstream SSE for a very long time, exceeding the 1 CPU/1 GB ballpark without ever hitting the non-stream 16 MiB cap. The existing `UPSTREAM_MAX_CONCURRENT=64 / QUEUED=128` mitigates fan-out, but a single streamed response can still hold one slot indefinitely. Consider an aggregate stream byte/time cap or a documented max-stream duration.

### B7 — `metrics.rs` log-writer thread holds `stdout` lock per-batch, but `health` and `ui` handlers write JSON via `axum::Json` directly to the same stdout — no conflict, but test harnesses capturing stdout see interleaved `log_line` + response bytes
In `cargo test`, `LOG_SENDER` can be initialized by one test and then emit to the shared stdout that `harness` also captures. The current `OnceLock` makes this harmless (lines go to real stdout, not the test's capture), but it explains why `cargo test` with `--nocapture` mixes log lines with test output. Not a bug, but worth a comment in `metrics.rs` so future test writers do not chase phantom output.

### B8 — `docs/api.md` Ollama table says "Proxied as-is … No Ollama↔OpenAI translation yet" — correct, but `compose.yaml` `extra_hosts: host.docker.internal:host-gateway` is gated on gateway's `OPENROUTER_BASE_URL`, not on `OLLAMA_BASE_URL` (which does not exist). An Ollama upstream on `http://host.docker.internal:11434` would require a custom `OPENROUTER_BASE_URL` override, which the variable name does not convey. Rename or alias to `UPSTREAM_BASE_URL` or document the Ollama compose tweak explicitly.

---

## 2. Improvements (should-fix before broadening scope)

**I1 — Name the gateway's two proxy personalities explicitly.** The gateway is now *three* things: (a) routed virtual chat on `openrouter/pair/*` and `/v1/models`, (b) provider-pinned direct routes `modal/<id>`/`baseten/<id>`, (c) blind fallback proxy for everything else. `docs/api.md` does this, but `docs/routing.md` still frames routing as "suffix filters on OpenRouter models" — the Iter 13 virtual catalog replaced that mental model. Update `routing.md` to open with the 6 virtual ids as the primary contract, and move suffix-filter history to an appendix.

**I2 — Document the 6-model contract as the only supported `model` values.** `README.md` and `docs/README.md` still imply arbitrary model ids are first-class routed inputs. For a free-first personal router, that creates a quality-floor bypass: a caller that sends a raw OpenRouter id bypasses tier/free logic. If that is intentional, say so ("virtual ids are the policy path; raw ids are passthrough with no budget/quality enforcement"). Today the product-viability review is correct that this distinction is easy to miss.

**I3 — Hard-iterate the load baseline process.** Iter 12's re-run showed json p50 +0.26ms identical to Iter 3 but p99 +13.10ms vs +0.40ms, single run, variance unconfirmed. `tests/load/results/compare-*` now has one Iter 12 datapoint. Add a `make load-baseline` target that runs `compare.sh` three times and reports median p50/p99, and require it in the PR template for any `crates/gateway` change. The existing `tests/load/compare.py` already parses the file — just loop it.

**I4 — Make `CATALOG_DB` readiness visible.** Add `GET /health` vs `GET /ready` distinction (common Rust pattern) or at least a `catalog_ready` field on `/health` / `/metrics`. Deployments currently health-check only `/health`, which is `{"status":"ok"}` even with an empty catalog — the Terraform `cloud-init` readiness loop already needed fixing once (F09); this would prevent B1 from looking healthy while being functionally down for virtual models.

**I5 — Tighten `Cargo.toml` crate versions.** The workspace bumped to `0.11.0` (Iter 8) and now `0.12.0+` territory (Iter 18), but `Cargo.lock` still pins old `reqwest`/`tokio` minors that carry the gzip-vs-`Content-Encoding` semantics from F03. The fix was correct (transparent passthrough, never decode), but the lockfile should be audited (`cargo audit` in CI) and the gateway's `Cargo.toml` should feature-gate `reqwest` explicitly: `default-features = false` + `rustls-tls` is already done — add a comment citing F03 so a future contributor does not re-enable `gzip`.

**I6 — Reduce `price.rs` promo matching from O(models × promos) at rebuild to a hash map.** At fixture scale (18 free offers) this is trivial; at live scale (456 models × 1483 endpoints in the Iter 18 live one-shot) the rebuild already shows `promos` linear scan per offering. Build a `HashMap<(model_id, provider_name, Option<tag>), promo>` once per sync cycle — not hot-path, but cleaner and closer to gateway-scale.

**I7 — Unify naming: `CATALOG_DB` is a directory/manifest/file tri-state, `CATALOG_IMAGE` is a Docker image, `SYNC_INTERVAL` vs `RELOAD_INTERVAL_SECS` are two different cadences.** The `.env.example` documents this, but `docs/operations.md` does not. A single "Configuration" table (var, default, units, hot-reloadable?) would prevent the common mistake of setting `CATALOG_DB=/catalog/catalog.db` vs `/catalog:/catalog:ro`.

---

## 3. Missing features (vs vision, not vs Iter 8)

These are not bugs in the shipped code, but product gaps that prevent "I turned it on and forgot about it" usage.

**M1 — No cost observability for the gateway's own judgments.** Iter 15 judges stay offline-batch (good — never on the request path), but there is no gateway-side spend signal at all: no `X-PennyRouter-Spend` header, no per-request `usage.cost_usd` addition, no local budget ledger. The vision is "every token at its best price" — a personal router should answer "what did this task cost me?" after the fact. Start with a post-routing `X-PennyRouter-Effective-Cost` header derived from the same `effective_cost` blending that chose the provider, with explicit caveats that it is an estimate, not a bill.

**M2 — No user-supplied budget policy on the request path.** The product-viability review asked for a hard spending policy (task/day/month ceilings). Iter 14 made virtual routes fail-closed on `provider.max_price`, but there is no personal `budget` field (e.g., `X-PennyRouter-Budget: max_cost_usd=0.50`) nor a local budget file. This is the single most important personal-router feature: without it, "free-first" cannot be "never escape my price band without asking."

**M3 — No tool/context eligibility gate.** Coding harnesses need function calling + large context. The current `Offering` has `tag`/`precision`/`context_length` but not "supports function calling" or "supports reasoning_content continuation." A free model that cannot do the harness's tool loop can still win on price. The catalog sync already has a `judge` dimension — a boolean `capabilities` gate (tools OK, context OK) is a small catalog addition with outsized product value.

**M4 — No per-user or per-workspace isolation beyond `Authorization + model` session key.** Two projects sharing the same OpenRouter key + model share a provider pin, cache warmth, and throttling state. The intended product is personal, but even a personal user has multiple concurrent harnesses. An explicit `X-PennyRouter-Session-Id` (optional, opaque, caller-provided) that scopes pins and throttling would be cleaner than inferring scope from the key.

**M5 — No streaming-budget or abort signal.** A 1M-token request that streams back 1M tokens is legitimate per `MAX_TOKENS` (bytes/4 estimate, 4 MiB), but a runaway provider that streams indefinitely within idle 60s can still hold resources (B6). A gateway-side max-stream-bytes or max-stream-duration (configurable, default generous) with a 500 + `stream_too_large` and a terminal SSE error would close this product loop.

**M6 — Limited backend diversity in the live catalog.** Iter 18's live one-shot (456 models, 1483 endpoints, 18 zero-price offers) is OpenRouter-only. The platform already has `MODAL_BASE_URL`/`BASETEN_BASE_URL` for direct routes, but not for catalog population. A second provider's catalog (even just Baseten's public inference list) would validate that the `effective_cost` + promo model generalizes — currently it is tested only on OpenRouter's price shape.

**M7 — No end-to-end test that exercises the real gateway binary with a real catalog snapshot.** Unit/integration tests are solid (routing/unit, reload, hardening, throttling, providers, virtual-models), but there is no `cargo test` that starts the gateway binary (not just the `AppState` router) and hits it over HTTP with a fixture DB. The existing `tests/load/mock_backend.py` + `compare.sh` do this at the load level, but not as a fast `cargo test` regression.

---

## 4. What is already good (keep)

- **F01–F11 fixes are real.** Provider field preservation with `only`/`ignore` intersection, upload-idle/total deadlines (30s/60s, 408), transparent compressed passthrough with hop-by-hop strip, 16 MiB upstream bound, SSE terminal `error` event, atomic `data-before-manifest` publish + invalid-candidate skip, Terraform `/api` already correct, readiness loop now exits nonzero, unknown-score ordering, time-boxed promos (`valid_until_unix`). Each has a regression test.
- **Virtual catalog as the stable menu.** Six ids (`openrouter/pair/{privacy/}{free,low,think}`) with local `/v1/models` and retry (virtual-only, 1×) + throttled-skip (60s) is a clear product surface.
- **Small, readable, well-tested Rust.** ~13K lines gateway + catalog, fixtures in `crates/catalog/tests/fixtures`, iteration discipline in `TODO.md`/`docs/roadmap.md` that actually matches `git log`.

---

## 5. Suggested next slice (one iteration, not a roadmap dump)

If only one more iteration is funded, make it **M2 + I4 + I1 in that order**: (1) a minimal, fail-closed personal budget header that the gateway can enforce on virtual ids; (2) a `catalog_ready` signal on health/ready so deployments do not look green while empty; (3) docs that present the 6 virtual ids as the product, with raw ids as explicitly unprotected passthrough. That closes the product-viability loop without adding a classifier, a judge, or a new provider.

## 6. Sources

- Current main `47529c6` (Iter 18) full tree, compared to `39356da` (Iter 8) security + product-viability + resource-efficiency reviews.
- `crates/gateway/src/lib.rs` (1407L), `routing.rs` (2190L), `crates/catalog/src/{price,rank,judge*,fetch,artifact,process}.rs`, `docs/*.md`, `compose*.yaml`, `terraform/hetzner`, `.github/workflows/*`.
- No `cargo test`/`cargo build`/`terraform plan` run in this review; findings are source-confirmed, not live-exploited.

