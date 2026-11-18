# Proposal: Artificial Analysis index ingestion — `benchmarks.artificial_analysis.*`

> Date: 2026-11-03 · Author: freeclaw · Status: Implemented (Iter 31).
> As-built deviations from the draft: values round from floats (§2,
> §4.1); `ModelRow` untouched — `ModelPayload` + `aa_coding_map()`
> instead (§4.2); UI shows the index in the model header, not a
> column (§5); first consumer is judging scope, not minindex (§8).
> Depends on: `upstream/main` @ `a79e5a8` — schema v5, Iter 28 pinned-model routes.
> Companion: `docs/proposals/2026-11-03-minindex-filter.md` (gateway `minindex` suffix — this doc is the catalog prerequisite).

## TL;DR

OpenRouter's `GET /v1/models` already returns `benchmarks.artificial_analysis.{intelligence_index, coding_index, agentic_index}` per model (verified live: `451 total, 248 with benchmarks` on 2026-11-03; e.g. `intelligence_index: 51`). The catalog persists model payloads but never indexes these fields, so the gateway cannot filter or rank on them. Add three nullable indexed columns to `models`, extract them deterministically in `process.rs`, and expose them to the gateway snapshot.

No new fetch path, no judge, no ranking change in this iteration — just make the signal queryable.

## 1. Why

- Users want "cheapest model above coding quality X" (`/minindex/[number]` — companion proposal). Without an indexed column the gateway would have to JSON-parse every model payload per request, violating the realtime contract (`docs/routing.md`: one local lookup, no heavy work).
- The signal is public, stable, and already fetched (`GET /v1/models` + `EXTRA_MODEL_IDS` via `fetch.rs`). We only need to distill it.
- Coverage is real: `248/451` models carry `benchmarks`, of which `~163` have non-null `coding_index` (snapshot 2026-11-02). Bare catalog inspection today shows nothing (`grep -rn benchmarks crates/` is empty) — the payload is opaque.

## 2. Source of truth

Per `https://openrouter.ai/openapi.json` (server `https://openrouter.ai/api/v1`) and live `GET /v1/models`:

```json
{
  "id": "openai/gpt-6.1-terra",
  "benchmarks": {
    "design_arena": [],
    "artificial_analysis": {
      "intelligence_index": 51,
      "coding_index": null,
      "agentic_index": null
    }
  }
}
```

- `intelligence_index`, `coding_index`, `agentic_index` are `number | null` (observed range `0..100`, treat as `0..100` inclusive; out-of-range reads as `None`/unknown, never clamped into range — a bogus `999` must not sort first). Live values mix integers and floats (observed `64.6`); floats round to the nearest integer.
- `design_arena[]` is `array of {arena, category, elo, win_rate, rank}` — out of scope for this iteration (ELO, not a single threshold).
- Dedicated `GET /benchmarks` endpoint exists with `source`/`task_type` filters — not fetched now; if needed later, it's a new gather step, not this proposal.
- Variants (`:batch`, `:free`, `:nitro`) carry the same `benchmarks` object as their base id — no special casing.

## 3. Schema change — v5 → v6

### 3.1 Where

