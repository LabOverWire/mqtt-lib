#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

: "${PHASE:?Set PHASE=calib|calib-direct|accuracy|routercheck|main|crosscheck|hol|capped|paced|bcfleet|bclater}"
: "${ROUTER_IP:?Set ROUTER_IP in group${GROUP}.env}"
: "${ROUTER_HOP_US:?Set ROUTER_HOP_US (measured router one-way delay, microseconds) in group${GROUP}.env}"
: "${PUB_INTERNAL_IP:?Set PUB_INTERNAL_IP in group${GROUP}.env}"
: "${SUB_INTERNAL_IP:?Set SUB_INTERNAL_IP in group${GROUP}.env}"
ROUTER_GUARD="${ROUTER_GUARD:-0}"
if [ "$ROUTER_HOP_US" -lt 0 ] || [ "$ROUTER_HOP_US" -ge 10000 ]; then
    echo "ROUTER_HOP_US=${ROUTER_HOP_US} is outside [0, 10000)" >&2
    exit 1
fi

if [ "$PHASE" = "capped" ]; then
    EXPERIMENT="03f_capped_g${GROUP}"
elif [ "$PHASE" = "paced" ]; then
    EXPERIMENT="03g_paced_g${GROUP}"
elif [ "$PHASE" = "bcfleet" ] || [ "$PHASE" = "bclater" ]; then
    EXPERIMENT="03h_buildcheck_g${GROUP}"
else
    EXPERIMENT="03e_single_segment_g${GROUP}"
fi
RESULTS_DIR="${ROOT_DIR}/results-v5"
OUTPUT_DIR="${RESULTS_DIR}/${EXPERIMENT}"
PLAN="${OUTPUT_DIR}/plan_${PHASE}${BLOCK:+_b${BLOCK}}.csv"
MANIFEST="${OUTPUT_DIR}/manifest.csv"
REMOTE_DIR="$REMOTE_EXPERIMENTS_DIR"
mkdir -p "$OUTPUT_DIR"

if [ "$PHASE" = "calib-direct" ]; then
    EXPECTED_PATH="off"
    HOP_US=0
else
    EXPECTED_PATH="on"
    HOP_US="$ROUTER_HOP_US"
fi

CA="--ca-cert /opt/mqtt-certs/ca.pem"
BROKER_BASE="--tls-cert /opt/mqtt-certs/server.pem --tls-key /opt/mqtt-certs/server.key --quic-host 0.0.0.0:14567"
TPUT_ARGS="--mode throughput --duration 60 --warmup 5 --payload-size 256 --qos 0 --publishers 16 --subscribers 8 --inflight 64"
HOL_ARGS="--mode hol-blocking --topics 8 --duration 60 --warmup 5 --payload-size 256 --rate 500 --trace-dir /tmp/hol-traces"
QUIC_URL="--url quic://${BROKER_IP}:14567 ${CA}"

declare -A CLIENT_ARGS=(
    [tcp]="--url mqtt://${BROKER_IP}:1883"
    [tls]="--url mqtts://${BROKER_IP}:8883 ${CA}"
    [quic-main]="${QUIC_URL} --quic-stream-strategy control-only"
    [quic-main-ppub]="${QUIC_URL} --quic-stream-strategy per-publish"
    [quic-main-ptopic]="${QUIC_URL} --quic-stream-strategy per-topic"
    [quic-ctl]="${QUIC_URL} --quic-stream-strategy control-only"
    [quic-ptopic]="${QUIC_URL} --quic-stream-strategy per-topic"
    [quic-ppub]="${QUIC_URL} --quic-stream-strategy per-publish"
)
declare -A BROKER_EXTRA=(
    [tcp]=""
    [tls]=""
    [quic-main]=""
    [quic-main-ppub]=""
    [quic-main-ptopic]=""
    [quic-ctl]="--quic-delivery-strategy control-only"
    [quic-ptopic]="--quic-delivery-strategy per-topic"
    [quic-ppub]="--quic-delivery-strategy per-publish"
)
declare -A BASE_DELAY_US=([tput]=10000 [hol]=25000)
RERUN_SHA="7c65659d5e768d8a38f251bfbdb96c0ee9bdcaa79e2fd5eda36ac4f644377a07"
FOLLOWUP_SHA="f7f2afe469b3328b0b4929dfc14031c47d889b655c9c04f9956bc86912f734d7"
rated_phase() {
    case "$1" in
        capped|paced|bcfleet|bclater) return 0 ;;
        *) return 1 ;;
    esac
}
if [ "$PHASE" = "bcfleet" ]; then
    EXPECTED_SHA="$RERUN_SHA"
