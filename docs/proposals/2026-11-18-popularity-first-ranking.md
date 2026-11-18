# Proposal: Popularity-First Ranking Refactor

**Date:** 2026-11-18
**Status:** Proposed
**Authors:** leandro

## Executive Summary

This proposal rearchitectures the catalog and ranking system to prioritize **popularity by token usage** (focused on coding apps) as the primary signal, with **price segmentation** as the secondary filter. The goal is to make the routing more responsive to what developers actually use, rather than purely optimizing on theoretical cost/quality metrics.

## Current System (As of Iter 37)

The current ranking system orders providers by:
1. Quality tier (high/standard/low from AA indexes or scoring)
2. Error rate (from historical trends)
3. Cost (effective cost under balanced workload)
4. Latency
5. Provider identity (tie-breaker)

**Strengths:**
- Deterministic, offline-rebuildable
- Multi-signal with fallback chains
- Free offer tracking with expiry

**Limitations:**
- Popularity data (OpenRouter rankings) is collected but only used for the `openrouter/<slug>` menu pins — not for routing decisions
- The ranking doesn't adapt to real usage patterns
- No distinction between "coding-heavy" models (used by Cline, Roo, Cursor, etc.) and general models

## Proposed Architecture

### 1. Primary Signal: Popularity by Tokens (Coding Focus)

**Data Source:** OpenRouter `/api/v1/datasets/rankings-daily`

**Process:**
1. Fetch daily rankings (trailing 7-day window)
2. Filter to **coding apps** using a curated allowlist:
   - Cline, Roo Code, Cursor, Continue, Zed, Aider, GitHub Copilot, Windsurf, etc.
   - Configurable via `CODING_APPS` env or `coders.json` fixture
3. Aggregate tokens per model across coding apps
4. Compute **popularity score** per model:
   ```
   popularity_score(model) = coding_tokens(model) / total_coding_tokens
   ```

**Coding App Allowlist (initial):**
```json
{
  "coding_apps": [
    "Cline",
    "Roo Code", 
    "Roo",
    "Cursor",
    "Continue",
    "Zed",
    "Aider",
    "GitHub Copilot",
    "Windsurf",
    "Codeium",
    "Tabnine",
    "Amazon Q",
    "CodeWhisperer"
  ]
}
```

**Why This Works:**
- Coding apps represent the primary use case (developer productivity)
- Token usage is a strong signal of real-world adoption
- Daily refresh keeps rankings current
- Fallback: if no coding app data available, use all-model rankings

### 2. Secondary Signal: Price Segmentation

Once popularity is established, segment the top models:

**Tiers:**
- **low**: Cheapest models in top quartile by popularity
  - Use: `openrouter/pair/low` 
  - Target: Budget-conscious developers, hobby projects
  - Strategy: Minimize cost while maintaining quality floor

- **think**: Highest-quality models in top decile by popularity  
  - Use: `openrouter/pair/think`
  - Target: Production systems, complex reasoning
  - Strategy: Maximize capability, accept higher cost

**Implementation:**
```rust
pub struct PopularityFirstRanking {
    pub models: Vec<ModelPopularity>,
    pub low_tier: Vec<String>,   // cheapest top-quartile
    pub think_tier: Vec<String>, // highest-quality top-decile
    pub long_tail: Vec<String>,  // everything else
}

pub struct ModelPopularity {
    pub model_id: String,
    pub coding_tokens: u64,
    pub rank: u32,
    pub tier: Tier,  // Low | Think | LongTail
    pub cost_per_m: Option<f64>,
    pub quality_tier: Option<String>,
}
```

### 3. Long Tail: Minimum Value Model

For models outside the top popularity tiers:
- Route to **minimum value model cheapest of all per type**
- This ensures we always have a fallback even for obscure models
- Per-type means: chat, completion, decision, etc.

**Example:**
```
openrouter/pair/model/rare-model-v2  →  routes to cheapest eligible provider
openrouter/pair/decision             →  routes to cheapest decision-capable model
```

