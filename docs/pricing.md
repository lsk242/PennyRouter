# Pricing

Price is not a single number: input, output, and cache tokens price
differently, and the mix varies per request. Accurate cheapest-provider
selection really wants a classifier — which we are not building yet.
(Iter 7.)

## Abstraction (no classifier)

Until a classifier exists, collapse the input/output/cache relationship
into a small set of workload profiles (e.g. cache-heavy session,
balanced chat, output-heavy generation) and rank providers per profile.
The default profile should match the common case; suffix tags may select
one explicitly later.

## Promo and free handling

- Free-testing models are free only for limited windows: model them as
  time-boxed provider-scoped facts with expiry, never as permanent zeros.
- Partial-discount windows are the same shape with a multiplier.
- A generic `openrouter/free`-style request means "optimize for cost
  including free tiers" and must survive a promo expiring mid-day without
  misrouting.

## Type awareness

Routing must respect model/provider type constraints (modality, context
limits, ZDR) before comparing price — cheapest among eligible providers,
never cheapest overall. Precision counts as a type constraint too: compare
prices within a precision class, since lower precision can mean lower
quality at a lower price.

## Status (Iter 7)

- Profiles: `balanced` (50/40/5/5 input/output/cache-read/cache-write,
  the default behind every cost comparison), `cache_heavy`
  (20/10/60/10), `output_heavy` (20/70/5/5). No per-request profile
  selection yet — no classifier, no profile tags; the profiles exist
  so cost math is explicit and testable.
- Effective cost: shares blended after discount; missing
  prompt/completion prices are unpriceable (sort last); missing cache
  prices bill as input; discounts clamp to [0, 1].
- Promos file: provider-scoped `{model_id, provider_name, tag?,
  discount, starts_at, ends_at}` windows (RFC 3339, `[start, end)`),
  evaluated at rebuild; active windows override the catalog discount
  column, expired ones fall back to it. `discount: 1.0` is a free
  window. `catalog-rank --promos` consumes them; the sync container
  passes `/input/promos.json` when present.
- `openrouter/free` requests route to any zero-cost offering across
  models (majority class among them, orientation tags apply) and
  rewrite to its backend id; with nothing free they pass through
  untouched. Free-session pins re-route when the offering stops being
  free, so a mid-day expiry never misroutes at full price.
- From Iter 11, promo-free eligibility is published in the ranking
  document (`free_offers`: provider + tag + `valid_until_unix`; no
  expiry means free by permanent catalog zeros), and the gateway
  checks the wall clock: an expired or missing fact is never free,
  even mid-window-cached. Catalog zeros stay expiry-free by design.
- From Iter 13, tier comparison uses the blended effective cost
  (input/output/cache-read/cache-write under the balanced profile)
  with eligibility before price: caller restrictions, priceable, and
  known precision class. `low` = minimum effective cost;
  `think` = best quality signals (tier, then score, then errors, then
  price). FP resolution is a separate class and never substitutes
  silently — unknown-class offerings are ineligible for tiered
  routing.
- Modality/context/ZDR constraints stay out: price compares within
  precision class (the constraint the index carries); richer
  eligibility waits for the signals.
- Caller budget caps (Iter 14): `provider.max_price: {prompt,
  completion}` in the request body sets a hard ceiling compared
  against the raw per-token `price_prompt`/`price_completion`
  columns — not the blended effective cost, so the promise is
  legible without knowing the workload profile. Both legs must be
  present and numeric (JSON numbers or numeric strings); a malformed
  cap is silently no cap, never a guessed one. Under a well-formed
  cap, an offering with a missing leg is ineligible (fail closed);
  without a cap, unpriceable offerings still just sort last. Caps
  apply to every routed candidate and every sticky pin, on both
  `/v1/chat/completions` and `/v1/responses`.