else
    EXPECTED_SHA="$FOLLOWUP_SHA"
fi
if rated_phase "$PHASE"; then
    MANIFEST_HEADER="label,order,phase,mode,config,loss,run,workload,router_probe,broker_probe,rate,delay_us,router_hop_us,router_guard,path_state,ilb_health,ilb_health_after,started,finished,binary_sha,broker_flags,bench_args"
else
    EXPECTED_SHA="$RERUN_SHA"
    MANIFEST_HEADER="label,order,phase,mode,config,loss,run,workload,router_probe,broker_probe,delay_us,router_hop_us,router_guard,path_state,ilb_health,ilb_health_after,started,finished,broker_flags,bench_args"
fi
SKIPPED=0

scp_router() {
    scp -q -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no -o ProxyJump="${SSH_USER}@${BROKER_SSH_IP}" \
        "${SSH_USER}@${ROUTER_IP}:$1" "$2"
}

scp_broker() {
    scp -q -i "$SSH_KEY_PATH" -o StrictHostKeyChecking=no "${SSH_USER}@${BROKER_SSH_IP}:$1" "$2"
}

deploy_scripts() {
    local host
    for host in ssh_broker ssh_pub ssh_sub ssh_router; do
        COPYFILE_DISABLE=1 tar --no-xattrs -C "$ROOT_DIR" -czf - netem monitor | \
            "$host" "sudo mkdir -p ${REMOTE_DIR} && sudo chown \$(id -u):\$(id -g) ${REMOTE_DIR} && tar -xzmf - -C ${REMOTE_DIR}"
    done
}

stop_leftover_probes() {
    ssh_broker "sudo pkill -INT -x bpftrace; for _ in \$(seq 1 20); do pgrep -x bpftrace >/dev/null || break; sleep 0.5; done; ! pgrep -x bpftrace" || return 1
    ssh_router "sudo pkill -INT -x bpftrace; for _ in \$(seq 1 20); do pgrep -x bpftrace >/dev/null || break; sleep 0.5; done; ! pgrep -x bpftrace" || return 1
}

reset_impairment() {
    ssh_broker "sudo bash ${REMOTE_DIR}/netem/clear.sh" >/dev/null || return 1
    ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh 0" >/dev/null || return 1
}

ilb_healthy() {
    local state
    state=$(ilb_health)
    echo "$state"
    [ "$state" = "HEALTHY" ]
}

preflight_step() {
    local report="$1" name="$2"
    shift 2
    echo "=== ${name} ===" >> "$report"
    if ! "$@" >> "$report" 2>&1; then
        echo "PREFLIGHT FAILED at '${name}', see ${report}" >&2
        tail -n 20 "$report" >&2
        exit 1
    fi
}

host_report() {
    "$1" "uname -r; sha256sum ${REMOTE_DIR}/netem/* ${REMOTE_DIR}/monitor/*; ethtool -k ens4; mqttv5 --version; sha256sum \$(readlink -f \$(command -v mqttv5))"
}

binary_check() {
    local host sha
    for host in ssh_broker ssh_pub ssh_sub; do
        sha=$("$host" "sha256sum \$(readlink -f \$(command -v mqttv5)) | cut -d' ' -f1" < /dev/null) || return 1
        echo "${host} ${sha}"
        if [ "$sha" != "$EXPECTED_SHA" ]; then
            echo "binary mismatch on ${host}: ${sha} (expected ${EXPECTED_SHA})" >&2
            return 1
        fi
    done
}

manifest_check() {
    if [ -f "$MANIFEST" ] && [ "$(head -1 "$MANIFEST")" != "$MANIFEST_HEADER" ]; then
        echo "manifest header mismatch in ${MANIFEST}" >&2
        return 1
    fi
}

