# AGENTS.md — instructions for coding agents

Read this before changing the repo.

## Source of truth

- `TODO.md` + `docs/roadmap.md`: what to build, in order. One iteration
  at a time.
- `docs/`: all product documentation, Markdown only. Never put docs
  elsewhere.
- `docs/architecture.md`: long-term design. Don't contradict it without
  discussion.

## Iteration discipline

- Each iteration is small, testable alone, and valuable alone.
- Don't pull forward deferred items from `docs/future.md` or later
  platform stages without being asked.
- Update `TODO.md` checkboxes and `docs/roadmap.md` when an iteration
  lands.

## Code rules (from Iter 1 on)

- Rust, from scratch, performance-first. No network calls on the request
  path except the single backend forward.
- Realtime path stays allocation-lean; heavy work goes to periodic jobs.
- Footprint ballpark: ~1 CPU / 1 GB; requests up to 1M tokens.
- Never commit secrets: backend keys travel with requests, never in
  config or fixtures (use redacted fixtures).

## Repo hygiene

- Keep `README.md` badges and doc links working; every new doc gets an
  entry in `docs/README.md`.
- Commit messages like `Iter N: ...`. Never commit or push unless
  explicitly asked.
