#!/usr/bin/env python3
import sys
from pathlib import Path

import fig01_spread_vs_loss
import fig02_spike_isolation
import fig03_latency_percentiles

script_dir = Path(__file__).resolve().parent
default_results = script_dir.parent.parent / "results" / "01_subscriber_flow_delivery"
default_output = script_dir / "output"

results_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else default_results
output_dir = Path(sys.argv[2]) if len(sys.argv) > 2 else default_output
output_dir.mkdir(parents=True, exist_ok=True)

print("Generating subscriber flow delivery figures...")
print()

for name, mod in [
    ("fig01_spread_vs_loss", fig01_spread_vs_loss),
    ("fig02_spike_isolation", fig02_spike_isolation),
    ("fig03_latency_percentiles", fig03_latency_percentiles),
]:
    print(f"[{name}]")
    mod.main(results_dir, output_dir)

print()
print("Done.")