preflight() {
    local report path
    report="${OUTPUT_DIR}/preflight_${PHASE}_$(date +%Y%m%dT%H%M%S).txt"
    preflight_step "$report" "local git" git -C "$ROOT_DIR" log -1 --format=%H
    preflight_step "$report" "local changes" git -C "$ROOT_DIR" status --short -- netem monitor parallel analysis
    preflight_step "$report" "manifest header" manifest_check
    preflight_step "$report" "binary" binary_check
    preflight_step "$report" "leftover probes" stop_leftover_probes
    preflight_step "$report" "broker" host_report ssh_broker
    preflight_step "$report" "pub" host_report ssh_pub
    preflight_step "$report" "sub" host_report ssh_sub
    preflight_step "$report" "router" ssh_router "uname -r; sha256sum ${REMOTE_DIR}/netem/* ${REMOTE_DIR}/monitor/*; ethtool -k ens4; command -v bpftrace; cat /sys/class/net/ens4/device/features"
    preflight_step "$report" "router setup" ssh_router "sudo bash ${REMOTE_DIR}/netem/router_setup.sh ${BROKER_IP} ${PUB_INTERNAL_IP} ${SUB_INTERNAL_IP} ${ROUTER_GUARD}"
    preflight_step "$report" "impairment reset" reset_impairment
    preflight_step "$report" "ilb health" ilb_healthy
    path=$(router_path_state)
    echo "=== router path: ${path} (expected ${EXPECTED_PATH}) ===" >> "$report"
    if [ "$path" != "$EXPECTED_PATH" ]; then
        echo "PREFLIGHT FAILED: router path is ${path}, phase ${PHASE} needs ${EXPECTED_PATH} (03e_infra.sh route-on/route-off)" >&2
        exit 1
    fi
    echo "preflight ok: ${report}"
}

set_impairment() {
    local mode="$1" loss="$2" delay_us="$3"
    case "$mode" in
        single)
            ssh_broker "sudo bash ${REMOTE_DIR}/netem/broker_netem.sh ${delay_us} 0 100000"
            ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh ${loss}"
            ;;
        legacy)
            ssh_broker "sudo bash ${REMOTE_DIR}/netem/broker_netem.sh ${delay_us} ${loss} 100000"
            ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh 0"
            ;;
        legacy-calib)
            ssh_broker "sudo bash ${REMOTE_DIR}/netem/broker_netem.sh ${delay_us} ${loss}"
            ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh 0"
            ;;
        router)
            ssh_broker "sudo bash ${REMOTE_DIR}/netem/clear.sh"
            ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh ${loss} ${delay_us}"
            ;;
        tbf)
            ssh_broker "sudo bash ${REMOTE_DIR}/netem/broker_tbf.sh ${delay_us} ${loss}"
            ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh 0"
            ;;
        *)
            echo "unknown mode ${mode}" >&2
            return 1
            ;;
    esac
}

snapshot() {
    local label="$1" when="$2"
    ssh_broker "echo '=== tc ==='; tc -s -d qdisc show dev ens4; echo '=== snmp ==='; cat /proc/net/snmp; \
        echo '=== netstat ==='; cat /proc/net/netstat; echo '=== dev ==='; cat /proc/net/dev" \
        > "${OUTPUT_DIR}/${label}_broker_${when}.txt" 2>&1 || true
    ssh_router "echo '=== tc ==='; tc -s -d qdisc show dev ens4; echo '=== ingress ==='; sudo bash ${REMOTE_DIR}/netem/router_ingress_count.sh; \
        echo '=== dev ==='; cat /proc/net/dev; echo '=== softnet ==='; cat /proc/net/softnet_stat; \
        echo '=== snmp ==='; cat /proc/net/snmp; echo '=== netstat ==='; cat /proc/net/netstat; echo '=== ethtool ==='; ethtool -S ens4" \
        > "${OUTPUT_DIR}/${label}_router_${when}.txt" 2>&1 || true
}

ROUTER_PROBE_PID=""
BROKER_PROBE_PID=""
ROUTER_MONITOR_PID=""
BACKLOG_PID=""

start_probe() {
    local host="$1" script="$2"
    "$host" "sudo nohup bpftrace ${REMOTE_DIR}/netem/${script} > /tmp/probe.txt 2>&1 & echo \$!"
}

stop_probe() {
    local host="$1" pid="$2"
    "$host" "sudo kill -INT ${pid}; for _ in \$(seq 1 20); do sudo kill -0 ${pid} 2>/dev/null || break; sleep 0.5; done" || true
}

start_side_monitors() {
    ssh_router "pkill -f '[r]outer_monitor.sh'" 2>/dev/null || true
    ssh_broker "pkill -f '[b]roker_backlog.sh'" 2>/dev/null || true
    ROUTER_MONITOR_PID=$(ssh_router "nohup bash ${REMOTE_DIR}/monitor/router_monitor.sh > /tmp/router_monitor.csv 2>&1 & echo \$!") || ROUTER_MONITOR_PID=""
    BACKLOG_PID=$(ssh_broker "nohup bash ${REMOTE_DIR}/netem/broker_backlog.sh > /tmp/broker_backlog.csv 2>&1 & echo \$!") || BACKLOG_PID=""
}

