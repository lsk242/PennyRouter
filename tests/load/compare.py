#!/usr/bin/env python3
"""Compare direct-vs-gateway workload summaries into a report.

Usage:
  compare.py direct.json gateway.json --output-dir DIR [--label TEXT]

Writes report.json and report.md. The report is the deliverable: it states
the measured overhead without gating on an arbitrary budget.
"""

import argparse
import json
from pathlib import Path

ROWS = [
    ("requests", "requests", "{:.0f}", False),
    ("rps", "requests_per_second", "{:.1f}", False),
    ("error_rate", "error_rate", "{:.4f}", False),
    ("mean", "latency_ms.mean", "{:.2f}ms", True),
    ("p50", "latency_ms.p50", "{:.2f}ms", True),
    ("p95", "latency_ms.p95", "{:.2f}ms", True),
    ("p99", "latency_ms.p99", "{:.2f}ms", True),
    ("ttfb_p50", "ttfb_ms.p50", "{:.2f}ms", True),
]


def lookup(summary, dotted):
    value = summary
    for part in dotted.split("."):
        value = value[part]
    return value


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("direct")
    parser.add_argument("gateway")
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--label", default="")
    args = parser.parse_args(argv)

    direct = json.loads(Path(args.direct).read_text())
    gateway = json.loads(Path(args.gateway).read_text())
    table = []
    for name, key, fmt, has_overhead in ROWS:
        first, second = lookup(direct, key), lookup(gateway, key)
        row = {"metric": name, "direct": first, "gateway": second}
        if has_overhead and first is not None and second is not None:
            row["overhead"] = second - first
        table.append(row)

    def over(key):
        row = next(r for r in table if r["metric"] == key)
        value = row.get("overhead")
        return f"{value:+.2f}ms" if value is not None else "n/a"

    report = {
        "label": args.label,
        "mode": direct.get("mode"),
        "sessions": direct.get("sessions"),
        "duration_seconds": direct.get("duration_seconds"),
        "direct": args.direct,
        "gateway": args.gateway,
        "table": table,
        "verdict": (
            f"gateway overhead: p50 {over('p50')}, "
            f"p99 {over('p99')}, ttfb p50 {over('ttfb_p50')}; "
            f"errors direct={lookup(direct, 'error_rate'):.4f} "
            f"gateway={lookup(gateway, 'error_rate'):.4f}"
        ),
    }
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    (output_dir / "report.json").write_text(json.dumps(report, indent=2) + "\n")

    lines = [
        f"# Gateway overhead report ({report['mode']})",
        "",
        f"sessions: {report['sessions']}, duration: {report['duration_seconds']}s"
        + (f", {args.label}" if args.label else ""),
        "",
        "| metric | direct | gateway | overhead |",
        "| --- | --- | --- | --- |",
    ]
    def cell(value, fmt):
        return fmt.format(value) if value is not None else "n/a"

    for name, key, fmt, has_overhead in ROWS:
        row = next(r for r in table if r["metric"] == name)
        cells = [name, cell(row["direct"], fmt), cell(row["gateway"], fmt)]
        if has_overhead and row.get("overhead") is not None:
            cells.append(f"{row['overhead']:+.2f}ms")
        else:
            cells.append("—")
        lines.append("| " + " | ".join(cells) + " |")
    lines += ["", report["verdict"], ""]
    (output_dir / "report.md").write_text("\n".join(lines))
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
