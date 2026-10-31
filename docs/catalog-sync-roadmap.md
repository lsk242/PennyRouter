# Catalog Sync Roadmap

Phased plan for the sync process. Each phase ships one small layer:
testable by itself, valuable by itself. No phase may depend on a later
one. Gateway-side milestones stay in [roadmap.md](roadmap.md).

## Phase 1 — Gathering

- Goal: `catalog-sync` pulls models/providers from OpenRouter into the
  catalog DB: fixed core columns plus JSON payload columns preserving
  the raw source data. Publishes a versioned DB artifact.
- Value: the raw data foundation exists and is inspectable; later phases
  and the gateway build on a stable artifact.
- Test: fixture sync populates the DB; queries return expected rows;
  artifact carries version and checksum.

## Phase 2a — Processing (deterministic)

- Goal: a processing job distills raw payloads into fixed indexed columns
  with pure-code extraction (normalized price parts, precision class from
  quantization, promo discount) — the queryable projection routing and
  ranking read.
- Value: stable, comparable facts for ranking without losing any raw
  input; the fixed schema grows only with proven index needs.
- Test: processing fixtures yield the expected indexed values; raw
  payloads preserved.

## Phase 2b — Processing (AI-assisted) (done)

- Goal: model-judged signals (e.g. quality flags) fill further indexed
  columns behind the hook Phase 2a establishes.
- Value: judgment-based facts that pure extraction cannot produce.
- Test: fixtures with recorded model outputs yield the expected indexed
  values; no live model calls in tests.
- Landed: `Judge` trait + `RecordedJudge` (validated JSON: tier
  high/standard/low, score 0–1, tag-narrowed matching) fill
  `quality_tier`/`quality_score`/`judged_at` (schema v3) via
  `catalog-process --judge`; `cargo test`, clippy, and fmt pass.

## Phase 3 — Local deployment

- Goal: the sync runs containerized on a schedule, sharing its DB
  artifact with the gateway through a mounted volume (gateway
  read-only); CI gates the Rust workspace and the catalog image.
- Value: the deployment shape is proven early — image, schedule,
  volume contract — before live data and gateway consumption land.
- Test: catalog image builds; one-shot container publishes an artifact
  a read-only mount can see; compose file renders.

## Phase 4 — Judge backends: Baseten + Modal (done)

- Goal: add the first live `Judge` backends behind the trait Phase 2b
  establishes: models served on Baseten (OpenAI-compatible chat at
  `https://inference.baseten.co/v1`, hackathon model
  `meta-models/muse-spark-open-1026`) and Modal (endpoint shape TBD
  from its docs). They judge OpenRouter catalog offerings into the
  same indexed columns (`quality_tier`/`quality_score`/`judged_at`)
  — OpenRouter stays the only catalog source; Baseten/Modal are
  processing LLMs, not onboarded sources.
- Value: real model judgments replace hand-written recordings in
  production while tests stay offline and deterministic; the catalog
  proves pluggable judge backends early, before the index needs them.
- Test: recorded-output fixtures still yield the expected indexed
  values with no live model calls; each live backend ships a
  recorded fixture plus a redacted config guide; live calls are
  explicit opt-in via env keys only (never in config, fixtures, or
  repo); workspace gates pass.
- Steps:
  - Baseten judge: request/response shape, prompt + output
    validation (tier high/standard/low, score 0–1), recorded
    fixture, redacted config guide in `docs/` if setup is needed.
  - Modal judge from its docs (endpoint shape, models, auth);
    same recorded fixture → validation → test flow; config guide
    if setup is needed.
  - Backend selection for `catalog-process` (recorded stays the
    default; live backends are explicit opt-in); same-input
    determinism per the `Judge` contract.
- Not in this phase: new catalog sources (no Baseten/Modal
  model/endpoint rows), gateway use (single OpenRouter upstream
  unchanged), multi-backend routing (still deferred, see
  [future.md](future.md)).
- Landed: `BasetenJudge` + `ModalJudge` behind the Phase 2b trait over
  a shared OpenAI-chat core (temperature 0, JSON-object replies, same
  tier/score validation as recorded); `catalog-process
  --judge-backend baseten|modal` with recorded still the default;
  recorded fixtures per backend; live smoke judged 4/4 fixture
  endpoints on each backend with real varied verdicts; `cargo test`,
  clippy, and fmt pass.

## Phase 5 — Live fetching (done, landed as gateway Iter 18)

- Goal: `catalog-sync --fetch-openrouter` pulls the public `GET
  /models` list plus one per-model endpoints body (no auth, no keys)
  through the unchanged fixture boundary; `FETCH_OPENROUTER=1`
  flips the container off fixtures.
- Value: routing decides over live data instead of three fixture
  models; fixtures stay the offline default for tests and local work.
- Test: stub-server fetch tests; no live calls in tests. Live
  one-shot synced 456 models / 1483 endpoints with 18 zero-price
  offerings.

## Later

Further sources, remote artifact delivery, retention policy —
pulled forward deliberately, never by default.