stop_side_monitors() {
    local label="$1"
    if [ -n "$ROUTER_MONITOR_PID" ]; then
        ssh_router "kill ${ROUTER_MONITOR_PID}" 2>/dev/null || true
    fi
    if [ -n "$BACKLOG_PID" ]; then
        ssh_broker "kill ${BACKLOG_PID}" 2>/dev/null || true
    fi
    if [ -n "$label" ]; then
        scp_router /tmp/router_monitor.csv "${OUTPUT_DIR}/${label}_router_resources.csv" || true
        scp_broker /tmp/broker_backlog.csv "${OUTPUT_DIR}/${label}_broker_backlog.csv" || true
    fi
    ROUTER_MONITOR_PID=""
    BACKLOG_PID=""
}

stop_run_probes() {
    local label="$1"
    if [ -n "$ROUTER_PROBE_PID" ]; then
        stop_probe ssh_router "$ROUTER_PROBE_PID"
        [ -n "$label" ] && { scp_router /tmp/probe.txt "${OUTPUT_DIR}/${label}_router_probe.txt" || echo "WARN: router probe copy failed ${label}" >&2; }
        ROUTER_PROBE_PID=""
    fi
    if [ -n "$BROKER_PROBE_PID" ]; then
        stop_probe ssh_broker "$BROKER_PROBE_PID"
        [ -n "$label" ] && { scp_broker /tmp/probe.txt "${OUTPUT_DIR}/${label}_broker_probe.txt" || echo "WARN: broker probe copy failed ${label}" >&2; }
        BROKER_PROBE_PID=""
    fi
}

cleanup() {
    trap - EXIT INT TERM
    stop_run_probes ""
    stop_side_monitors ""
    stop_stale_monitors
    stop_broker || true
    ssh_broker "sudo bash ${REMOTE_DIR}/netem/clear.sh" >/dev/null 2>&1 || true
    ssh_router "sudo bash ${REMOTE_DIR}/netem/router_loss.sh 0" >/dev/null 2>&1 || true
}

run_hol() {
    local label="$1" bench_args="$2"
    ssh_pub "rm -rf /tmp/hol-traces; ulimit -n 65536; mqttv5 bench ${bench_args}" \
        > "${OUTPUT_DIR}/${label}.json" 2>/dev/null || true
    warn_if_empty "${OUTPUT_DIR}/${label}.json"
    local csv
    for csv in messages.csv quinn_stats.csv; do
        scp -q -i "$SSH_KEY_PATH" "${SSH_USER}@${PUB_IP}:/tmp/hol-traces/${csv}" \
            "${OUTPUT_DIR}/${label}_${csv}" 2>/dev/null || true
    done
}

json_complete() {
    local f="${OUTPUT_DIR}/$1.json"
    [ -f "$f" ] && [ "$(wc -c < "$f")" -gt 300 ] && python3 - "$f" <<'PY'
import json
import sys
results = json.load(open(sys.argv[1])).get("results", {})
sys.exit(0 if (results.get("throughput_avg") or results.get("measured_rate") or 0) > 0 else 1)
PY
}

run_recorded() {
    [ -f "$MANIFEST" ] && grep -q "^$1," "$MANIFEST" && json_complete "$1"
}

