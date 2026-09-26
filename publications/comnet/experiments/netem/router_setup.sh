#!/usr/bin/env bash
set -euo pipefail

BROKER_IP="${1:?usage: $0 <broker_ip> <pub_ip> <sub_ip> [guard 0|1]}"
PUB_IP="${2:?usage: $0 <broker_ip> <pub_ip> <sub_ip> [guard 0|1]}"
SUB_IP="${3:?usage: $0 <broker_ip> <pub_ip> <sub_ip> [guard 0|1]}"
GUARD="${4:-0}"

DEV=$(ip route show default | awk '{print $5}' | head -1)
MTU=$(cat "/sys/class/net/${DEV}/mtu")
BURST=$((MTU + 14))

sysctl -qw net.ipv4.ip_forward=1
sysctl -qw net.ipv4.conf.all.send_redirects=0
sysctl -qw "net.ipv4.conf.${DEV}.send_redirects=0"
for feature in gro rx-gro-hw lro rx-gro-list rx-udp-gro-forwarding; do
    ethtool -K "$DEV" "$feature" off 2>/dev/null || true
done

ethtool -k "$DEV" | grep -E '^(generic-receive-offload|rx-gro-hw|large-receive-offload|rx-gro-list|rx-udp-gro-forwarding):'
still_on=$(ethtool -k "$DEV" | grep -E '^(generic-receive-offload|rx-gro-hw|large-receive-offload|rx-gro-list):' | grep -c ': on' || true)

if [ "$still_on" -gt 0 ] && [ "$GUARD" != "1" ]; then
    echo "ERROR: receive coalescing still on for ${DEV}; rerun with guard=1" >&2
    exit 3
fi

tc qdisc del dev "$DEV" root 2>/dev/null || true
tc qdisc del dev "$DEV" clsact 2>/dev/null || true

tc qdisc add dev "$DEV" clsact
tc filter add dev "$DEV" ingress protocol ip prio 1 u32 \
    match ip src "${BROKER_IP}/32" match ip dst "${PUB_IP}/32" action pass
tc filter add dev "$DEV" ingress protocol ip prio 2 u32 \
    match ip src "${BROKER_IP}/32" match ip dst "${SUB_IP}/32" action pass

tc qdisc add dev "$DEV" root handle 1: prio bands 3 priomap 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2
if [ "$GUARD" = "1" ]; then
    tc qdisc add dev "$DEV" parent 1:1 handle 5: tbf rate 100gbit burst "$BURST" latency 50ms
    tc qdisc add dev "$DEV" parent 5:1 handle 10: netem loss 0% limit 100000
    echo "5:1" > /tmp/router_netem_parent
else
    tc qdisc add dev "$DEV" parent 1:1 handle 10: netem loss 0% limit 100000
    echo "1:1" > /tmp/router_netem_parent
fi
tc filter add dev "$DEV" parent 1: protocol ip prio 1 u32 match ip src "${BROKER_IP}/32" flowid 1:1
echo "router: dev=${DEV} broker=${BROKER_IP} pub=${PUB_IP} sub=${SUB_IP} guard=${GUARD} coalescing_on=${still_on}"
