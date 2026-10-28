# Catalog Sync Architecture

The catalog sync is a separate process from the gateway: own binary,
schedule, and host — it may run on a different machine entirely. It
gathers provider/model data, writes the catalog DB, and ships the DB
file to each gateway host. It serves no traffic.

Contract with the gateway (see [architecture.md](architecture.md)): the
versioned DB artifact is the only shared surface. The gateway atomically
swaps in each new copy and rebuilds its in-memory snapshot; it never
writes the catalog.

## Pipeline

```text
OpenRouter (later: other sources)
  │  gather raw provider/model data
  ▼
validate + normalize ──► write catalog DB ──► publish versioned artifact
                                                     (DB file + version/sha)
ship to gateway host(s) ──► atomic local swap ──► snapshot rebuild
```

Gathering and processing are separate phases (see
[catalog-sync-roadmap.md](catalog-sync-roadmap.md)): raw data lands
first; indexed facts are distilled afterwards. The gateway never waits
on either — it keeps serving the last good copy.

## Schema: fixed index over fluid payload

Sources change constantly, so the schema is two-tier:

- **Fixed core columns** (stable identity + bookkeeping): model id,
  provider, source, fetched-at timestamps. Small and rarely changing.
- **Flexible JSON payload columns** preserving the raw source data, so
  nothing is lost when a source adds or renames fields.
- **Fixed indexed columns**, added by the processing phase: normalized
  prices, precision class, quality signals, promo windows — the stable,
  queryable projection routing and ranking read.

The fixed schema grows only with proven index needs; the payload keeps
everything else.

## Engine (proposed: SQLite)

A single-file DB keeps cross-machine sync a plain file copy, needs no
server to operate, and stays queryable (JSON1) for payload inspection.
This is a proposal to confirm in review, not a locked choice.

## Versioning and delivery

Each publish is a versioned artifact: DB file plus version and checksum.
Gateway hosts fetch (or receive) the artifact, verify it, and swap it in
atomically; a failed or stale sync never breaks serving. Local delivery
is a mounted volume (`catalog-data`) the gateway mounts read-only;
remote hosts later.

## Operations

- Own schedule (periodic); independent deploy and scaling.
- Gathering reads only public source endpoints — no API keys in config
  or repo. Live judge backends (Phase 4) take keys from env vars only,
  never from config, fixtures, or repo. (Gateway request keys still
  travel with requests.)
- Never serves gateway traffic; never shares a process with the gateway.

## Deployment

- Image: `docker/Dockerfile.catalog` ships `catalog-sync` and
  `catalog-process` on debian-slim. One cycle by default; set
  `SYNC_INTERVAL` (seconds) to loop on a schedule.
- Local compose (`compose.yaml`): the `catalog-sync` service writes to
  the shared `catalog-data` volume, which the gateway mounts read-only.
  Input is the fixture tree mounted at `/input` until live fetching
  lands. Prod compose stays gateway-only; remote catalog delivery is a
  later step.
- CI (`rust.yml`): workspace test/fmt/clippy gate plus a catalog image
  build with a one-shot sync smoke test.

## Repo layout

```text
crates/
  catalog/              # schema + queries (lib, shared with gateway readers)
                        # catalog-sync binary (this process)
```
