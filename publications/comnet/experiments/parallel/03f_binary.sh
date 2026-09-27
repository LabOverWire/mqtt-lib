#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/common_parallel.sh"

ACTION="${1:?usage: GROUP=G $0 install <local binary> <name> | use <name> <sha256> | status}"

on_hosts() {
    local host
    for host in ssh_broker ssh_pub ssh_sub; do
        printf '%-10s ' "$host"
        "$host" "$@" < /dev/null
    done
}

case "$ACTION" in
    install)
        BINARY="${2:?local binary path}"
        NAME="${3:?installed name}"
        EXPECTED=$(shasum -a 256 "$BINARY" | cut -d' ' -f1)
        for host in ssh_broker ssh_pub ssh_sub; do
            "$host" "cat > /tmp/${NAME} && sudo install -m 755 /tmp/${NAME} /opt/mqtt-bin/${NAME} && rm -f /tmp/${NAME}" < "$BINARY"
        done
        on_hosts "sha256sum /opt/mqtt-bin/${NAME} | cut -d' ' -f1" | grep -c "$EXPECTED" | \
            awk -v want=3 '{ if ($1 != want) { print "checksum mismatch on " want - $1 " host(s)"; exit 1 } else { print "installed on 3 hosts" } }'
        ;;
    use)
        NAME="${2:?installed name}"
        EXPECTED="${3:?expected sha256}"
        for host in ssh_broker ssh_pub ssh_sub; do
            if ! "$host" "test -x /opt/mqtt-bin/${NAME} && sha256sum /opt/mqtt-bin/${NAME} | grep -q ^${EXPECTED}" < /dev/null; then
                echo "${host}: /opt/mqtt-bin/${NAME} missing or wrong sha; nothing switched" >&2
                exit 1
            fi
        done
        on_hosts "sudo ln -sfn /opt/mqtt-bin/${NAME} /usr/local/bin/mqttv5 && sha256sum \$(readlink -f /usr/local/bin/mqttv5) | cut -d' ' -f1" | tee /dev/stderr | \
            grep -c "$EXPECTED" | awk '{ if ($1 != 3) { print "switch incomplete: " $1 " of 3 hosts on the expected binary"; exit 1 } else { print "all 3 hosts on the expected binary" } }'
        ;;
    status)
        on_hosts "readlink /usr/local/bin/mqttv5; ls /opt/mqtt-bin"
        ;;
    *)
        echo "unknown action ${ACTION}" >&2
        exit 1
        ;;
esac
