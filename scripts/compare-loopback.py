#!/usr/bin/env python3
"""Alternate two prebuilt release loopback test binaries; no network beyond localhost.

Build both revisions with the same instrumented tests/loopback.rs and toolchain.
Run without concurrent builds. Linux /usr/bin/time supplies whole-process CPU/RSS;
the harness supplies client-thread CPU, sampled latency and allocation counts.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")
    args.output.mkdir(parents=True, exist_ok=True)
    binaries = {
        label: {"path": str(getattr(args, label).resolve()),
                "sha256": hashlib.sha256(getattr(args, label).read_bytes()).hexdigest()}
        for label in ("baseline", "candidate")
    }
    (args.output / "binaries.json").write_text(json.dumps(binaries, indent=2) + "\n")
    samples = {"baseline": {}, "candidate": {}}
    resources = {"baseline": [], "candidate": []}
    for run in range(args.runs):
        order = ("baseline", "candidate") if run % 2 == 0 else ("candidate", "baseline")
        for label in order:
            binary = getattr(args, label).resolve()
            log = args.output / f"{label}-{run}.log"
            print(f"Running {label} {run}: {binary}", flush=True)
            with log.open("w") as output:
                subprocess.run(
                    ["/usr/bin/time", "-v", str(binary), "--exact", "loopback_rpc_cost",
                     "--ignored", "--nocapture", "--test-threads=1"],
                    stdout=output, stderr=subprocess.STDOUT, timeout=90, check=True,
                )
            text = log.read_text()
            resources[label].append({key: float(re.search(pattern, text).group(1))
                                     for key, pattern in (
                ("user_seconds", r"User time \(seconds\): ([\d.]+)"),
                ("system_seconds", r"System time \(seconds\): ([\d.]+)"),
                ("maximum_rss_kib", r"Maximum resident set size \(kbytes\): (\d+)"),
            )})
            for payload in ("Plain", "Gzip"):
                values = samples[label].setdefault(payload, {key: [] for key in
                    ("rpcs_per_second", "thread_cpu_us", "p50_us", "p95_us", "p99_us", "allocations")})
                for rate, cpu in re.findall(
                    rf"{payload} reply .*? round [024]: ([\d.]+) RPC/s; client thread ([\d.]+) us", text
                ):
                    values["rpcs_per_second"].append(float(rate))
                    values["thread_cpu_us"].append(float(cpu))
                for percentiles in re.findall(
                    rf"{payload} round [024]: sampled RPC latency p50/p95/p99 ([\d.]+)/([\d.]+)/([\d.]+) us", text
                ):
                    for key, value in zip(("p50_us", "p95_us", "p99_us"), percentiles):
                        values[key].append(float(value))
                values["allocations"].extend(map(float, re.findall(
                    rf"{payload} reply .*? round [13]: ([\d.]+) allocations", text)))
    for payloads in samples.values():
        for metrics in payloads.values():
            for key, values in metrics.items():
                expected = args.runs * (2 if key == "allocations" else 3)
                if len(values) != expected:
                    raise ValueError(f"{key}: expected {expected} samples, found {len(values)}")
    summary = {label: {payload: {key: {"median": statistics.median(values), "samples": values}
                               for key, values in metrics.items()}
                       for payload, metrics in payloads.items()}
               for label, payloads in samples.items()}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    (args.output / "resources.json").write_text(json.dumps(resources, indent=2) + "\n")
    print(json.dumps({label: {payload: {key: value["median"] for key, value in metrics.items()}
                             for payload, metrics in payloads.items()}
                      for label, payloads in summary.items()}, indent=2))


if __name__ == "__main__":
    main()
