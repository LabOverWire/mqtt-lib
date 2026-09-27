#!/usr/bin/env bash
set -euo pipefail

: "${GROUP:?Set GROUP=1|2|3}"
: "${ACCEPT_OVERRIDE:?Set ACCEPT_OVERRIDE=<reason>}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export ACCEPT_OVERRIDE

run_phase() {
    local attempt
    for attempt in 1 2 3 4 5; do
        if PHASE="$1" bash "${HERE}/03e_single_segment.sh"; then
            return 0
        fi
        echo "phase $1 attempt ${attempt} failed; resuming in 120 s" >&2
        sleep 120
    done
    return 1
}

run_phase main
run_phase crosscheck
run_phase hol
