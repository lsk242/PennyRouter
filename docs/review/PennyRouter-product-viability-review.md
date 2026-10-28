# PennyRouter product viability and improvement review

**Purpose:** Assess PennyRouter as a personal, free-first, very-low-cost model router for coding harnesses such as OpenCode. This is not an enterprise gateway evaluation.

**Reviewed on:** 28 October 2026 UTC.  
**Repository:** [lsk242/PennyRouter](https://github.com/lsk242/PennyRouter).  
**Pinned source:** [`39356da2af67759751c819e48b98b61b6187fdef`](https://github.com/lsk242/PennyRouter/commit/39356da2af67759751c819e48b98b61b6187fdef), Iter 8, declared version 0.11.0. GitHub comparison confirmed `main` was identical during this review.  
**Companion:** The separate PennyRouter security and bug review covers detailed defects. This document changes their priority to match personal coding use; it does not replace that review.

## Recommendation

**Continue, but narrow the product promise and finish the policy before expanding the platform.** PennyRouter is a plausible personal tool and an interesting small open-source project. The reviewed version is an early foundation, not yet a dependable implementation of the requested free-first coding router.

The valuable promise is: *use a free model when it can reliably do my coding task; otherwise use an explicitly approved inexpensive model; never escape my price band without asking; show what the completed task actually cost.* That is more specific and useful than finding the cheapest provider for an arbitrary model.

The implementation mostly addresses that latter problem. It selects providers for the model the caller already named, plus a special cross-model `openrouter/free` route. It does not yet establish a coding quality floor, enforce a hard personal spending policy, or close the loop between routing and task success. The free path lacks the tool/context eligibility that a coding agent needs. These are core product gaps, not optional enterprise features. [Current routing code](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L245-L446)

My assessment is:

- **Personal usefulness:** credible, if setup stays simple and the router proves it saves money or repeated manual model switching on the owner's actual work.
- **Open-source niche:** plausible among cost-conscious coding-harness users who want control over model eligibility and spending.
- **Current daily-driver readiness:** insufficient for a strong “safe budget plus minimum coding quality” claim.
- **Commercial viability:** unproven. There is no user research, retention evidence, willingness-to-pay evidence, or measured savings in this review. A hosted subscription could consume the very savings this audience wants.

The strongest reason to build it is a reliable personal workflow. A broad gateway business is not necessary to justify that outcome.

## Clarifying the intended model and price floor

The user's model names are real reference points, but they do not identify one interchangeable quality or price tier.

**“GPT Luna” reasonably maps to `gpt-6-luna`.** OpenAI's official catalog lists it. **“DeepSeek V4” needs a variant:** the current direct API pricing page lists `deepseek-flash`, serving DeepSeek-V4.1-Flash-1017, and `deepseek-v4-pro`, serving V4-Pro-0813. Legacy V4 Flash identifiers now resolve to the newer Flash model. This is precisely why an alias alone is not enough to pin a benchmark baseline. [OpenAI model page](https://developers.openai.com/api/docs/models/gpt-6-luna), [DeepSeek current pricing](https://api-docs.deepseek.com/quick_start/pricing/)

### Current direct API reference prices

USD per one million tokens, checked on the review date. These are provider-direct rates, not a promise about the price through OpenRouter or another intermediary.

| Model and service condition | Uncached input | Cached input | Output |
|---|---:|---:|---:|
| GPT-6 Luna, Standard, input up to 272K per request | $0.10 | $0.01 | $0.50 |
| DeepSeek V4.1 Flash, off-peak | $0.15 | $0.003 | $0.60 |
| DeepSeek V4.1 Flash, peak | $0.30 | $0.006 | $1.20 |
| DeepSeek V4 Pro, off-peak | $0.66 | $0.022 | $1.98 |
| DeepSeek V4 Pro, peak | $1.32 | $0.044 | $3.96 |

Sources: [OpenAI pricing](https://developers.openai.com/api/docs/pricing), [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/). Flash's peak windows are weekday 01:00–04:00 and 06:00–10:00 UTC, excluding Chinese public holidays; other times are off-peak. Use the current provider policy rather than hard-coding a calendar assumption indefinitely.

For Luna, the model page lists cache writes at $0.125 per million tokens and higher whole-request rates above 272K input tokens: input/cache rates double and output rises by 1.5 times. Other service tiers and longer cache-retention options have separate pricing. This review uses Standard, short-context rates, not discounted batch rates for an interactive coding session. [Luna details](https://developers.openai.com/api/docs/models/gpt-6-luna), [pricing options](https://developers.openai.com/api/docs/pricing)

**Working product interpretation:** anchor the initial paid pool around Luna and Flash economics, while treating V4 Pro as a separately configured, higher-price option. This is a recommendation, not an assumption that the user has approved a particular dollar cap. Configuration must ask for explicit input, output, cache-write and task/day/month ceilings. Do not silently widen those ceilings when an alias changes or a promotion ends.

None of these prices proves coding equivalence. The quality floor must name a baseline model, API path, reasoning setting, harness version and task set. A model can be excellent at code generation and still fail a specific harness's tool loop.

### A compatibility issue that directly affects this goal

OpenAI documents Luna function calling through Chat Completions only with `reasoning_effort: none`; its Responses API supports the intended reasoning/tool workflow. PennyRouter applies routing to `/v1/chat/completions`, while `/v1/responses` takes the generic passthrough path. **Forwarding a Luna Responses request is not the same as applying PennyRouter's routing or budget policy to it.** This must be an explicit integration decision. [Luna API requirements](https://developers.openai.com/api/docs/models/gpt-6-luna), [PennyRouter endpoint handling](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L128-L137), [API contract](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/docs/api.md)

DeepSeek's thinking/tool loop also has provider-specific requirements: its documentation requires prior `reasoning_content` to be returned on subsequent tool-bearing turns. Byte forwarding is useful, but the harness and adapter must preserve the correct fields and continuation semantics. Test this explicitly rather than assuming that all OpenAI-shaped APIs behave alike. [DeepSeek thinking and tool calls](https://api-docs.deepseek.com/guides/thinking_mode/)

## What the project already does well

The architecture has sensible foundations for a small personal service:

- **Local routing decisions:** requests do not need an extra classifier-model call. That avoids another source of latency and paid tokens.
- **A small Rust gateway and separate catalog work:** moving ingestion and ranking away from inference requests is a good direction.
- **An explicit price model:** input, output, cache reads and cache writes are represented separately. Workload profiles are a better starting point than comparing input price alone.
- **Awareness of cache continuity and endpoint differences:** provider affinity and precision classes acknowledge real sources of cost and quality variation, even though current enforcement needs work.
- **Inspectability:** the source is small enough to reason about; there are fixtures, unit/integration test sources, a lockfile, and clear iteration records. That is valuable for a personal tool whose behavior should be understandable.
- **Incremental delivery:** the repository already separates present functionality from many deferred ideas. Keeping this discipline is more useful than adding an elaborate automatic judge now.

These are architectural strengths, not measured performance or savings claims. The “roughly 1 CPU / 1 GB” and “blazing-fast” descriptions were not independently benchmarked in this review. [Repository overview](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/README.md), [price implementation](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/catalog/src/price.rs#L13-L75)

## Where the current implementation misses the personal coding goal

### Provider optimization is not yet quality-constrained model routing

For a named model, the router chooses one of that model's offerings. Only `openrouter/free` crosses model boundaries, and that path selects from zero-cost offerings without a coding-specific eligibility gate. The projected `Offering` fields do not carry tool support, effective context limits, or a measured coding pass rate. An FP precision class cannot stand in for these capabilities, especially across unrelated models. [Offering fields and decision paths](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L41-L107)

A suitable improvement is a small, explicit set of approved coding models, not a universal intelligence classifier. The task can stay on one chosen model for the session unless a clear eligibility or availability reason requires a change.

### Free eligibility and prices are not reliably live

The development Compose setup mounts catalog fixtures as input. The gateway loads its snapshot at startup; reload methods exist but the shipped server does not call them periodically. The promo ranker can reorder offers, but active promotion eligibility/expiry does not flow correctly into the free-route filter. A “continuously updated, free-first” promise therefore runs ahead of the implementation. [Compose inputs](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/compose.yaml), [startup loading](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/lib.rs#L104-L121), [free eligibility](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L342-L423)

Start with a few supported sources and timestamped, validated facts. A manually maintained allowlist with trustworthy expiry is preferable to a broad, stale catalog that confidently chooses the wrong offer.

### Current fallback semantics conflict with a strict spending promise

An unknown model, unavailable catalog, or failed lookup can become transparent upstream passthrough. That is convenient for a generic proxy, but a budget route must not silently become an unrestricted request. The current rewrite also replaces the caller's entire `provider` object, losing restrictions such as `max_price`, allowed providers and privacy constraints. Disabling upstream fallbacks does not restore the discarded policy, nor does it enforce the exact selected endpoint/precision. [Rewrite behavior](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L426-L446)

Keep an explicit passthrough mode if useful. A strict budget route should instead fail with a clear reason when policy cannot be established. The no-local-free-offer case currently forwards `openrouter/free` to OpenRouter; that is not proof of a paid charge, but it still abandons PennyRouter's own quality/eligibility decision.

### Affinity is broader and weaker than a coding session

Sessions are keyed by authorization plus requested model, not the harness's actual conversation. Two unrelated projects using the same key/model share a provider pin. Pins lack expiry. On the free route, a pin records provider and precision but not the selected model, so it is not a guarantee of model continuity. [Session and pin implementation](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L95-L131), [selection](https://github.com/lsk242/PennyRouter/blob/39356da2af67759751c819e48b98b61b6187fdef/crates/gateway/src/routing.rs#L264-L423)

Use an opaque conversation identifier and pin model, endpoint, relevant reasoning mode and policy generation. Keep it only while eligible. Price caps, expired promotions, exhausted quota and incompatible context must override affinity. Cache warmth is a preference, not permission to break the budget.

## Market fit and real substitutes

The first competitor is a user selecting one cheap model in OpenCode and leaving it there. That workflow has almost no additional setup or maintenance. PennyRouter must outperform its total cost and friction, not merely demonstrate a clever ranking algorithm.

| Alternative | What it already provides | What PennyRouter must add to justify itself |
|---|---|---|
| Native OpenCode model selection and direct provider access | Provider configuration, model selection and custom base URLs | Less switching and clearly safer personal spending without breaking harness features |
| OpenCode Console free/low-cost offerings | Coding-oriented model access, including time-limited free options | A useful policy across authorized sources, not just another layer over the same offers |
| OpenRouter Free Models Router | Free-model selection with capability filtering | A tested coding floor and stable task behavior rather than just zero token price |
| OpenRouter Pareto Code Router | Coding-score tiers, price-based selection, fallback models and best-effort session stickiness | Explicit personal price-band enforcement and evidence from the user's actual tasks |
| LiteLLM | Broad provider integration, routing, retries and spend controls | A substantially simpler personal setup; otherwise use an existing gateway |
| A local model through the harness | Avoids hosted token charges and can keep inference local | Better eligible-task results after hardware, electricity, setup and latency are considered |

Primary sources: [OpenCode models](https://opencode.ai/docs/models/), [providers](https://opencode.ai/docs/providers/), [Console model catalog](https://opencode.ai/v2/docs/console/models/), [OpenRouter free routing](https://openrouter.ai/docs/guides/routing/routers/free-router), [Pareto coding router](https://openrouter.ai/docs/guides/routing/routers/pareto-router), [LiteLLM overview](https://docs.litellm.ai/docs/), [LiteLLM budget behavior](https://docs.litellm.ai/docs/proxy/users).

Three qualifications matter:

1. **OpenRouter is not merely a static sort.** Its documented Pareto router already chooses coding models by a score tier, has same-tier fallbacks and supports conversation stickiness. The documentation says its router-specific parameter cannot directly cap request cost or latency, while OpenRouter separately offers provider price constraints and spend controls. This leaves a possible personal-policy niche, not an empty market. Do not advertise sticky routing itself as unique. [Pareto behavior and limits](https://openrouter.ai/docs/guides/routing/routers/pareto-router), [provider price caps](https://openrouter.ai/docs/guides/routing/provider-selection)
2. **Free offers may belong to a particular harness or account.** OpenCode currently lists a time-limited DeepSeek V4.1 Flash Free option, but says free-tier chat models require OpenCode. Its price page is not blanket authorization for an arbitrary proxy to reuse those offers. Check supported integration and terms; never rely on client impersonation, rotating accounts, or quota bypass. [OpenCode Console restrictions and free offers](https://opencode.ai/v2/docs/console/models/)
3. **More features have operational cost.** LiteLLM's budget documentation, for example, notes that DB-backed budget checks do not cap a DB-less deployment. Comparing feature checkboxes without deployment conditions is misleading. [LiteLLM budgets](https://docs.litellm.ai/docs/proxy/users)

**Best-fit users:** one developer, learning projects, hobby or independent work with a real spending ceiling, willingness to tolerate an occasional “no eligible model” result, and repositories suitable for the selected providers' data policies.

**Poor-fit users:** people who need the strongest available model regardless of price, zero-maintenance inference, guaranteed free capacity, unsupported harness APIs, or sensitive code whose provider/privacy requirements cannot be verified. Enterprise tenancy, billing and governance should not drive this project's near-term design.

## Economics should be measured per completed task

Use this objective:

> Minimize total cost of accepted coding outcomes, subject to the chosen quality floor, price ceilings, latency tolerance and data policy.

Token prices are only inputs. The bill includes uncached input, cache reads/writes, billed reasoning/output, provider tool charges, failed attempts and retries. Long tool schemas, repository context, repeated tool results and compaction summaries all matter. A lower-price model may need more turns or leave more repair work. Prompt cache discounts depend on actual reuse and provider behavior, not merely choosing the same provider.

### Illustrative arithmetic rather than measured performance

Assume a task uses a cumulative 1M input tokens and 50K billed output tokens across calls, each with at most 272K input. Exclude cache writes, external tool fees, transaction fees and taxes. These token volumes are an example, not an observed PennyRouter workload.

| Route | All input uncached | 90% of input charged as cache hits |
|---|---:|---:|
| Luna Standard | $0.1250 | $0.0440 |
| Flash off-peak | $0.1800 | $0.0477 |
| Flash peak | $0.3600 | $0.0954 |

The calculations use the direct-provider rates above. They do **not** establish which model completes the task, uses fewer tokens, or gets that cache-hit ratio. They show why “lowest input price” and “best completed-task cost” can differ, and why tiny absolute savings can be outweighed by another retry or several minutes of manual repair.

Measure the following separately:

- Total billed spend divided by accepted tasks, counting spend on failed tasks too.
- First-attempt completion, final completion, tool-call validity, unnecessary edits and regressions.
- Task latency and human repair time, without inventing an hourly value for the user.
- Free-route success rate, wait time, quota exhaustion and paid fallback spend.
- Local runtime/hosting expense and maintenance effort.

For a hosted deployment, a simple break-even check is `fixed monthly operating cost / observed fractional API savings`. Hypothetically, $5/month of overhead needs $25/month of baseline API spend to break even at 20% savings. Neither number is measured here. For very light personal usage, local operation or no router may be cheaper.

Free-first also needs a waiting policy. An immediate paid fallback can defeat the intent; endless retrying can make the harness unusable. Honor provider backoff signals, keep retries bounded, and let the user choose whether an exhausted free pool means wait, stop, or use the approved cheap pool. OpenRouter documents account-dependent free limits and quota/error information; this review does not assume a fixed number of free requests is available to this user. [OpenRouter limits](https://openrouter.ai/docs/api_reference/limits)

## A focused routing policy

The following is a **proposed behavioral contract**, not syntax already implemented in PennyRouter:

1. Accept a coding route such as `penny/coding-free-first`; distinguish it from explicit passthrough.
2. Resolve required API format, tools, effective context, output allowance, privacy and approved models.
3. Exclude candidates with unknown or stale critical price/capability facts. A missing quality score is unknown, not evidence of excellence.
4. Prefer an eligible, available, genuinely free offer that passed the coding gate.
5. If none is usable, apply the user's wait/stop/cheap-fallback setting. A paid fallback must remain inside every configured ceiling.
6. Reserve a conservative maximum request cost against the remaining task and aggregate budgets before forwarding. Include concurrent and retried requests; reconcile actual usage afterwards.
7. Preserve the selected model/endpoint within a conversation while it remains eligible. Switch at a safe turn boundary and disclose the reason.
8. If no candidate satisfies both quality and cost, return “no eligible route.” Do not quietly lower the quality bar or choose a premium model.

Budget reservation is not magic invoice control. It needs enforceable output limits, correct price units and tier rules, durable accounting, and conservative treatment of missing usage or interrupted streams. Keep an upstream per-key spending cap as an independent backstop where supported. Unknown final usage must not immediately release the entire reservation as if the call were free.

Use a numeric money representation suitable for billing decisions rather than letting floating-point rounding determine a hard cap. Record the price source, timestamp, currency, units, provider endpoint, promotion expiry and service tier. A single blended ranking score is not enough to enforce a multidimensional price band.

### Establish the coding floor cheaply

Start with a curated pool and repeatable tasks from the owner's real languages and repositories: a small bug fix, a multi-file change, tests, refactoring, repository search and a long-context continuation. Add malformed/parallel tool-call and patch-fidelity tests. Use synthetic or permission-cleared code for provider evaluation.

Run each candidate through the same pinned harness, tools, prompts, budgets and test environment. Compare accepted patches, tests and manual review with the chosen Luna/Flash/Pro baseline. Repeat enough tasks to expose variability; publish counts and uncertainty. External benchmark scores are useful screening signals, not proof that a candidate reaches the user's floor in OpenCode. Do not define an invented “90% quality” threshold and present it as established.

Keep quality promotion offline. Do not send every live task to a second paid judge, fan out to several models, or escalate to a premium reviewer by default. A small benchmark suite and ordinary repository tests provide more defensible initial evidence at lower cost.

## Practical OpenCode usage

### What can be tried with the present revision

Use local/loopback access with a tightly limited upstream key and nonsensitive test tasks. Supply a real, current catalog deliberately; the supplied fixture workflow is not a live production model inventory. Verify that the gateway loaded the intended artifact rather than silently entering passthrough.

OpenCode supports a custom provider with a base URL and model definitions. A representative Chat Completions setup is below. Replace `VERIFIED_UPSTREAM_MODEL_ID` with an actual model supported by both the upstream and the loaded catalog. Add credentials through OpenCode's `/connect` flow rather than committing a key to the configuration. This example is documentation-derived and **was not run end-to-end**. [OpenCode custom providers](https://opencode.ai/docs/providers/)

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "pennyrouter": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "PennyRouter local",
      "options": {
        "baseURL": "http://127.0.0.1:3000/v1"
      },
      "models": {
        "VERIFIED_UPSTREAM_MODEL_ID": {
          "name": "Verified low-cost coding model"
        }
      }
    }
  },
  "model": "pennyrouter/VERIFIED_UPSTREAM_MODEL_ID"
}
```

The gateway's default upstream base is `https://openrouter.ai/api`; the incoming credential is therefore an OpenRouter key in that configuration, not a direct DeepSeek or OpenAI key. The client URL ends in `/v1`; the gateway upstream base does not. Verify streaming, one complete tool round trip, cancellation, model identity, provider selection and usage on a disposable task before relying on it. A successful `/health` response alone proves none of these.

Do not advertise `/privacy` as an enforced protection: the current parser ignores it. Likewise, the special `openrouter/free` route is not yet a qualified coding policy. A Responses-based provider uses a different OpenCode adapter; the current PennyRouter fallback path will forward those requests without routing them. Keep that distinction visible in onboarding.

### The intended everyday flow after the essentials are fixed

The user chooses the coding baseline, permitted providers, free-only versus free-first behavior and hard budgets once. OpenCode then uses one stable PennyRouter route. At the end of a task, a short local summary shows actual model/endpoint, why it was eligible, free or paid status, cache usage, attempts, spend and remaining budget. On quota exhaustion, the harness gets a useful explanation rather than an opaque failure or a surprise premium bill.

A small CLI/status page and a working sample configuration are sufficient. A multi-user dashboard is unnecessary for this goal.

## Prioritized changes and acceptance criteria

### First make the promise safe

**1. Preserve and enforce policy across every inference path.** Fix provider-object replacement and exact endpoint enforcement. Add explicit strict and passthrough modes; strict routes reject unavailable policy instead of failing open. Apply the same policy to supported Chat Completions and Responses flows, or reject unsupported flows clearly. **Accept when:** tests show no provider, price, privacy, context or capability restriction is weakened by routing, fallback, reload or an unknown model.

**2. Make coding streams and tool loops faithful.** Repair response-header/compression handling and truncated-stream error signaling from the security review. Add harness-level tests for tool-call fragments, reasoning continuation, long patches, cancellation and upstream errors. **Accept when:** representative supported OpenCode tasks behave like the direct upstream, including failure visibility, with no missing or duplicated tool execution.

**3. Add hard budget enforcement and a small approved model pool.** Do not wait for a sophisticated ranker. Reserve maximum bounded request costs, reconcile usage and expose the decision. **Accept when:** concurrent requests, retries, missing usage, process restart and price changes cannot silently authorize spend outside policy; accounting uncertainty remains visible and conservative.

### Then make free-first routing useful

**4. Publish and reload fresh eligibility facts.** Fix promo eligibility/expiry, startup ordering, last-good recovery and actual background refresh. Free facts expire independently of rank ordering. **Accept when:** a long-running process admits an activated offer, rejects it at expiry, handles corrupt updates, and never treats stale unknown prices as zero.

**5. Establish an evidence-based coding floor.** Add explicit capability metadata and a task-level evaluation record for each approved model/endpoint/mode. Correct missing-score ordering. **Accept when:** untested or incompatible free models cannot enter the default coding pool; comparisons disclose task counts, failures and cost, without claiming universal model equivalence.

**6. Fix conversation affinity and bounded recovery.** Use an explicit session ID, pin the model as well as endpoint, expire entries and invalidate them when policy changes. Distinguish retriable availability errors from invalid requests or exhausted budgets. **Accept when:** separate projects do not share pins, a free expiry cannot preserve a paid pin, and no retry starts after partially delivered tool output without an explicit safe recovery path.

**7. Prove the personal benefit.** Compare the full workflow with a fixed cheap model, native free options and an OpenRouter baseline over representative real tasks. **Accept when:** it reduces cost or manual switching while meeting the chosen quality/latency tolerance, with the evidence and failures recorded. If it cannot, simplify to a policy/configuration helper rather than continuing to build routing machinery.

### Only expand where the evidence supports it

Consider a direct DeepSeek/OpenAI adapter only when actual prices, cache behavior or access restrictions show a worthwhile advantage over the existing upstream. Direct multi-provider access changes credential handling and API compatibility; it is not a trivial catalog extension. Keep secrets out of logs and repository configuration.

Defer enterprise tenancy, centralized billing, marketplace features, broad cloud deployment expansion, live LLM judges, automatic multi-model fan-out and a large classifier. Extreme Rust optimization is a separate engineering review; it should not delay budget and coding correctness. At personal scale, a simpler process and fewer failure modes can matter more than shaving microseconds off lookup time.

### Security priorities under this narrower scope

Policy preservation, trustworthy cost decisions, clean streaming and private credential handling remain immediate because they affect one person directly. Bind locally by default, redact query values, and bound upload/response size, upload time and concurrency. Public gateway hardening becomes more urgent only if network exposure is intended. The previous review's public-exposure severity should not be mistaken for a requirement to build an enterprise platform.

## Suggested project description

> PennyRouter is a personal model router for coding agents. It aims to use free models when they meet your tested coding requirements, then fall back only to low-cost models inside your chosen budget. It keeps compatible sessions stable and makes routing and spending visible. The current implementation provides an OpenRouter-backed gateway, provider selection and an experimental free-model route; strict budget policies, coding qualification and live eligibility refresh are still being completed.

This wording separates intent from shipped functionality. Replace “every token at its best price” and continuously live ranking claims with a narrower claim until the evidence supports them. Update the README, vision, API and operations documents together so a user can tell what is supported today.

## Review limits

This assessment used the pinned source, repository documentation, the earlier security review and current official provider/harness documentation. It did not run PennyRouter, install dependencies, execute repository scripts, make paid inference calls, test OpenCode integration, survey users, or measure coding quality, savings, CPU, memory or latency. The only numeric execution was independent arithmetic for the labeled cost example.

Prices and offers are snapshots, not guarantees. Direct-provider and intermediary prices can differ; eligibility may depend on geography, account, terms, model version and API path. A provider's capability or benchmark claim is not an independent quality measurement. No repository changes, PRs, purchases or external publication were made.

**Bottom line:** build the smallest trustworthy personal coding policy that can beat a fixed cheap model on the owner's actual work. The project has useful foundations. Its next value comes from hard spending boundaries, compatible tool loops, fresh free eligibility and honest task-level evidence, rather than more gateway breadth.
