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
atomically; a failed or stale sync never breaks serving. Initial
delivery can be a local file copy; remote hosts later.

## Operations

- Own schedule (periodic); independent deploy and scaling.
- Reads only public source endpoints — no API keys in config or repo.
  (Gateway request keys still travel with requests; the sync needs none
  of them.)
- Never serves gateway traffic; never shares a process with the gateway.

## Repo layout

```text
crates/
  catalog/              # schema + queries (lib, shared with gateway readers)
                        # catalog-sync binary (this process)
```
