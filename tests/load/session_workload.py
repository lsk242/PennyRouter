#!/usr/bin/env python3
"""Health-only session workload for the PennyRouter load scaffold.

Each session is one worker with a persistent keep-alive connection doing
repeated GET requests with think times between them. Stdlib only.

Reads a workload file with stages, runs each stage, and writes one JSON
evidence file per stage plus a summary. Exits nonzero if any stage
exceeds its error budget (or p99 budget, when set).
"""

import argparse
import http.client
import json
import random
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from urllib.parse import urlsplit

BUCKETS_MS = [1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, float("inf")]


def percentile(sorted_samples, q):
    if not sorted_samples:
        return None
    index = min(len(sorted_samples) - 1, int((len(sorted_samples) - 1) * q))
    return sorted_samples[index]


def run_session(base, path, expected_body, deadline, think_min, think_max,
                timeout, seed, record):
    """One session: repeated GETs on a reused connection until deadline."""
    rng = random.Random(seed)
    conn = None
    while time.monotonic() < deadline:
        start = time.monotonic()
        try:
            if conn is None:
                conn = http.client.HTTPConnection(base.hostname, base.port,
                                                  timeout=timeout)
            conn.request("GET", path)
            response = conn.getresponse()
            body = response.read()
            latency_ms = (time.monotonic() - start) * 1000
            ok = response.status == 200 and body == expected_body
            record(latency_ms, response.status, None if ok else "bad_response")
        except Exception as error:  # noqa: BLE001 - counted, then retried
            record(None, 0, type(error).__name__)
            if conn is not None:
                conn.close()
                conn = None
        time.sleep(rng.uniform(think_min, think_max))
    if conn is not None:
        conn.close()


def run_stage(base_url, output_dir, stage, defaults):
    base = urlsplit(base_url)
    if base.scheme != "http":
        raise ValueError("only http base URLs are supported")
    sessions = stage["sessions"]
    duration = stage["duration_seconds"]
    think_min = stage.get("think_min_seconds", defaults["think_min_seconds"])
    think_max = stage.get("think_max_seconds", defaults["think_max_seconds"])
    timeout = stage.get("timeout_seconds", defaults["timeout_seconds"])
    max_error_rate = stage.get("max_error_rate", defaults["max_error_rate"])
    p99_budget = stage.get("p99_budget_ms", defaults.get("p99_budget_ms"))
    expected_body = defaults["expected_body"].encode()
    seed = defaults.get("seed", 12)

    import threading
    lock = threading.Lock()
    latencies = []
    statuses = {}
    errors = {}

    def record(latency_ms, status, error):
        with lock:
            statuses[status] = statuses.get(status, 0) + 1
            if latency_ms is not None:
                latencies.append(latency_ms)
            if error is not None:
                errors[error] = errors.get(error, 0) + 1

    deadline = time.monotonic() + duration
    started = time.monotonic()
    with ThreadPoolExecutor(max_workers=sessions) as pool:
        futures = [
            pool.submit(run_session, base, defaults["base_path"],
                        expected_body, deadline, think_min, think_max,
                        timeout, seed + index, record)
            for index in range(sessions)
        ]
        for future in futures:
            future.result()
    elapsed = time.monotonic() - started

    latencies.sort()
    requests = sum(statuses.values())
    error_count = sum(errors.values())
    error_rate = error_count / requests if requests else 0.0
    histogram = {str(b): 0 for b in BUCKETS_MS}
    for sample in latencies:
        for bound in BUCKETS_MS:
            if sample <= bound:
                histogram[str(bound)] += 1
                break
    result = {
        "stage": stage["name"],
        "config": {
            "sessions": sessions,
            "duration_seconds": duration,
            "think_min_seconds": think_min,
            "think_max_seconds": think_max,
            "timeout_seconds": timeout,
            "max_error_rate": max_error_rate,
            "p99_budget_ms": p99_budget,
        },
        "requests": requests,
        "errors": errors,
        "status_codes": {str(k): v for k, v in statuses.items()},
        "error_rate": error_rate,
        "requests_per_second": requests / elapsed if elapsed else 0.0,
        "elapsed_seconds": elapsed,
        "latency_ms": {
            "count": len(latencies),
            "min": latencies[0] if latencies else None,
            "mean": sum(latencies) / len(latencies) if latencies else None,
            "p50": percentile(latencies, 0.50),
            "p95": percentile(latencies, 0.95),
            "p99": percentile(latencies, 0.99),
            "max": latencies[-1] if latencies else None,
        },
        "histogram_ms": histogram,
    }
    passed = error_rate <= max_error_rate
    if p99_budget is not None and result["latency_ms"]["p99"] is not None:
        passed = passed and result["latency_ms"]["p99"] <= p99_budget
    result["pass"] = passed and requests > 0

    path = output_dir / f"{stage['name']}.json"
    path.write_text(json.dumps(result, indent=2) + "\n")
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workload", required=True,
                        help="workload JSON file with stages")
    parser.add_argument("--base-url", required=True,
                        help="gateway URL, e.g. http://localhost:3000")
    parser.add_argument("--output-dir", required=True,
                        help="directory for stage evidence files")
    parser.add_argument("--seed", type=int, default=12,
                        help="base RNG seed for think times")
    args = parser.parse_args(argv)

    from pathlib import Path
    workload = json.loads(Path(args.workload).read_text())
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    defaults = {
        "base_path": workload.get("base_path", "/health"),
        "expected_body": workload.get("expected_body", '{"status":"ok"}'),
        "think_min_seconds": workload.get("think_min_seconds", 0.1),
        "think_max_seconds": workload.get("think_max_seconds", 0.5),
        "timeout_seconds": workload.get("timeout_seconds", 10),
        "max_error_rate": workload.get("max_error_rate", 0.01),
        "p99_budget_ms": workload.get("p99_budget_ms"),
        "seed": args.seed,
    }
    results = []
    for stage in workload["stages"]:
        result = run_stage(args.base_url, output_dir, stage, defaults)
        results.append(result)
        lat = result["latency_ms"]
        print(f"{result['stage']}: sessions={result['config']['sessions']} "
              f"req={result['requests']} err={result['error_rate']:.4f} "
              f"rps={result['requests_per_second']:.1f} "
              f"p50={lat['p50']:.2f}ms p99={lat['p99']:.2f}ms "
              f"pass={result['pass']}", flush=True)
    summary = {
        "workload": workload.get("workload", "sessions"),
        "base_url": args.base_url,
        "seed": args.seed,
        "stages": [
            {"stage": r["stage"], "pass": r["pass"],
             "requests": r["requests"], "error_rate": r["error_rate"],
             "p99_ms": r["latency_ms"]["p99"]}
            for r in results
        ],
        "pass": all(r["pass"] for r in results),
    }
    (output_dir / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    return 0 if summary["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
