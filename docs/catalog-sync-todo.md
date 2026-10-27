# Catalog Sync TODO

Iteration checklist for the sync process. Detail and acceptance criteria
live in [catalog-sync-roadmap.md](catalog-sync-roadmap.md). Gateway
milestones stay in [../TODO.md](../TODO.md).

- [ ] **Phase 1 — Gathering.** `catalog-sync` binary pulls fixture data
      into the DB (fixed core columns + JSON payload) and publishes a
      versioned artifact. Test: fixture sync → expected rows; artifact
      has version and checksum.
- [ ] **Phase 2 — Processing.** AI-assisted job distills payloads into
      fixed indexed columns (normalized prices, precision class, quality
      signals). Test: processing fixtures yield expected indexed values;
      raw payloads preserved.
