#!/usr/bin/env python3
"""Chat completions workload for overhead comparison.

Repeated POSTs on keep-alive connections, measuring total latency and
time-to-first-byte. Exits nonzero when the error budget is exceeded.
Stdlib only.

Usage:
  chat_workload.py --base-url URL --output FILE [--mode json|sse]
    [--sessions N] [--duration S] [--api-key KEY] [--model M]
    [--think-min F] [--think-max F] [--timeout S] [--max-error-rate F]
"""

import argparse
import http.client
import json
import random
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from urllib.parse import urlsplit

BUCKETS_MS = [1, 2, 5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, float("inf")]


def percentile(sorted_samples, q):
    if not sorted_samples:
        return None
    index = min(len(sorted_samples) - 1, int((len(sorted_samples) - 1) * q))
    return sorted_samples[index]


def summarize(samples):
    ordered = sorted(samples)
    histogram = {str(b): 0 for b in BUCKETS_MS}
    for sample in ordered:
        for bound in BUCKETS_MS:
            if sample <= bound:
                histogram[str(bound)] += 1
                break
    return {
        "count": len(ordered),
        "min": ordered[0] if ordered else None,
        "mean": sum(ordered) / len(ordered) if ordered else None,
        "p50": percentile(ordered, 0.50),
        "p95": percentile(ordered, 0.95),
        "p99": percentile(ordered, 0.99),
        "max": ordered[-1] if ordered else None,
        "histogram_ms": histogram,
    }


def run_session(base, body, headers, deadline, think, timeout, seed, record):
    rng = random.Random(seed)
    conn = None
    while time.monotonic() < deadline:
        start = time.monotonic()
        try:
            if conn is None:
                conn = http.client.HTTPConnection(base.hostname, base.port, timeout=timeout)
            conn.request("POST", "/v1/chat/completions", body=body, headers=headers)
            response = conn.getresponse()
            first = response.read(1)
            ttfb_ms = (time.monotonic() - start) * 1000
            rest = response.read()
            total_ms = (time.monotonic() - start) * 1000
            payload = first + rest
            if response.status == 200 and (
                b"chat.completion" in payload or b"[DONE]" in payload
            ):
                record(total_ms, ttfb_ms, None)
            else:
                record(None, None, f"http_{response.status}")
        except Exception as error:  # noqa: BLE001 - counted, then retried
            record(None, None, type(error).__name__)
            if conn is not None:
                conn.close()
                conn = None
        time.sleep(rng.uniform(*think))
    if conn is not None:
        conn.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--mode", choices=["json", "sse"], default="json")
    parser.add_argument("--sessions", type=int, default=20)
    parser.add_argument("--duration", type=float, default=15)
    parser.add_argument("--api-key", default="compare-dummy-key")
    parser.add_argument("--model", default="openai/gpt-oss-20b")
    parser.add_argument("--think-min", type=float, default=0.05)
    parser.add_argument("--think-max", type=float, default=0.2)
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--max-error-rate", type=float, default=0.01)
    parser.add_argument("--seed", type=int, default=7)
    args = parser.parse_args(argv)

    base = urlsplit(args.base_url)
    if base.scheme != "http":
        parser.error("only http base URLs are supported")
    body = json.dumps(
        {
            "model": args.model,
            "messages": [{"role": "user", "content": "Say ok"}],
            "max_tokens": 16,
            "stream": args.mode == "sse",
        }
    )
    headers = {
        "Authorization": f"Bearer {args.api_key}",
        "Content-Type": "application/json",
    }

    lock = threading.Lock()
    totals, ttfbs, errors = [], [], {}
    requests = 0

    def record(total_ms, ttfb_ms, error):
        nonlocal requests
        with lock:
            requests += 1
            if total_ms is not None:
                totals.append(total_ms)
            if ttfb_ms is not None:
                ttfbs.append(ttfb_ms)
            if error is not None:
                errors[error] = errors.get(error, 0) + 1

    deadline = time.monotonic() + args.duration
    started = time.monotonic()
    with ThreadPoolExecutor(max_workers=args.sessions) as pool:
        futures = [
            pool.submit(
                run_session,
                base,
                body,
                headers,
                deadline,
                (args.think_min, args.think_max),
                args.timeout,
                args.seed + index,
                record,
            )
            for index in range(args.sessions)
        ]
        for future in futures:
            future.result()
    elapsed = time.monotonic() - started

    error_count = sum(errors.values())
    error_rate = error_count / requests if requests else 0.0
    result = {
        "mode": args.mode,
        "base_url": args.base_url,
        "sessions": args.sessions,
        "duration_seconds": args.duration,
        "requests": requests,
        "errors": errors,
        "error_rate": error_rate,
        "requests_per_second": requests / elapsed if elapsed else 0.0,
        "elapsed_seconds": elapsed,
        "latency_ms": summarize(totals),
        "ttfb_ms": summarize(ttfbs),
    }
    with open(args.output, "w") as handle:
        json.dump(result, handle, indent=2)
        handle.write("\n")
    def ms(value):
        return f"{value:.2f}" if value is not None else "n/a"

    lat = result["latency_ms"]
    print(
        f"{args.mode}: req={requests} err={error_rate:.4f} "
        f"rps={result['requests_per_second']:.1f} "
        f"p50={ms(lat['p50'])}ms p99={ms(lat['p99'])}ms",
        flush=True,
    )
    if requests == 0 or error_rate > args.max_error_rate:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
