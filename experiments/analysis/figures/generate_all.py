import sys
from pathlib import Path

from fig01_spike_iso_vs_loss import main as fig01
from fig02_spike_iso_vs_rtt import main as fig02
from fig03_spike_iso_vs_topics import main as fig03
from fig04_wcorr_vs_spike_iso import main as fig04
from fig05_latency_percentiles import main as fig05
from fig06_timeseries_cwnd import main as fig06
from fig07_inter_arrival import main as fig07
from fig08_connection_latency import main as fig08
from fig09_throughput_vs_loss import main as fig09
from fig10_resource_overhead import main as fig10
from fig11_qos_comparison import main as fig11
from fig12_datagram_vs_stream import main as fig12
from fig13_resource_overhead_scaled import main as fig13
from fig14_strategy_comparison import main as fig14

FIGURES = [
    ("fig01_spike_iso_vs_loss", fig01),
    ("fig02_spike_iso_vs_rtt", fig02),
    ("fig03_spike_iso_vs_topics", fig03),
    ("fig04_wcorr_vs_spike_iso", fig04),
    ("fig05_latency_percentiles", fig05),
    ("fig06_timeseries_cwnd", fig06),
    ("fig07_inter_arrival", fig07),
    ("fig08_connection_latency", fig08),
    ("fig09_throughput_vs_loss", fig09),
    ("fig10_resource_overhead", fig10),
    ("fig11_qos_comparison", fig11),
    ("fig12_datagram_vs_stream", fig12),
    ("fig13_resource_overhead_scaled", fig13),
    ("fig14_strategy_comparison", fig14),
]


def main():
    if len(sys.argv) < 3:
        print(f"Usage: {sys.argv[0]} <results_dir> <output_dir>")
        sys.exit(1)

    results_dir = Path(sys.argv[1])
    output_dir = Path(sys.argv[2])

    if not results_dir.exists():
        print(f"ERROR: results directory not found: {results_dir}")
        sys.exit(1)

    output_dir.mkdir(parents=True, exist_ok=True)

    print(f"Results: {results_dir}")
    print(f"Output:  {output_dir}")
    print()

    for name, figure_fn in FIGURES:
        print(f"Generating {name}...")
        try:
            figure_fn(results_dir, output_dir)
        except Exception as exc:
            print(f"  FAILED: {exc}")
    print()
    print("Done.")


if __name__ == "__main__":
    main()
