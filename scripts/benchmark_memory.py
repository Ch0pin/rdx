#!/usr/bin/env python3
"""Sample benchmark process-tree RSS; this is not Activity Monitor footprint.

Usage: python3 scripts/benchmark_memory.py OUTPUT.json COMMAND [ARG...]
COMMAND's stdout goes to OUTPUT.json; RSS samples go to OUTPUT.json.memory.json.
"""
import json
import subprocess
import sys
import time
from pathlib import Path

output = Path(sys.argv[1])
peak = 0
samples = 0
with output.open("w") as stream:
    process = subprocess.Popen(sys.argv[2:], stdout=stream)
    while process.poll() is None:
        raw = subprocess.check_output(["ps", "-axo", "pid=,ppid=,rss="], text=True)
        rows = [tuple(map(int, line.split())) for line in raw.splitlines() if line.strip()]
        owned = {process.pid}
        while True:
            descendants = {pid for pid, parent, _ in rows if parent in owned}
            if descendants <= owned:
                break
            owned |= descendants
        peak = max(peak, sum(rss for pid, _, rss in rows if pid in owned))
        samples += 1
        time.sleep(0.2)
    status = process.wait()
metrics = {"peak_process_tree_rss_mib": peak / 1024, "samples": samples,
           "exit_code": status, "note": "Sum of sampled RSS, including shared-page double counting; not Activity Monitor footprint."}
output.with_suffix(output.suffix + ".memory.json").write_text(json.dumps(metrics, indent=2) + "\n")
print(json.dumps(metrics))
sys.exit(status)
