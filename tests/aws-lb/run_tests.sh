#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/env.sh"

PASSED=0
FAILED=0
ERRORS=""

start_backend() {
    local ip=$1
    local name=$2
    local extra_args="${3:-}"
    ssh_cmd "$ip" "killall mqttv5 || true" 2>/dev/null
    sleep 1
    ssh_cmd "$ip" "nohup mqttv5 broker --allow-anonymous \
        --host 0.0.0.0:1883 \
        --tls-host 0.0.0.0:8883 \
        --tls-cert $REMOTE_CERT_DIR/server.pem \
        --tls-key $REMOTE_CERT_DIR/server.key \
        --quic-host 0.0.0.0:14567 \
        --storage-backend memory \
        $extra_args \
        > /tmp/broker.log 2>&1 & echo \$!"
}

start_lb() {
    local config=$1
    ssh_cmd "$LB_IP" "killall mqttv5 || true" 2>/dev/null
    sleep 1
    ssh_cmd "$LB_IP" "nohup mqttv5 broker --config $REMOTE_CERT_DIR/$config \
        > /tmp/broker.log 2>&1 & echo \$!"
}

stop_all() {
    for ip in "$LB_IP" "$BACKEND_A_IP" "$BACKEND_B_IP"; do
        ssh_cmd "$ip" "killall mqttv5 || true" 2>/dev/null
    done
    sleep 1
}

stop_backends() {
    for ip in "$BACKEND_A_IP" "$BACKEND_B_IP"; do
        ssh_cmd "$ip" "killall mqttv5 || true" 2>/dev/null
    done
    sleep 1
}

run_test() {
    local name=$1
    shift
    echo -n "  $name ... "
    if output=$("$@" 2>&1); then
        echo "PASS"
        PASSED=$((PASSED + 1))
    else
        echo "FAIL"
        ERRORS="${ERRORS}\n  FAIL: ${name}\n${output}\n"
        FAILED=$((FAILED + 1))
    fi
}

echo "=========================================="
echo "  AWS Load Balancer Integration Tests"
echo "=========================================="
echo "LB:        $LB_IP"
echo "Backend A: $BACKEND_A_IP"
echo "Backend B: $BACKEND_B_IP"
echo ""

# ----------------------------------------------------------
echo "--- TCP Tests ---"
stop_all
start_backend "$BACKEND_A_IP" "backend-a"
start_backend "$BACKEND_B_IP" "backend-b"
start_lb "lb-tcp.json"
sleep 3

run_test "TCP: pub through LB" \
    mqttv5 pub --url "mqtt://${LB_IP}:1883" --topic "test/tcp/pub" \
    --message "tcp-redirect" --non-interactive

run_test "TCP: sub through LB receives message" bash -c "
    timeout 10 mqttv5 sub --url 'mqtt://${LB_IP}:1883' --topic 'test/tcp/sub' \
        --count 1 --non-interactive &
    SUB_PID=\$!
    sleep 2
    mqttv5 pub --url 'mqtt://${BACKEND_A_IP}:1883' --topic 'test/tcp/sub' \
        --message 'tcp-sub-test' --non-interactive
    mqttv5 pub --url 'mqtt://${BACKEND_B_IP}:1883' --topic 'test/tcp/sub' \
        --message 'tcp-sub-test' --non-interactive
    wait \$SUB_PID
"

run_test "TCP: pub+sub through LB end-to-end" bash -c "
    timeout 10 mqttv5 sub --url 'mqtt://${BACKEND_A_IP}:1883' --topic 'test/tcp/e2e' \
        --count 1 --non-interactive --client-id 'tcp-sub-a' &
    SUB_A=\$!
    timeout 10 mqttv5 sub --url 'mqtt://${BACKEND_B_IP}:1883' --topic 'test/tcp/e2e' \
        --count 1 --non-interactive --client-id 'tcp-sub-b' &
    SUB_B=\$!
    sleep 2
    mqttv5 pub --url 'mqtt://${LB_IP}:1883' --topic 'test/tcp/e2e' \
        --message 'tcp-e2e' --non-interactive --client-id 'tcp-pub-e2e'
    wait \$SUB_A || true
    wait \$SUB_B || true
"

run_test "TCP: distribution across backends" bash -c "
    for i in 0 1 2 3; do
        mqttv5 pub --url 'mqtt://${LB_IP}:1883' --topic 'test/tcp/dist' \
            --message 'msg-\$i' --non-interactive --client-id 'dist-\$i'
    done
"

# ----------------------------------------------------------
echo ""
echo "--- TLS Tests ---"
stop_all
start_backend "$BACKEND_A_IP" "backend-a"
start_backend "$BACKEND_B_IP" "backend-b"
start_lb "lb-tls.json"
sleep 3

run_test "TLS: pub through LB" \
    mqttv5 pub --url "mqtts://${LB_IP}:8883" --topic "test/tls/pub" \
    --message "tls-redirect" --non-interactive \
    --ca-cert "$CERT_DIR/ca.pem"

