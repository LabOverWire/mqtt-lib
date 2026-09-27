#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

ACTION="${1:?usage: GROUP=G $0 up|tools|guest|health|smoke|hop|route-off|route-on|down|down-shared}"

ZONE="us-west1-b"
NETWORK="default"
ROUTER="exp3-router-${GROUP}"
BROKER_VM="mqoq-broker-${GROUP}"
BROKER_TAG="exp3-brk-${GROUP}"
IG="exp3-ig-${GROUP}"
BACKEND="exp3-bs-${GROUP}"
RULE="exp3-fr-${GROUP}"
HEALTH="exp3-hc"
FIREWALL="exp3-allow-hc"
BPFTRACE_URL="https://github.com/bpftrace/bpftrace/releases/download/v0.27.0/bpftrace-x86_64"
: "${PUB_INTERNAL_IP:?Set PUB_INTERNAL_IP in group${GROUP}.env}"
: "${SUB_INTERNAL_IP:?Set SUB_INTERNAL_IP in group${GROUP}.env}"

exists() { gc "$@" --format='value(name)' >/dev/null 2>&1; }

pbr_name() { echo "exp3-g${GROUP}-${1//./-}"; }

router_internal_ip() {
    gc compute instances describe "$ROUTER" --zone="$ZONE" --format='value(networkInterfaces[0].networkIP)'
}

clear_broker_qdisc() {
    ssh_broker "sudo bash ${REMOTE_EXPERIMENTS_DIR}/netem/clear.sh"
}

up() {
    local image
    image=$(gc compute disks describe "$(gc compute instances describe "$BROKER_VM" --zone="$ZONE" \
        --format='value(disks[0].source.basename())')" --zone="$ZONE" --format='value(sourceImage)')
    echo "router image: ${image}"

    exists compute instances describe "$ROUTER" --zone="$ZONE" || \
        gc compute instances create "$ROUTER" --zone="$ZONE" --machine-type=n2-standard-4 --can-ip-forward \
            --no-address --image="$image" --subnet="$NETWORK" --tags=exp3-router
    exists compute firewall-rules describe "$FIREWALL" || \
        gc compute firewall-rules create "$FIREWALL" --network="$NETWORK" --allow=tcp:22 \
            --source-ranges=35.191.0.0/16,130.211.0.0/22 --target-tags=exp3-router
    exists compute health-checks describe "$HEALTH" --region="$GCP_REGION" || \
        gc compute health-checks create tcp "$HEALTH" --region="$GCP_REGION" --port=22
    exists compute instance-groups unmanaged describe "$IG" --zone="$ZONE" || \
        gc compute instance-groups unmanaged create "$IG" --zone="$ZONE"
    gc compute instance-groups unmanaged list-instances "$IG" --zone="$ZONE" \
        --format='value(instance.basename())' | grep -qx "$ROUTER" || \
        gc compute instance-groups unmanaged add-instances "$IG" --zone="$ZONE" --instances="$ROUTER"
    exists compute backend-services describe "$BACKEND" --region="$GCP_REGION" || \
        gc compute backend-services create "$BACKEND" --region="$GCP_REGION" --load-balancing-scheme=INTERNAL \
            --protocol=UNSPECIFIED --health-checks="$HEALTH" --health-checks-region="$GCP_REGION"
    gc compute backend-services describe "$BACKEND" --region="$GCP_REGION" \
        --format='value(backends[].group)' | grep -q "instanceGroups/${IG}" || \
        gc compute backend-services add-backend "$BACKEND" --region="$GCP_REGION" \
            --instance-group="$IG" --instance-group-zone="$ZONE"
    exists compute forwarding-rules describe "$RULE" --region="$GCP_REGION" || \
        gc compute forwarding-rules create "$RULE" --region="$GCP_REGION" --load-balancing-scheme=INTERNAL \
            --network="$NETWORK" --subnet="$NETWORK" --ip-protocol=L3_DEFAULT --ports=ALL --backend-service="$BACKEND"

    local ilb dst
    ilb=$(gc compute forwarding-rules describe "$RULE" --region="$GCP_REGION" --format='value(IPAddress)')
    echo "ilb next hop: ${ilb}"
    for dst in "$PUB_INTERNAL_IP" "$SUB_INTERNAL_IP"; do
        exists network-connectivity policy-based-routes describe "$(pbr_name "$dst")" || \
            gc network-connectivity policy-based-routes create "$(pbr_name "$dst")" \
                --network="projects/${GCP_PROJECT}/global/networks/${NETWORK}" \
                --source-range="${BROKER_IP}/32" --destination-range="${dst}/32" \
                --ip-protocol=ALL --protocol-version=IPV4 --next-hop-ilb-ip="$ilb" \
                --tags="$BROKER_TAG" --priority=100
    done
    echo "router internal ip: $(router_internal_ip)  (set ROUTER_IP in group${GROUP}.env, then run tools, guest, route-on)"
}

