# Archived Experiment Results

Results in this directory have been superseded by newer data collected after
methodology fixes (Phases 1-3 of the HOL blocking investigation).

## results_archived_rate500/

Early experiment results collected with a 500 msg/s rate limit before the
rate was standardized to 5000 msg/s. Contains experiments 02-04.
Superseded by: `results/02_hol_blocking/`, `results/03_throughput_under_loss/`,
`results/04_stream_strategies/`.

## results-pre-server-streams/

Results collected before the broker supported configurable server delivery
strategies. The broker always delivered via per-topic streams regardless of
the client's `--quic-stream-strategy` setting, making per-publish and
control-only comparisons invalid. Contains experiments 01-06.
Superseded by: `results/01_connection_latency/`, `results/02_hol_blocking/`,
`results/03_throughput_under_loss/`, `results/04_stream_strategies/`.

## 02_hol_blocking_pre_trace/

HOL blocking results collected after Phases 1-3 fixes (DataPerSubscription
deprecation, server delivery strategy, QUIC flow control tuning) but before
trace instrumentation (Phase 5). Data is valid but lacks per-message traces
(`messages.csv`, `quinn_stats.csv`) and derived metrics (`spike_isolation_ratio`,
`inter_arrival_cluster_ratio`). 80 JSON files, 240 total files.
Superseded by: `results/02_hol_blocking/` which includes trace data.
