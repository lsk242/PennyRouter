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
