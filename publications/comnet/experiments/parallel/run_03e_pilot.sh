#!/usr/bin/env bash
set -euo pipefail

: "${GROUP:?Set GROUP=1|2|3}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

run_phase() {
    local attempt
    for attempt in 1 2 3; do
        if PHASE="$1" bash "${HERE}/03e_single_segment.sh"; then
            return 0
        fi
        echo "phase $1 attempt ${attempt} failed; resuming in 60 s" >&2
        sleep 60
    done
    return 1
}

run_phase calib
bash "${HERE}/03e_infra.sh" route-off
if run_phase calib-direct; then
    bash "${HERE}/03e_infra.sh" route-on
else
    bash "${HERE}/03e_infra.sh" route-on
    exit 1
fi
run_phase accuracy
python3 "${HERE}/../analysis/single_segment_accept.py" "${HERE}/../results-v5/03e_single_segment_g${GROUP}"