extract_bpftrace() {
    echo "test -x /opt/bpftrace/AppRun || { cd /tmp && chmod 755 bpftrace.appimage && rm -rf squashfs-root && ./bpftrace.appimage --appimage-extract >/dev/null && sudo rm -rf /opt/bpftrace && sudo mv /tmp/squashfs-root /opt/bpftrace; }"
}

tools() {
    local install='sudo tee /usr/local/bin/bpftrace >/dev/null <<< $'"'"'#!/bin/sh\nexec /opt/bpftrace/AppRun "$@"'"'"' && sudo chmod 755 /usr/local/bin/bpftrace && sudo modprobe sch_netem && sudo modprobe sch_tbf'
    ssh_broker "test -s /tmp/bpftrace.appimage || curl -fsSL -o /tmp/bpftrace.appimage ${BPFTRACE_URL}"
    ssh_broker "$(extract_bpftrace) && ${install}"
    if ! ssh_router "test -x /opt/bpftrace/AppRun"; then
        ssh -A -i "$SSH_KEY_PATH" $SSH_OPTS "${SSH_USER}@${BROKER_SSH_IP}" \
            "scp -q -o StrictHostKeyChecking=no /tmp/bpftrace.appimage ${SSH_USER}@${ROUTER_IP}:/tmp/bpftrace.appimage"
        ssh_router "$(extract_bpftrace)"
    fi
    ssh_router "$install"
    ssh_broker "sudo bpftrace --version; uname -r"
    ssh_router "sudo bpftrace --version; uname -r"
}

guest() {
    COPYFILE_DISABLE=1 tar --no-xattrs -C "$ROOT_DIR" -czf - netem monitor | \
        ssh_router "sudo mkdir -p ${REMOTE_EXPERIMENTS_DIR} && sudo chown \$(id -u):\$(id -g) ${REMOTE_EXPERIMENTS_DIR} && tar -xzmf - -C ${REMOTE_EXPERIMENTS_DIR}"
    ssh_router "sudo bash ${REMOTE_EXPERIMENTS_DIR}/netem/router_setup.sh ${BROKER_IP} ${PUB_INTERNAL_IP} ${SUB_INTERNAL_IP} ${ROUTER_GUARD:-0}"
}

health() {
    local state
    state=$(ilb_health)
    echo "ilb backend health: ${state}"
    [ "$state" = "HEALTHY" ]
}

ping_loss() {
    ssh_broker "ping -q -c ${2:-500} -i 0.01 $1" | sed -n 's/.* \([0-9.]*\)% packet loss.*/\1/p'
}

smoke() {
    local path dst loss
    health
    clear_broker_qdisc
    ssh_router "sudo bash ${REMOTE_EXPERIMENTS_DIR}/netem/router_loss.sh 0"
    path=$(router_path_state)
    echo "router path: ${path}"
    [ "$path" = "on" ] || { echo "SMOKE FAIL: broker traffic is not crossing the router" >&2; return 1; }
    for dst in "$SUB_INTERNAL_IP" "$PUB_INTERNAL_IP"; do
        loss=$(ping_loss "$dst") || loss=""
        echo "broker -> ${dst} through router: ${loss:-no reply}% loss"
        [ "$loss" = "0" ] || { echo "SMOKE FAIL: packets lost or not delivered to ${dst}" >&2; return 1; }
    done
    echo "SMOKE PASS (ICMP; loaded TCP/UDP accounting is gated per run by the calib phase)"
}

wait_for_path() {
    local want="$1" attempt state
    for attempt in $(seq 1 30); do
        state=$(router_path_state)
        if [ "$state" = "$want" ]; then
            echo "router path ${want} (attempt ${attempt})"
            return 0
        fi
        sleep 10
    done
    echo "router path did not become ${want} (last: ${state})" >&2
    return 1
}

