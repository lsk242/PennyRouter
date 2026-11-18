# Review method

Standing rules for multi-agent project reviews (from D10, 2026-11-03:
the synthesis input once marked all scopes incomplete while the full
handoffs were complete).

## Finding rubric (Q18, header template for future review docs)

Every finding carries one severity, quoted from this rubric — never
an ad-hoc label. Copy this block into the review header.

- **Critical**: exploitable now, data loss, silent wrong billing, or
  a hang/panic on admitted input. Fix before anything else.
- **High**: reachable failure with real impact (wrong route, failed
  closed where it must serve, money/trust moved). Needs an iteration.
- **Medium**: hardening or product gap with a bounded fix; docs
  overstating what ships.
- **Low**: polish, docs-only, or deferred-with-reason.

Dedup by root cause: one finding per root cause. Twins reference the
primary (`Fixed as X` / `Twin of Y`) instead of duplicating text,
and the merge step diffs counts per scope before publishing.

- Synthesis consumes the full per-scope files (or refs), never
  summaries alone.
- The synthesizer diffs finding counts (B/C/P/D/I/S/U + decisions)
  between scope handoffs and the merged report before publishing.
- Every review file opens with a method note stating full-text
  synthesis and the count check.
