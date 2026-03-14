# Figure Generation Scripts

Each script generates one publication figure as PDF + PNG in `output/`.

## Figure-Script-Data Mapping

| Figure | Script | Data Directory | Description |
|--------|--------|---------------|-------------|
| fig01 | `fig01_spike_iso_vs_loss.py` | results-v5 | Spike isolation ratio vs packet loss rate |
| fig02 | `fig02_spike_iso_vs_rtt.py` | results-v5 | Spike isolation ratio vs RTT |
| fig03 | `fig03_spike_iso_vs_topics.py` | results-v5 | Spike isolation ratio vs topic count |
| fig04 | `fig04_wcorr_vs_spike_iso.py` | results-v5 | Windowed correlation vs spike isolation |
| fig05 | `fig05_latency_percentiles.py` | results-v5 | Latency percentile distributions |
| fig06 | `fig06_timeseries_cwnd.py` | results-v5 | Latency time series with CWND overlay |
| fig07 | `fig07_inter_arrival.py` | results-v5 | Inter-arrival time distributions |
| fig08 | `fig08_connection_latency.py` | results-v5 | Connection setup latency by transport |
| fig09 | `fig09_throughput_vs_loss.py` | results-v5 | Throughput vs packet loss rate |
| fig10 | `fig10_resource_overhead.py` | results-v5 | Broker CPU and RSS time series |
| fig11 | `fig11_qos_comparison.py` | results-v5 | QoS 0 vs QoS 1 comparison |
| fig12 | `fig12_datagram_vs_stream.py` | results-v5 | QUIC datagram vs stream mode |
| fig13 | `fig13_resource_overhead_scaled.py` | results-v5 | Resource overhead at scale |
| fig14 | `fig14_strategy_comparison.py` | results-v5 | Stream strategy comparison |
| fig15 | `fig15_frame_packing.py` | results-v5 | Frame packing policy impact |

## Regenerating All Figures

```bash
python3 generate_all.py <results_dir> output/
```

All scripts accept `results_dir` as the first argument (default: `results-v5`).

## Shared Style

`style.py` defines consistent colors, line styles, and label mappings across all figures.