route_off() {
    gc compute instances remove-tags "$BROKER_VM" --zone="$ZONE" --tags="$BROKER_TAG"
    wait_for_path off
}

route_on() {
    gc compute instances add-tags "$BROKER_VM" --zone="$ZONE" --tags="$BROKER_TAG"
    wait_for_path on
}

median_rtt_ms() {
    ssh_broker "ping -c 1000 -i 0.005 $1" | sed -n 's/.*time=\([0-9.]*\) ms/\1/p' | sort -n | \
        awk '{a[NR] = $1} END { if (NR == 0) exit 1; print a[int((NR + 1) / 2)] }'
}

hop() {
    local with without
    clear_broker_qdisc
    ssh_router "sudo bash ${REMOTE_EXPERIMENTS_DIR}/netem/router_loss.sh 0"
    wait_for_path on
    with=$(median_rtt_ms "$SUB_INTERNAL_IP")
    trap 'route_on' EXIT
    route_off
    without=$(median_rtt_ms "$SUB_INTERNAL_IP")
    trap - EXIT
    route_on
    if ! awk -v w="$with" -v o="$without" 'BEGIN { exit !(w > o && o > 0) }'; then
        echo "HOP FAIL: via router ${with} ms is not above direct ${without} ms" >&2
        return 1
    fi
    awk -v w="$with" -v o="$without" -v g="$GROUP" \
        'BEGIN { printf "group %s median rtt via router %.3f ms, direct %.3f ms, ROUTER_HOP_US=%d\n", g, w, o, (w - o) * 1000 + 0.5 }'
}

down() {
    local dst leftover=""
    clear_broker_qdisc || true
    gc compute instances remove-tags "$BROKER_VM" --zone="$ZONE" --tags="$BROKER_TAG" || true
    for dst in "$PUB_INTERNAL_IP" "$SUB_INTERNAL_IP"; do
        gc network-connectivity policy-based-routes delete "$(pbr_name "$dst")" || true
    done
    gc compute forwarding-rules delete "$RULE" --region="$GCP_REGION" || true
    gc compute backend-services delete "$BACKEND" --region="$GCP_REGION" || true
    gc compute instance-groups unmanaged delete "$IG" --zone="$ZONE" || true
    gc compute instances delete "$ROUTER" --zone="$ZONE" || true
    for dst in "$PUB_INTERNAL_IP" "$SUB_INTERNAL_IP"; do
        exists network-connectivity policy-based-routes describe "$(pbr_name "$dst")" && leftover+=" $(pbr_name "$dst")"
    done
    exists compute forwarding-rules describe "$RULE" --region="$GCP_REGION" && leftover+=" ${RULE}"
    exists compute backend-services describe "$BACKEND" --region="$GCP_REGION" && leftover+=" ${BACKEND}"
    exists compute instance-groups unmanaged describe "$IG" --zone="$ZONE" && leftover+=" ${IG}"
    exists compute instances describe "$ROUTER" --zone="$ZONE" && leftover+=" ${ROUTER}"
    if [ -n "$leftover" ]; then
        echo "TEARDOWN INCOMPLETE:${leftover}" >&2
        return 1
    fi
    echo "group ${GROUP} torn down; shared ${HEALTH} and ${FIREWALL} remain until: GROUP=${GROUP} $0 down-shared"
}

down_shared() {
    gc compute health-checks delete "$HEALTH" --region="$GCP_REGION" || true
    gc compute firewall-rules delete "$FIREWALL" || true
    local leftover=""
    exists compute health-checks describe "$HEALTH" --region="$GCP_REGION" && leftover+=" ${HEALTH}"
    exists compute firewall-rules describe "$FIREWALL" && leftover+=" ${FIREWALL}"
    if [ -n "$leftover" ]; then
        echo "TEARDOWN INCOMPLETE:${leftover}" >&2
        return 1
    fi
    echo "shared resources removed"
}

case "$ACTION" in
    up) up ;;
    tools) tools ;;
    guest) guest ;;
    health) health ;;
    smoke) smoke ;;
    hop) hop ;;
    route-off) route_off ;;
    route-on) route_on ;;
    down) down ;;
    down-shared) down_shared ;;
    *) echo "unknown action ${ACTION}" >&2; exit 1 ;;
esac