### 4. Data Flow

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│ OpenRouter      │────▶│ Popularity       │────▶│ Routing         │
│ Rankings API    │     │ Aggregator       │     │ Decision        │
│ (daily)         │     │ (coding focus)   │     │ (popularity-    │
└─────────────────┘     └──────────────────┘     │  first)         │
                                                  └─────────────────┘
                                                           │
                                                           ▼
                        ┌─────────────────┐     ┌──────────────────┐
                        │ Popularity      │     │ Gateway          │
                        │ Artifact        │────▶│ (serves          │
                        │ (ranking.json)  │     │  ordering)       │
                        └─────────────────┘     └──────────────────┘
```

### 5. Reliability & Fallbacks

**Data Reliability:**
- Popularity data is CC-BY licensed from OpenRouter
- Falls back gracefully: if rankings API fails, use last-known-good
- If no popularity data: fall back to current quality-first ranking

**Freshness:**
- Sync every 24 hours (configurable)
- Ranking artifact includes `as_of` timestamp
- Gateway rejects stale rankings (older than 2× sync interval)

**Failure Modes:**
| Scenario | Behavior |
|----------|----------|
| Rankings API down | Use last-good popularity data |
| No coding app data | Use all-model popularity |
| No popularity data at all | Fall back to current quality-first |
| Stale artifact | Gateway keeps serving last-good |

### 6. Migration Path

**Phase 1: Data Collection (No breaking changes)**
- Add `CODING_APPS` config
- Extend `popular.rs` to compute coding-focused popularity
- Store in catalog meta (new keys, old ignored)

**Phase 2: Ranking Integration**
- Add `popularity_first` ranking orientation
- Gateway reads new ranking, falls back to current if absent
- No breaking changes to existing virtual IDs

**Phase 3: Default Switch (after validation)**
- Make `popularity_first` the default for `low` and `think`
- Monitor metrics: error rates, latency, user complaints
- Rollback plan: config flag to revert to old ranking

### 7. Metrics & Observability

**New Metrics:**
```
pennyrouter_popularity_models_total{model, tier}
pennyrouter_popularity_requests_total{result}
pennyrouter_popularity_tokens_total{model}
```

**UI Additions:**
- `/ui` Overview: Top 10 coding-popular models
- `/ui` Models: Popularity column per model
- `/ui` Inspector: Popularity-aware routing preview

### 8. Testing Strategy

**Unit Tests:**
- Coding app filter logic
- Popularity score calculation
- Tier assignment (low/think/longtail)
- Fallback chains

**Integration Tests:**
- Rankings API mock → popular.rs → artifact
- Gateway reads artifact → routes correctly
- Stale artifact → falls back

**Live Validation:**
- One-shot: compare popularity-first vs current routing
- A/B test: 10% traffic → popularity-first, measure error/latency

## Open Questions

1. **Coding app list maintenance**: Who maintains the allowlist? Auto-detect new apps?
2. **Token vs request count**: Tokens better reflects usage, but requests might better reflect adoption
3. **Time window**: 7 days current; 30 days might be more stable for ranking
4. **Long tail policy**: Cheapest-per-type vs popularity-tier fallback?

## Implementation Plan

### Sprint 1 (2-3 days)
- [ ] Extend `popular.rs` with coding app filter
- [ ] Add `CODING_APPS` config (env + file)
- [ ] Unit tests for popularity aggregation

### Sprint 2 (2-3 days)  
- [ ] Add `popularity_first` ranking orientation
- [ ] Update `rank.rs` to produce new ordering
- [ ] Integration tests

### Sprint 3 (1-2 days)
- [ ] Gateway: read new ranking, fallback handling
- [ ] UI updates
- [ ] Metrics

### Sprint 4 (1 day)
- [ ] Live validation / A/B test
- [ ] Documentation
- [ ] Default switch decision

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Coding apps not representative | Allowlist is configurable; can include more apps |
| Popularity data stale | Daily sync; stale detection + fallback |
| Long-tail models get poor routing | Min-value fallback ensures always-cheapest |
| Performance regression | Offline computation; gateway reads single lookup |

## Success Criteria

1. **Top 10 coding models** correctly identified with >90% accuracy vs manual review
2. **Routing latency**: No increase (offline computation)
3. **Error rates**: Maintain or improve vs current system
4. **User feedback**: Positive on "right model for the job"

## References

- [OpenRouter Rankings API](https://openrouter.ai/docs/api-reference/overview#rankings)
- [Current Routing Documentation](docs/routing.md)
- [Current Popularity Implementation](crates/catalog/src/popular.rs)
- [Ranking System](crates/catalog/src/rank.rs)