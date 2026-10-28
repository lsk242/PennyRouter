# PennyRouter — Security and Bug Review

**Review date:** 28 October 2026 (UTC)  
**Repository:** [lsk242/PennyRouter](https://github.com/lsk242/PennyRouter)  
**Pinned revision:** [`39356da2af67759751c819e48b98b61b6187fdef`](https://github.com/lsk242/PennyRouter/commit/39356da2af67759751c819e48b98b61b6187fdef)  
**Commit:** 28 October 2026, 20:06:53 UTC — “Iter 8: drop-in fallback proxy (proxy unknown paths upstream + log method/path)”  
**Declared workspace crate versions:** `0.11.0`

## Executive summary

**Do not treat this revision as ready for an untrusted public gateway without additional controls.** The most important security findings are that routed requests silently lose caller-supplied provider/privacy restrictions, and unauthenticated incomplete uploads can retain substantial memory without a deadline or aggregate admission limit. These are source-confirmed defects, not claims of exploitation against a deployment.

The review also found concrete HTTP compatibility, routing, artifact-recovery, and deployment bugs. In particular, compressed responses lose their encoding headers; the selected provider endpoint/precision is not transmitted upstream; streaming failures look like clean successful EOF; the Terraform default targets the wrong upstream API path; and deployment readiness checks report success after every probe has failed.

**11 primary findings: 2 High, 7 Medium, 2 Low**, plus lower-priority correctness issues and separately identified operational gaps/hardening recommendations. Severity reflects the stated conditions, not an assumed production deployment. No Critical issue, remote-code-execution exploit, SQL injection, confirmed SSRF, request-smuggling exploit, or compromised credential was established. This is not a security certification.

### Primary findings

| ID | Severity | Finding | Evidence / important condition |
|---|---|---|---|
| F01 | **High** | Caller privacy/provider restrictions are overwritten | Routed recognized model; request-level restrictions can disappear |
| F02 | **High** | Incomplete uploads retain buffers without deadline/admission limit | Network-accessible listener without effective outer limits; no valid key needed |
| F03 | Medium | Response-header loss corrupts compressed responses | Upstream returns encoded content advertised by client |
| F04 | Medium | Chosen endpoint and precision are not enforced upstream | Provider has multiple endpoint/tier/precision offerings |
| F05 | Medium | Upstream response bodies are collected without a byte bound | Large upstream response; exploitability is conditional |
| F06 | Medium | SSE failures become successful truncated responses | Upstream stalls or fails after headers |
| F07 | Medium | Partial artifact publication prevents recovery to valid older data | Interrupted/concurrent publication or corrupt manifest |
| F08 | Medium | Terraform default omits OpenRouter’s `/api` prefix | Default Hetzner module configuration |
| F09 | Low | Deployment readiness loop succeeds after all failures | All health probes fail but sleeps succeed |
| F10 | Low | Missing quality scores outrank known scores | Equal-tier candidates on quality-oriented routing |
| F11 | Medium | Time-boxed free promotions do not reach free-route eligibility | Promo supplied through `catalog-rank --promos` |

**Recommended order:** fix F01/F02 before public exposure; then F03/F04/F05/F06 and deployment F08/F09; then artifact recovery, ranking/free-route correctness and refresh integration. Add regression tests at the actual HTTP/artifact boundaries, not only pure selection functions.

## Scope, method, and limitations

- Read-only static review of the gateway, routing/metrics, all catalog source modules/binaries, tests/fixtures, Cargo manifests/lockfile, Docker/Compose, deployment shell script, Hetzner Terraform/cloud-init, CI workflows, and relevant product/operations documents.
- The source snapshot comprised **107 files** fetched at the pinned commit. Each local file’s Git blob SHA-1 was checked against the GitHub tree; all 107 matched. Root `AGENTS.md` was read. No repository files were edited or commits/PRs/issues created.
- Independent reviewers examined HTTP/gateway, routing, catalog, and dependencies; key findings were cross-checked against surrounding callers, tests, and contrary documentation.
- Seven small **independent standard-library/control-flow models** were executed, covering provider-object replacement, gzip representation handling, quality sorting, fixture price arithmetic, Terraform URL construction, failed readiness-loop semantics, and negative prices. These models **do not import or execute PennyRouter** and do not substitute for integration tests.
- **Not executed:** Cargo build/test/Clippy, downloaded project scripts, dependency installation, Docker build/run, Terraform plan/apply, load/fuzz tests, live provider calls, or production probes. No real API keys were used. Regression designs below are recommendations, not tests claimed to have run.
- Dependency checks were targeted comparisons with primary RustSec advisories, not a complete `cargo audit`, SBOM, container/native-library scan, or compiled feature audit. Production ingress controls, deployment state, secret history, cloud-account settings, and GitHub protection settings were not inspected.
- “Confirmed” means the defect and its stated consequence follow from the reviewed source and, where relevant, verified library/API semantics. Actual production reachability and impact remain conditional on the specified configuration.

### Relevant trust boundaries

The gateway accepts caller-supplied backend credentials rather than holding its own OpenRouter key. It reads/buffers the request and can make routing/session decisions **before** upstream credential validation. Catalog/ranking artifacts are local operator-managed inputs, not files directly writable through the public HTTP API. The current catalog synchronizer consumes input files; it is not a live web crawler or live LLM tool executor. These distinctions materially limit several hypothetical attack claims.

## Detailed primary findings

### F01 — Routed requests silently discard caller security/privacy constraints

**High · confidentiality/policy enforcement · source-confirmed**

**Evidence:** [`routing.rs:429–446`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L429-L446), especially lines 441–444; selection receives only model and authorization at lines 435–439.

For a recognized model, `route_request` replaces the entire incoming `provider` object with a new object containing only `order` and `allow_fallbacks:false`. It neither passes the original provider constraints into selection nor preserves them for OpenRouter to enforce.

A request such as:

```json
{
  "model": "a/model-present-in-the-catalog",
  "messages": [{"role": "user", "content": "synthetic test data"}],
  "provider": {
    "zdr": true,
    "data_collection": "deny",
    "only": ["AllowedProvider"],
    "max_price": {"prompt": 1, "completion": 2}
  }
}
```

can leave the gateway with:

```json
"provider": {"order": ["ChosenProvider"], "allow_fallbacks": false}
```

**Impact/preconditions:** With catalog routing enabled, a request relying on request-level privacy restrictions or a provider allowlist can be sent under a weaker policy, potentially exposing prompts to a disallowed provider/data policy. Price caps and supported-parameter/quantization constraints can also disappear. Account-wide upstream restrictions may still protect a particular account; this review does not claim those are bypassed. Unknown models/plain passthrough do not take this rewriting path. OpenRouter documents these request-level controls in its [provider-selection guide](https://openrouter.ai/docs/guides/routing/provider-selection).

This is **not** a complaint that the `/privacy` model suffix is ignored: that suffix is explicitly deferred in [`docs/routing.md:53–56`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/docs/routing.md#L53-L56). The defect is erasing existing backend-native restrictions.

**Fix:** Treat caller restrictions as hard constraints. Preserve security-sensitive fields, filter candidate endpoints accordingly, and intersect the chosen provider with `only`/`ignore` and other restrictions. If the gateway cannot enforce a restriction, either preserve it for authoritative upstream rejection or fail explicitly—never silently remove it. Do not reuse an incompatible session pin.

**Regression:** Record the exact outgoing body at a local mock for recognized and unknown models. Assert `zdr`, `data_collection`, allow/deny lists, price caps, parameter requirements and quantizations survive or cause an explicit rejection. Include sticky-session requests where the allowed provider set changes.

### F02 — Unauthenticated incomplete uploads can hold large buffers indefinitely

**High · availability · source-confirmed; exposed-deployment condition**

**Evidence:** [`lib.rs:262–292`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L262-L292); callers at [`203–205`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L203-L205) and [`253–257`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L253-L257); router setup [`128–136`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L128-L136); listener [`main.rs:9–13`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/main.rs#L9-L13).

`read_capped_body` appends chunks to a `Vec` and waits on `stream.next().await` without a total upload deadline or read-idle deadline. Its **4,000,000-byte** cap only rejects a newly arriving chunk that exceeds the cap. Upstream timeouts do not start until collection finishes. No gateway-level concurrency/admission or aggregate buffer budget is installed before collection.

**Impact/preconditions:** A reachable client can declare a body, send almost 4 MB, then leave it unfinished. That buffer remains retained per request without requiring a valid backend key or an upstream call. Hundreds of retained requests can exceed the advertised approximately 1 GB footprint. A properly configured outer proxy may mitigate exposure; the shipped gateway itself does not supply these protections. The risk is not that one request bypasses the byte cap, but that many incomplete below-cap requests have unbounded lifetime/count.

**Fix:** Enforce total upload deadlines **and** read-idle limits, plus bounded admission and aggregate buffered-byte accounting before body collection. An idle timeout alone does not stop a trickle sender. Reject oversized declared lengths early while retaining incremental checks for chunked/misdeclared bodies; release permits/buffers on cancellation.

**Regression:** In an isolated test, use one small partial upload and a short configured deadline; assert timeout/connection closure and zero mock-upstream requests. Test a tiny concurrency limit with two or three clients, chunked uploads and trickle uploads. No OOM-scale stress is necessary to prove the controls.

### F03 — Dropped response headers break compressed content and proxy semantics

**Medium · correctness/interoperability · source and dependency semantics confirmed**

**Evidence:** request header forwarding [`lib.rs:308–314`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L308-L314); response reconstruction [`349–363`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L349-L363), [`390–394`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L390-L394), [`422–426`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L422-L426); reqwest features [`gateway/Cargo.toml:22`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/Cargo.toml#L22).

The gateway forwards `Accept-Encoding`, but reconstructs responses with almost no upstream headers beyond `Content-Type`. Reqwest is configured with defaults disabled and only `rustls-tls`; automatic gzip decoding is optional and not enabled. Thus gzip bytes can pass through unchanged while `Content-Encoding: gzip` is discarded. Clients then interpret compressed bytes as plain JSON or SSE. See [reqwest 0.12.28 gzip documentation](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.gzip).

The same code removes `Retry-After`, `Content-Range`, `WWW-Authenticate`, cache validators and other end-to-end metadata. This is relevant to the broad fallback proxy, not only chat. No cache-poisoning/data-leak exploit is asserted.

**Fix:** Preserve safe end-to-end headers consistently on success, streaming and error paths; remove hop-by-hop fields and connection-nominated fields. Choose transparent compressed passthrough or explicit decoding, maintaining coherent metadata. If a body is rewritten, update representation-dependent headers. Error parsing also needs a defined compression policy.

**Regression:** Mock gzip JSON and gzip SSE; verify valid downstream decoding. Test `429 + Retry-After`, `206 + Content-Range`, and `401 + WWW-Authenticate`. The isolated model confirmed compressed bytes do not parse as plain JSON; the actual Rust transport path was not run.

### F04 — The selected offering’s endpoint/precision is lost when forwarding

**Medium · routing/price/quality correctness · source-confirmed; live provider behavior not tested**

**Evidence:** `Decision` records tag and precision [`routing.rs:71–80`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L71-L80); selection retains them [`332–338`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L332-L338); the wire request uses only provider display name [`440–444`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L440-L444). Example multi-offering data: [OpenAI fixture](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/tests/fixtures/endpoints/openai__gpt-6.1-terra-pro.json).

The router selects among endpoint tags/precision classes, but forwards only `decision.provider`. It drops the chosen `decision.tag` and does not add the selected quantization or service tier. Picking the right offering internally is insufficient if several offerings share the same provider name.

The checked-in fixture has `openai/flex`, `openai`, and `openai/fast`. Independent arithmetic using the project’s balanced weights gives costs `0.0000012825`, `0.000002565`, and `0.00000513` respectively. A price decision can select flex but emit only `"order":["OpenAI"]`. These are fixture-derived values, **not a statement of current market pricing**. OpenRouter’s [specific-endpoint guidance](https://openrouter.ai/docs/guides/routing/provider-selection#targeting-specific-provider-endpoints) distinguishes endpoint/tier targeting from a general provider preference.

**Impact/preconditions:** Multi-offering providers may receive a request for a different tier/precision than the one whose price or quality was ranked. The fixture’s base price is twice flex, illustrating the consequence, not proving an actual charge. Acceptance/canonicalization of the display name `OpenAI` was not live-tested; no separate “display names always fail” claim is made.

**Fix:** Store and transmit the canonical provider/endpoint identity and compatible service-tier/quantization restrictions. Preserve the exact raw quantization as well as the broad precision class: the classifier groups BF16/FP16 and several 4-bit formats, so a class is not necessarily a valid exact backend quantization value. Preserve caller constraints from F01 and reject conflicts. Test the outgoing request, not just the returned internal `Decision`.

**Regression:** Two same-provider offerings with different tags/prices/precision; assert the exact chosen offering is enforceable in the outgoing wire representation. Include the three fixture tiers and a caller-supplied incompatible `service_tier`.

### F05 — Large upstream success/error bodies have no memory bound

**Medium · availability · confirmed missing bound; exploitability conditional**

**Evidence:** [`lib.rs:349–350`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L349-L350), [`410–420`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L410-L420), [`445–451`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L445-L451).

Non-SSE success and error paths call `upstream.bytes()` to collect the entire body. A 300-second timer limits duration, not received bytes. The 200-character error snippet is generated only after full collection; parsing the error JSON and cloning its `error` field can allocate further. The [pinned reqwest response implementation](https://raw.githubusercontent.com/seanmonstar/reqwest/v0.12.28/src/async_impl/response.rs) confirms whole-body collection.

**Impact/preconditions:** A sufficiently large/fast response can exhaust memory before the timer expires. This requires a reachable large-object/output path, a caller able to request it, or a malfunctioning/malicious configured upstream. The fallback can forward non-chat endpoints, but no particular default OpenRouter large-response exploit was established or probed. This is separate from F02’s unauthenticated pre-upstream retention.

**Fix:** Stream responses that need no rewriting with backpressure/cancellation. Apply a strict bounded reader to error bodies and any responses that must be buffered; reject/stop oversized payloads and enforce aggregate budgets.

**Regression:** A local mock returns chunked 200 and 500 bodies slightly over a deliberately tiny test cap, with absent/misleading Content-Length. Assert bounded memory/early cancellation without a large load test.

### F06 — SSE failures are converted into clean successful EOF

**Medium · output integrity · source-confirmed**

**Evidence:** [`lib.rs:375–387`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L375-L387); existing stall test [`streaming.rs:150–194`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/tests/streaming.rs#L150-L194).

The stream returns `None` for timeout and upstream body error—the same value used for normal completion. Since HTTP 200 was already sent, the downstream body ends successfully with only a prefix. The existing test explicitly unwraps successful body collection of this prefix, so it encodes the problematic behavior.

**Impact:** Consumers relying on transport/body success can accept incomplete output as complete. Clients that require a terminal SSE marker may detect it; not every SDK is necessarily affected. Headers cannot be changed after transmission, but body failure can still be signaled.

**Fix:** Propagate a terminal body error/reset instead of normal EOF, or implement a documented compatible SSE error event where appropriate. Distinguish normal completion, timeout, transport failure, and cancellation in metrics/logs.

**Regression:** Mock one delta followed by a stall, reset, or partial event. Require a downstream error/explicit terminal failure, not successful EOF. Keep normal terminal events byte-for-byte unchanged.

### F07 — Non-atomic publication and fail-fast discovery defeat artifact recovery

**Medium · availability/routing integrity · source-confirmed operational failure**

**Evidence:** catalog publisher [`artifact.rs:42–61`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/artifact.rs#L42-L61); ranking publisher [`rank.rs:452–469`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/rank.rs#L452-L469); discovery/verification [`routing.rs:450–501`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L450-L501); startup fallback [`lib.rs:111–118`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L111-L118).

Both publishers write directly to final artifact/manifest names. A failed/interrupted write can expose a truncated matching manifest. `newest_manifest` parses every matching file with fail-fast `?`; **any** malformed matching manifest aborts discovery. It also chooses the newest manifest before verifying its referenced data and does not try an older valid artifact when verification fails. Republishing identical content rewrites an existing content-addressed file rather than keeping it immutable.

**Trigger/impact:** Keep a good older catalog; leave an incomplete new `catalog-*.manifest.json` after a failed publish. On startup, routing can be disabled even though valid data exists. A malformed older file can also continue poisoning discovery after later good publications. An already-running router retains its current in-memory snapshot when an explicit reload fails—this is not a claim that one malformed file immediately crashes all serving traffic. Ranking discovery can similarly fail or retain an old ranking.

**Fix:** Stage to unique same-filesystem temporary files outside discovery globs; flush/sync as needed; atomically rename immutable data first and manifest last. Never truncate published content-addressed files. Discovery should validate candidates newest-to-oldest, skip/log invalid candidates and retain the newest valid compatible snapshot. Serialize writers or use unique working databases.

**Regression:** Fault-inject writes/crashes, publish concurrently with reads, leave malformed and checksum-invalid candidates alongside a valid older artifact, and republish identical bytes. Verify startup and explicit reload recovery.

### F08 — Default Hetzner deployment forwards to the wrong OpenRouter path

**Medium · deployment outage · source-confirmed configuration bug**

**Evidence:** [`variables.tf:83–87`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/terraform/hetzner/variables.tf#L83-L87) sets `https://openrouter.ai`; [`cloud-init.yaml:23–25`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/terraform/hetzner/templates/cloud-init.yaml#L23-L25) exports it; [`lib.rs:306`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L306) appends the request path. Compare the correct application default [`lib.rs:108–109`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L108-L109) and Compose default [`compose.prod.yaml:16`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/compose.prod.yaml#L16).

The Terraform default produces `https://openrouter.ai/v1/chat/completions`, whereas OpenRouter documents `https://openrouter.ai/api/v1/chat/completions` in its [API reference](https://openrouter.ai/docs/api/api-reference/chat/create-a-chat-completion). Terraform explicitly overrides the application’s correct default, so the deployed gateway targets the wrong endpoint unless the operator overrides the variable.

**Impact:** Inference is misconfigured while `/health` can still report success. No live call was made and no exact upstream response/status is asserted.

**Fix:** Align the Terraform default to `https://openrouter.ai/api`; validate expected base-URL semantics. Add a generated-config test and a mock-backed forwarding smoke test, not only liveness.

**Regression:** Render cloud-init with defaults; assert the eventual forwarded path includes exactly one `/api/v1`. Cover explicit custom upstream prefixes.

### F09 — Health-check exhaustion still reports deployment success

**Low · operational correctness · source and isolated shell semantics confirmed**

**Evidence:** [`deploy.sh:106–108`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/scripts/deploy.sh#L106-L108), duplicated at [`cloud-init.yaml:59`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/terraform/hetzner/templates/cloud-init.yaml#L59).

The loop runs `curl ... && break || sleep 2`. When curl fails, successful sleep becomes the iteration’s status. After the final failed probe the loop still returns success; the deploy script prints its “deployed” message. `set -e` does not fix this AND/OR-list control flow.

**Impact:** Failed startup can be reported as a successful deployment. The check also lacks per-probe curl timeouts, so its apparent 30×2-second retry budget is not a hard bound.

**Fix:** Track success explicitly and exit nonzero after exhaustion. Set connect and total probe deadlines, and collect diagnostic service status without exposing secrets. Apply to both copies.

**Regression:** Stub all probes to fail and assert nonzero exit/no success message; then test success on the last attempt and a hanging probe. An independent shell model substituting `false` for the probe and `true` for sleep reproduced exit status 0 without invoking the project script or any network endpoint.

### F10 — Quality sorting promotes missing scores ahead of known scores

**Low · ranking correctness · source and independent model confirmed**

**Evidence:** [`rank.rs:47–85`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/rank.rs#L47-L85), reused by gateway fallback [`routing.rs:645–665`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L645-L665).

`asc_nulls_last` orders unknown scores after known numbers. Descending quality is implemented by reversing the **optional operands** (`asc_nulls_last(b.score,a.score)`), which also reverses the null policy. Within an equal quality tier, `[0.99, None, 0.01]` becomes `[None, 0.99, 0.01]`. Later tie-breakers cannot correct it.

**Impact/preconditions:** An unscored offering is favored over a scored offering for quality-oriented fresh selection; both offline and fallback ranking use this comparator. Partial judgment records can legitimately supply a tier without a score. Severity could rise where quality is a material business policy; no confidentiality impact is claimed.

**Fix/regression:** Compare numeric scores descending only for the `Some/Some` case while keeping known-before-unknown branches. Test known-high, unknown, known-low at equal tier in offline rank and gateway fallback, preserving tier precedence.

### F11 — Time-boxed free promotions are unavailable to free-route selection

**Medium · routing/feature correctness · source-confirmed integration defect**

**Evidence:** free candidates are filtered before ranking at [`routing.rs:352–356`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L352-L356); inline cost uses only the catalog discount [`538–546`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L538-L546); ranking artifacts carry identities/order, not eligibility/expiry [`rank.rs:264–289`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/rank.rs#L264-L289); promo-window evaluation [`price.rs:167–214`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/price.rs#L167-L214).

`catalog-rank --promos` can evaluate a 100% active discount and reorder that model’s offerings. But the published document does not carry the zero-cost eligibility/expiry fact. `decide_free` first filters using `inline_cost`, which knows only the catalog’s static discount column. A normally paid offering made free solely by a current promo window is therefore excluded **before** ranking can help. If no other free offering exists, the original request passes through to OpenRouter rather than taking the locally promoted offer.

**Impact/caveat:** This loses advertised promo-aware free selection. It does not by itself prove a paid request is charged as free or an actual billing loss. Promo expiry and stale static discounts are also unsafe to infer from old snapshots; the missing refresh integration discussed below compounds staleness but is a separate issue.

**Fix:** Publish validated current free eligibility and validity intervals with the routing snapshot, or carry the authoritative promo facts into gateway eligibility. Do not treat an expired/missing time-boxed fact as a permanent zero. Keep eligibility constraints separate from relative ordering.

**Regression:** A positive-price endpoint with a 100% window supplied only through `--promos` must become eligible during the window and ineligible at its end, including with an existing session pin and without restarting the server. Test fallback behavior when no offer is eligible.

## Additional confirmed lower-priority issues

These are actionable but should not distract from the primary fixes.

| ID | Issue / evidence | Consequence and recommended regression |
|---|---|---|
| A01 | **Chat query parameters disappear.** [`lib.rs:196–223`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L196-L223) never extracts the URI and hardcodes the chat path. | Unlike fallback, chat loses query options needed by some custom upstreams. No required OpenRouter query was established. Forward path-and-query; test escaped multi-parameter queries with a mock. |
| A02 | **Negative prices are accepted and sort first.** [`process.rs:40–54`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/process.rs#L40-L54), [`price.rs:59–75`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/price.rs#L59-L75). | Operator-imported `prompt:"-1", completion:"-1"` yields effective cost `-1`, beating valid prices. No public request can directly write these fields. Require finite nonnegative prices and valid discounts; test negative, non-finite and overflow strings while preserving valid zero/unknown. The negative case was independently modeled; downstream NaN/SQLite behavior was not tested. |
| A03 | **Offline tag tie-breaking differs from fallback.** [`rank.rs:398–400`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/rank.rs#L398-L400) supplies `tag:None`, while [`routing.rs:656–660`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L656-L660) supplies the actual tag. | Equal same-provider offerings retain insertion order offline but sort by tag inline. Use the real tag and test permuted endpoint inputs. Identical database input remains deterministic; this is not arbitrary nondeterminism. |
| A04 | **Streaming observability ends at headers, not stream completion.** [`lib.rs:150–170`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L150-L170). | In-flight guard drops and success/latency are recorded while SSE may still run. Track body lifetime and terminal outcome or label the existing metric as header latency. Test a paused stream and a later stream failure. |
| A05 | **Wrong-type optional promo/judgment tags widen scope.** [`price.rs:130–133`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/price.rs#L130-L133), [`judge.rs:94`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/judge.rs#L94). | File input `"tag":42` is treated as absent, applying a record more broadly across provider offerings. Reject wrong types rather than converting to an unscoped fact. This is input-file robustness, not a public HTTP authorization bypass. |

## Operational gaps and hardening — not additional proven remote vulnerabilities

### Refresh and startup integration are incomplete

The shipped binary loads `AppState` once. [`Router::reload`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L186-L196) exists, but no production caller, watcher, periodic task, signal handler or refresh endpoint invokes it. Source search found swap/reload uses in tests rather than a running refresh path. Consequently, publishing a new catalog/ranking file does not update a running gateway.

This is **explicitly acknowledged** in [`docs/routing.md:67–70`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/docs/routing.md#L67-L70), which says startup-only loading, but conflicts with the automatic-pickup wording in [`docs/operations.md:79–85`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/docs/operations.md#L79-L85) and the periodic catalog container. Treat it as an integration gap and conflicting operational contract, not a concealed security feature. On a fresh shared volume, gateway startup can precede initial catalog publication, fail loading, and stay in passthrough indefinitely. No Compose dependency/readiness arrangement or retry connects those states.

Implement explicit background refresh/retry outside the request path, validate compatible catalog/rank generations together, preserve last-good state and expose readiness/version/age. Alternatively document restart-only operation clearly and remove automatic-refresh promises. A local integration test should publish a new artifact while the process remains alive and verify a changed decision, then simulate a corrupt update.

The production Compose and Terraform templates also provide no `CATALOG_DB` environment/mount wiring ([Compose](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/compose.prod.yaml#L9-L17), [cloud-init](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/terraform/hetzner/templates/cloud-init.yaml#L17-L25)). Their default deployment is therefore a passthrough gateway, not a configured optimizing router. Running catalog sync separately is reasonable; document and supply the artifact-delivery/read-only mount contract if routing is intended.

### Confidentiality and resource hardening

- **Raw query logging:** [`fallback_proxy:240–251`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L240-L251) logs full path-and-query. Tokens/private identifiers placed in query parameters enter logs. No existing credential leakage was established; normal Authorization headers are not logged here. Omit/redact query values and use route templates. Test with synthetic values only.
- **Session admission/retention:** [`routing.rs:264–329`](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L264-L329) retains raw authorization strings and inserts pins before upstream validation. The table is capped at 16,384 entries but lacks expiry/eviction. Arbitrary invalid keys can consume the table so new legitimate callers no longer gain stickiness. Authenticate/admit appropriately, bound key size, use an opaque keyed credential identifier, and expire/evict entries. Do not describe this as credential exfiltration or unlimited session count.
- **HTTP hop-by-hop headers:** forwarding removes only Host/Content-Length. Sanitize connection-specific and connection-nominated fields per [RFC 9110 §7.6.1](https://www.rfc-editor.org/rfc/rfc9110.html#section-7.6.1). No request-smuggling chain was established; Hyper performs its own framing normalization.
- **Redirect policy:** reqwest’s default follows redirects. For a reverse proxy, return them or restrict destinations/schemes/hops deliberately ([pinned reqwest redirect docs](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.redirect)). No attacker-controlled default-upstream redirect was identified; this is not a confirmed SSRF or cross-host credential-leak finding.
- **TLS and exposed deployment ports:** production Compose publishes HTTP on all interfaces by default, while Terraform supports optional Caddy TLS. Do not send real bearer keys over an untrusted plaintext network; bind privately/loopback behind a TLS ingress. Public liveness/metrics exposure is a deployment decision, not by itself auth bypass.
- **Artifact trust:** SHA-256 checks detect accidental corruption, not authenticity. Constrain manifest filenames to expected basenames and protect the artifact directory/transfer. Local manifest control is a stronger prerequisite than a remote request; no remote arbitrary-file-read exploit was established.
- **Supply-chain/process controls:** pin deployment images/actions to reviewed immutable identifiers where practical; add explicit advisory and image scanning with an exception policy. Existing non-root container users, read-only gateway catalog mount in development, committed Cargo.lock, and weekly Dependabot updates are useful controls, not evidence that all risks are covered.

## Dependency spot-check

**No confirmed vulnerable dependency was found in the targeted checks below.** These are patched-version comparisons, not a comprehensive clean bill of health. Lockfile presence alone does not prove a feature is compiled or reachable.

| Package locked | Primary advisories checked | Result |
|---|---|---|
| Tokio `1.54.1` | [RUSTSEC-2026-0384](https://rustsec.org/advisories/RUSTSEC-2026-0384.html), [0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html) | Locked version includes the listed fixes. |
| rustls `0.23.46` | [0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html), [0347](https://rustsec.org/advisories/RUSTSEC-2026-0347.html), [0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html), [0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html) | Locked version includes the listed fixes. |
| rustls-webpki `0.103.16`; webpki-roots `1.0.11` | [0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), [0381](https://rustsec.org/advisories/RUSTSEC-2026-0381.html) | Both patched for the checked advisories. |
| quinn-proto `0.11.19` | [0338](https://rustsec.org/advisories/RUSTSEC-2026-0338.html), [0339](https://rustsec.org/advisories/RUSTSEC-2026-0339.html), [0340](https://rustsec.org/advisories/RUSTSEC-2026-0340.html), [0341](https://rustsec.org/advisories/RUSTSEC-2026-0341.html), [0342](https://rustsec.org/advisories/RUSTSEC-2026-0342.html), [0343](https://rustsec.org/advisories/RUSTSEC-2026-0343.html), [0037](https://rustsec.org/advisories/RUSTSEC-2026-0037.html) | Patched. Workspace does not request reqwest `http3`; no active QUIC surface inferred merely from lockfile. |
| time `0.3.55` | [RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009.html) | Patched. |
| libsqlite3-sys `0.35.0`; rusqlite `0.37.0` | [RUSTSEC-2022-0090](https://rustsec.org/advisories/RUSTSEC-2022-0090.html), [2021-0128](https://rustsec.org/advisories/RUSTSEC-2021-0128.html), [2020-0014](https://rustsec.org/advisories/RUSTSEC-2020-0014.html) | Patched for these advisories. Bundled SQLite still requires its own native-code vulnerability tracking. |
| axum-core `0.5.6` | [RUSTSEC-2022-0055](https://rustsec.org/advisories/RUSTSEC-2022-0055.html) | Patched; not evidence that this application’s manual body collector has time/admission limits. |

Versions are recorded in the [pinned Cargo.lock](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/Cargo.lock). Reqwest’s selected features are in the [gateway manifest](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/Cargo.toml#L19-L22). The [Security workflow](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/.github/workflows/security.yml#L7-L17) runs Gitleaks, while [Rust CI](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/.github/workflows/rust.yml#L16-L18) runs fmt/Clippy/tests; no explicit lockfile advisory gate was found. [Dependabot](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/.github/dependabot.yml#L7-L10) already schedules weekly Cargo updates. No claim is made about private repository-level security settings or historical scan results.

## Verification evidence and proposed acceptance checks

### Executed independent checks

```text
PASS — all 107 materialized files match pinned Git blob SHA-1
PASS — provider-object replacement removes caller constraints
PASS — gzip bytes without encoding metadata are not plain JSON
PASS — reversed optional-score comparator: [None, 0.99, 0.01]
PASS — fixture costs: flex 1.2825e-6, base 2.565e-6, fast 5.13e-6
PASS — Terraform default constructs https://openrouter.ai/v1/chat/completions
PASS — modeled all-failed readiness loop exits 0
PASS — negative input prices produce -1.0, ahead of valid positive costs
```

“PASS” above means the isolated check demonstrated the described behavior/integrity property; **it does not mean the application is correct or its test suite passed**. The seven behavior models used only standard-library operations and a harmless shell-control-flow model. The repository’s Rust tests were read but not run.

### Before accepting remediation

1. Add mock-backed tests for caller policy preservation and exact endpoint/precision enforcement.
2. Add bounded upload/response tests, cancellation tests and admission-release tests using small configured limits.
3. Test compression plus end-to-end response headers across all three response paths, chat query retention, and broken SSE signaling.
4. Fault-test artifact publication/discovery and exercise real process refresh/startup recovery.
5. Test free-promo activation/expiry, missing quality values, invalid prices and consistent tag tie-breaking.
6. Validate generated deployment defaults, all-failed health probes, timeout behavior, and mock-backed request forwarding in addition to `/health`.
7. In an appropriately isolated, authorized development environment, run the full Rust suite/format/Clippy, Docker smoke tests, Terraform validation, dependency advisory check and container scan against the exact fixed revision. These checks remain **not run in this review**.

## Bottom line

The project has a small, inspectable Rust core and useful tests, but important security-policy and resource-boundary behaviors are missing or contradicted by the implementation. Prioritize **preserving caller restrictions and bounding unauthenticated work**, then make the HTTP proxy and chosen-routing decision faithfully match what callers requested. Validate these properties end-to-end before relying on the gateway for sensitive prompts or public traffic.