run_line() {
    local order="$1" phase="$2" mode="$3" config="$4" loss="$5" run="$6" workload="$7" router_probe="$8" broker_probe="$9" rate="${10:-0}"
    local label="${phase}_${mode}_${config}_loss${loss}pct_run${run}"
    if rated_phase "$phase"; then
        label="${phase}_${mode}_${config}_r${rate}_loss${loss}pct_run${run}"
    fi
    if run_recorded "$label"; then
        echo "  skip (complete): ${label}"
        return 0
    fi
    local delay_us=$((BASE_DELAY_US[$workload] - HOP_US))
    local broker_flags="${BROKER_BASE} ${BROKER_EXTRA[$config]}"
    local bench_args
    if [ "$workload" = "hol" ]; then
        bench_args="${CLIENT_ARGS[$config]} ${HOL_ARGS}"
    else
        bench_args="${CLIENT_ARGS[$config]} ${TPUT_ARGS}"
    fi
    if [ "$rate" -gt 0 ]; then
        bench_args="${bench_args} --rate ${rate}"
    fi
    echo "[${EXPERIMENT}] #${order} ${label} delay=${delay_us}us"

    rm -f "${OUTPUT_DIR}/${label}.json" "${OUTPUT_DIR}/${label}_pub.json"
    stop_broker
    reset_impairment
    local path health health_after
    path=$(router_path_state)
    health=$(ilb_health)
    if [ "$path" != "$EXPECTED_PATH" ] || [ "$health" != "HEALTHY" ]; then
        echo "WARN: router path ${path} (expected ${EXPECTED_PATH}), ilb '${health}'; skipping ${label}" >&2
        SKIPPED=$((SKIPPED + 1))
        return 0
    fi
    set_impairment "$mode" "$loss" "$delay_us"
    if ! start_broker "$broker_flags"; then
        echo "WARN: broker start failed, skipping ${label}" >&2
        SKIPPED=$((SKIPPED + 1))
        return 0
    fi
    local started
    started=$(date +%s)

    snapshot "$label" before
    if [ "$router_probe" = "1" ]; then
        ROUTER_PROBE_PID=$(start_probe ssh_router netem_probe.bt) || { ROUTER_PROBE_PID=""; echo "WARN: router probe start failed ${label}" >&2; }
    fi
    if [ "$broker_probe" = "1" ]; then
        local broker_script="netem_probe.bt"
        [ "$mode" = "tbf" ] && broker_script="tbf_probe.bt"
        [ "$mode" = "router" ] || [ "$mode" = "single" ] || [ "$mode" = "legacy-calib" ] && broker_script="tcp_cwr_probe.bt"
        BROKER_PROBE_PID=$(start_probe ssh_broker "$broker_script") || { BROKER_PROBE_PID=""; echo "WARN: broker probe start failed ${label}" >&2; }
    fi
    if [ -n "$ROUTER_PROBE_PID$BROKER_PROBE_PID" ]; then
        sleep 3
    fi
    start_monitors
    start_side_monitors

    if [ "$workload" = "hol" ]; then
        run_hol "$label" "$bench_args"
    else
        run_bench_split "$EXPERIMENT" "$label" "$bench_args"
    fi

    stop_side_monitors "$label"
    stop_monitors "$OUTPUT_DIR" "$label"
    stop_run_probes "$label"
    snapshot "$label" after
    health_after=$(ilb_health)

    if ! json_complete "$label"; then
        echo "WARN: no usable result for ${label}; not recorded, will retry on resume" >&2
        SKIPPED=$((SKIPPED + 1))
        sleep 5
        return 0
    fi
    [ -f "$MANIFEST" ] || echo "$MANIFEST_HEADER" > "$MANIFEST"
    if rated_phase "$phase"; then
        echo "${label},${order},${phase},${mode},${config},${loss},${run},${workload},${router_probe},${broker_probe},${rate},${delay_us},${HOP_US},${ROUTER_GUARD},${path},\"${health}\",\"${health_after}\",${started},$(date +%s),${EXPECTED_SHA},\"${broker_flags}\",\"${bench_args}\"" >> "$MANIFEST"
    else
        echo "${label},${order},${phase},${mode},${config},${loss},${run},${workload},${router_probe},${broker_probe},${delay_us},${HOP_US},${ROUTER_GUARD},${path},\"${health}\",\"${health_after}\",${started},$(date +%s),\"${broker_flags}\",\"${bench_args}\"" >> "$MANIFEST"
    fi
    sleep 5
}

trap cleanup EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

case "$PHASE" in
    main|crosscheck|hol)
        if ! python3 "${ROOT_DIR}/analysis/single_segment_accept.py" "$OUTPUT_DIR"; then
            if [ -z "${ACCEPT_OVERRIDE:-}" ]; then
                echo "pilot acceptance has not passed for ${EXPERIMENT}; refusing PHASE=${PHASE} (set ACCEPT_OVERRIDE=<reason> to proceed)" >&2
                exit 1
            fi
            echo "$(date -u +%FT%TZ) PHASE=${PHASE} proceeding despite failed pilot acceptance: ${ACCEPT_OVERRIDE}" | tee -a "${OUTPUT_DIR}/acceptance_overrides.log" >&2
        fi
        ;;
esac

python3 "${SCRIPT_DIR}/03e_plan.py" --group "$GROUP" --phase "$PHASE" ${BLOCK:+--block "$BLOCK"} --out "$PLAN"
deploy_scripts
preflight

while IFS=, read -r order phase mode config loss run workload router_probe broker_probe rate; do
    [ "$order" = "order" ] && continue
    run_line "$order" "$phase" "$mode" "$config" "$loss" "$run" "$workload" "$router_probe" "$broker_probe" "${rate%$'\r'}" < /dev/null
done < "$PLAN"

if [ "$SKIPPED" -gt 0 ]; then
    echo "experiment ${EXPERIMENT} phase ${PHASE} INCOMPLETE: ${SKIPPED} runs skipped or unusable; rerun to retry them" >&2
    exit 2
fi
echo "experiment ${EXPERIMENT} phase ${PHASE} complete"