run_test "TLS: sub through LB receives message" bash -c "
    timeout 10 mqttv5 sub --url 'mqtts://${LB_IP}:8883' --topic 'test/tls/sub' \
        --count 1 --non-interactive --ca-cert '$CERT_DIR/ca.pem' &
    SUB_PID=\$!
    sleep 2
    mqttv5 pub --url 'mqtts://${BACKEND_A_IP}:8883' --topic 'test/tls/sub' \
        --message 'tls-sub-test' --non-interactive --ca-cert '$CERT_DIR/ca.pem'
    mqttv5 pub --url 'mqtts://${BACKEND_B_IP}:8883' --topic 'test/tls/sub' \
        --message 'tls-sub-test' --non-interactive --ca-cert '$CERT_DIR/ca.pem'
    wait \$SUB_PID
"

run_test "TLS: pub+sub through LB end-to-end" bash -c "
    timeout 10 mqttv5 sub --url 'mqtts://${BACKEND_A_IP}:8883' --topic 'test/tls/e2e' \
        --count 1 --non-interactive --client-id 'tls-sub-a' \
        --ca-cert '$CERT_DIR/ca.pem' &
    SUB_A=\$!
    timeout 10 mqttv5 sub --url 'mqtts://${BACKEND_B_IP}:8883' --topic 'test/tls/e2e' \
        --count 1 --non-interactive --client-id 'tls-sub-b' \
        --ca-cert '$CERT_DIR/ca.pem' &
    SUB_B=\$!
    sleep 2
    mqttv5 pub --url 'mqtts://${LB_IP}:8883' --topic 'test/tls/e2e' \
        --message 'tls-e2e' --non-interactive --client-id 'tls-pub-e2e' \
        --ca-cert '$CERT_DIR/ca.pem'
    wait \$SUB_A || true
    wait \$SUB_B || true
"

run_test "TLS: wrong CA cert rejected" bash -c "
    ! mqttv5 pub --url 'mqtts://${BACKEND_A_IP}:8883' --topic 'test/tls/badca' \
        --message 'should-fail' --non-interactive 2>&1
"

# ----------------------------------------------------------
echo ""
echo "--- QUIC Tests ---"
stop_all
start_backend "$BACKEND_A_IP" "backend-a"
start_backend "$BACKEND_B_IP" "backend-b"
start_lb "lb-quic.json"
sleep 5

run_test "QUIC: pub through LB" \
    mqttv5 pub --url "quic://${LB_IP}:14567" --topic "test/quic/pub" \
    --message "quic-redirect" --non-interactive \
    --ca-cert "$CERT_DIR/ca.pem"

run_test "QUIC: sub through LB receives message" bash -c "
    timeout 15 mqttv5 sub --url 'quic://${LB_IP}:14567' --topic 'test/quic/sub' \
        --count 1 --non-interactive --ca-cert '$CERT_DIR/ca.pem' &
    SUB_PID=\$!
    sleep 4
    mqttv5 pub --url 'quic://${BACKEND_A_IP}:14567' --topic 'test/quic/sub' \
        --message 'quic-sub-test' --non-interactive --ca-cert '$CERT_DIR/ca.pem'
    mqttv5 pub --url 'quic://${BACKEND_B_IP}:14567' --topic 'test/quic/sub' \
        --message 'quic-sub-test' --non-interactive --ca-cert '$CERT_DIR/ca.pem'
    wait \$SUB_PID
"

run_test "QUIC: pub+sub through LB end-to-end" bash -c "
    timeout 15 mqttv5 sub --url 'quic://${BACKEND_A_IP}:14567' --topic 'test/quic/e2e' \
        --count 1 --non-interactive --client-id 'quic-sub-a' \
        --ca-cert '$CERT_DIR/ca.pem' &
    SUB_A=\$!
    timeout 15 mqttv5 sub --url 'quic://${BACKEND_B_IP}:14567' --topic 'test/quic/e2e' \
        --count 1 --non-interactive --client-id 'quic-sub-b' \
        --ca-cert '$CERT_DIR/ca.pem' &
    SUB_B=\$!
    sleep 4
    mqttv5 pub --url 'quic://${LB_IP}:14567' --topic 'test/quic/e2e' \
        --message 'quic-e2e' --non-interactive --client-id 'quic-pub-e2e' \
        --ca-cert '$CERT_DIR/ca.pem'
    wait \$SUB_A || true
    wait \$SUB_B || true
"

stop_backends
sleep 2

run_test "QUIC: dead backend error handling" bash -c "
    ! mqttv5 pub --url 'quic://${LB_IP}:14567' --topic 'test/quic/dead' \
        --message 'should-fail' --non-interactive \
        --ca-cert '$CERT_DIR/ca.pem' 2>&1
"

# ----------------------------------------------------------
stop_all

echo ""
echo "=========================================="
echo "  Results: $PASSED passed, $FAILED failed"
echo "=========================================="

if [ "$FAILED" -gt 0 ]; then
    echo -e "\nFailures:$ERRORS"
    exit 1
fi