`benchmarks` is per-model (same value for all of a model's endpoints). It belongs on `models`, not `endpoints`. `crates/catalog/src/schema.rs` today has no indexed columns on `models` — only `endpoints` has `ENDPOINT_INDEXED_COLUMNS`. This iteration introduces the pattern for `models`.

```rust
// crates/catalog/src/schema.rs
pub const SCHEMA_VERSION: u32 = 6;

pub const DDL: &str = "
CREATE TABLE IF NOT EXISTS models (
    model_id    TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    source      TEXT NOT NULL,
    payload     TEXT NOT NULL,
    fetched_at  TEXT NOT NULL,
    aa_intelligence_index INTEGER,
    aa_coding_index       INTEGER,
    aa_agentic_index      INTEGER
);
... endpoints unchanged ...
";

/// Indexed columns on `models` — mirrors ENDPOINT_INDEXED_COLUMNS pattern.
pub const MODEL_INDEXED_COLUMNS: &[(&str, &str)] = &[
    ("aa_intelligence_index", "INTEGER"),
    ("aa_coding_index",       "INTEGER"),
    ("aa_agentic_index",      "INTEGER"),
];
```

### 3.2 Migration

`Catalog::migrate()` (`crates/catalog/src/store.rs`) already loops over `ENDPOINT_INDEXED_COLUMNS` with `has_column` + `ALTER TABLE ADD COLUMN`. Extend it to also loop over `MODEL_INDEXED_COLUMNS` against `models`. Idempotent, covers v1..v5 DBs alike. Fresh DBs get the columns from `DDL` directly.

```rust
for (name, ty) in ENDPOINT_INDEXED_COLUMNS { /* existing */ }
for (name, ty) in MODEL_INDEXED_COLUMNS {
    if !self.has_column("models", name)? {
        self.conn.execute_batch(&format!("ALTER TABLE models ADD COLUMN {name} {ty}"))?;
    }
}
```

No data backfill needed beyond next `run_processing` pass — old rows keep `NULL` until reprocessed.

## 4. Processing — `crates/catalog/src/process.rs`

### 4.1 Extraction

New `ModelIndex` struct (parallel to `EndpointIndex`):

```rust
pub struct ModelIndex {
    pub aa_intelligence_index: Option<i64>,
    pub aa_coding_index: Option<i64>,
    pub aa_agentic_index: Option<i64>,
}
```

New helper:

```rust
fn extract_aa_index(model_payload: &Value, key: &str) -> Option<i64> {
    let v = model_payload.get("benchmarks")?
        .get("artificial_analysis")?
        .get(key)?;
    let n = match v {
        Value::Number(n) => n.as_f64()?,
        _ => return None, // null or non-number → None (unknown)
    };
    if !n.is_finite() || !(0.0..=100.0).contains(&n) {
        return None;
    }
    Some(n.round() as i64)
}
```

Called per model in `run_processing`:

```rust
pub fn run_processing(catalog: &Catalog, processed_at: &str) -> Result<ProcessSummary, Error> {
    // existing endpoint loop ...
    // new: model loop
    for model in catalog.all_models()? {
        let payload: Value = serde_json::from_str(&model.payload)?;
        catalog.update_model_index(&model.model_id, &ModelIndex {
            aa_intelligence_index: extract_aa_index(&payload, "intelligence_index"),
            aa_coding_index:       extract_aa_index(&payload, "coding_index"),
            aa_agentic_index:      extract_aa_index(&payload, "agentic_index"),
        })?;
    }
    // ...
}
```

`EXTRA_MODEL_IDS` models (Iter 24) already have their model payload persisted via `fetch.rs`'s `fetch_openrouter_extra` (`list.push(model)`) path, so they are covered without extra fetch logic.

### 4.2 Store — `crates/catalog/src/store.rs`

- `update_model_index(&self, model_id: &str, index: &ModelIndex)` — `UPDATE models SET aa_* = ? ... WHERE model_id = ?`.
- `all_models()` returns lightweight `ModelPayload { model_id, payload }` for the model loop; `aa_coding_map()` returns the non-null coding pairs for the snapshot. `ModelRow` deliberately untouched (no caller needs the columns back on the row).

No change to `fetch.rs` — `GET /v1/models` already returns `benchmarks`; live sync on `a79e5a8` pulled `451/248` without code change.

## 5. Gateway integration

Gateway snapshot (`crates/gateway/src/routing.rs`) currently loads `models -> Vec<Offering>` from `endpoints`. For `minindex` it needs `model_id -> aa_coding_index` alongside. Add to `Snapshot`:

```rust
pub struct Snapshot {
    pub models: HashMap<String, Vec<Offering>>,
    pub aa_coding_index: HashMap<String, i64>, // model_id -> coding_index (only when Some)
    pub artifact_version: Option<String>,
}
```

Loaded at startup and on every `RELOAD_INTERVAL_SECS` reload (default 60s) from `models.aa_coding_index` where non-null. `Offering` does not need the field — the filter consults the model-level map before ranking.

`GET /ui` (when `GATEWAY_UI=1`) shows the index in each model header (`· coding N`) where known — a per-row column would repeat a model-level value on every offering row.

## 6. What not to do

- No `design_arena` ELO ingestion (different shape, per-category, needs aggregation policy — future proposal if wanted).
- No ranking-index formula change — ranking stays per `docs/ranking-index.md` (price/tier/latency/trends). AA index is a filter, not a rank input, in this iteration. A follow-up can blend it as an assist signal.
- No write-path change — still `catalog-sync` → DB artifact → gateway `reload()`.

## 7. Tests + acceptance

Unit:

- `extract_aa_index` over `null`, missing `benchmarks`, non-number, out-of-range (`-1`, `999`, `100.6`), valid ints and rounding floats (`30.6` → `31`, `51.5` → `52`).
- `migrate()` from a v5 DB (no AA columns) adds all three; idempotent.
- `run_processing` on fixture models with/without `benchmarks` writes expected `aa_*` and leaves payload untouched.

Integration:

- `catalog-sync` over fixture `models` body containing `benchmarks` publishes a DB where `SELECT aa_coding_index FROM models WHERE model_id = ?` matches the fixture JSON.
- Gateway `Snapshot::load` carries `aa_coding_index` for known models, absent for unknown.

Live smoke (optional, same bar as Iter 18):

- `catalog-sync --fetch-openrouter` against real OpenRouter: `SELECT COUNT(*) FROM models WHERE aa_coding_index IS NOT NULL` ≈ `160+` (observed `168/469` on 2026-11-10; intelligence `89`, agentic `136`).

## 8. Rollout

Single iteration — schema bump + processing + store + snapshot plumbing. No gateway behavior change: the map is display-only until a consumer lands. First consumer is judging scope (AA + popular subset); the `minindex` companion stays proposed. Deploy note: a v6 gateway rejects v5 artifacts, so the first v6 publish must land before (or with) the gateway — reloads keep last-good across the window, and a boot on v5 stays in standby passthrough until the first v6 publish.

## References

- `crates/catalog/src/schema.rs` — `SCHEMA_VERSION=5`, `DDL`, `ENDPOINT_INDEXED_COLUMNS`, `V1_DDL`.
- `crates/catalog/src/store.rs` — `Catalog::migrate`, `has_column`, `ENDPOINT_COLS`, `update_endpoint_index`, `ModelRow`, `all_endpoints`, `get_model`.
- `crates/catalog/src/process.rs` — `run_processing`, `extract_tool_support`, `extract_decision_support`, `extract_price`, `classify_precision`.
- `crates/catalog/src/fetch.rs` — `fetch_openrouter` / `fetch_openrouter_extra`, `EXTRA_MODEL_IDS`, model persistence via `list.push(model)`.
- `crates/gateway/src/routing.rs` — `Snapshot`, `Offering`, `parse_virtual`, `VIRTUAL_MODELS`.
- `docs/routing.md`, `docs/api.md`, `docs/catalog-sync-architecture.md`, `docs/catalog-sync-roadmap.md`, `docs/ranking-index.md`.
- `docs/proposals/2026-11-03-minindex-filter.md` — gateway filter depending on this signal.
- OpenRouter: `https://openrouter.ai/openapi.json` (server `https://openrouter.ai/api/v1`), `GET /v1/models`, `GET /benchmarks`, `GET /v1/models/{id}/endpoints`.
