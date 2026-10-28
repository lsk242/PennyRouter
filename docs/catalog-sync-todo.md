# Catalog Sync TODO

Iteration checklist for the sync process. Detail and acceptance criteria
live in [catalog-sync-roadmap.md](catalog-sync-roadmap.md). Gateway
milestones stay in [../TODO.md](../TODO.md).

- [x] **Phase 1 — Gathering.** `catalog-sync` binary pulls fixture data
      into the DB (fixed core columns + JSON payload) and publishes a
      versioned artifact. Test: fixture sync → expected rows; artifact
      has version and checksum.
- [x] **Phase 2a — Processing (deterministic).** Job distills payloads
      into fixed indexed columns with pure-code extraction (normalized
      price parts, precision class, promo discount). Test: processing
      fixtures yield expected indexed values; raw payloads preserved.
- [x] **Phase 2b — Processing (AI-assisted).** Model-judged signals fill
      further indexed columns. Test: recorded-output fixtures yield
      expected values; no live model calls in tests.
- [x] **Phase 3 — Local deployment.** Containerized scheduled sync with
      volume-shared artifact (gateway read-only) + CI gates. Test:
      image builds; one-shot smoke publishes a readable artifact;
      compose renders.
- [ ] **Phase 4 — Judge backends: Baseten + Modal.** Live
      `Judge` backends behind the Phase 2b trait (Baseten + Modal
      models judging OpenRouter offerings); recorded-output fixtures
      stay the test path. Test: recorded fixtures yield expected
      values with no live calls; live backends opt-in via env keys
      only; gates pass.
