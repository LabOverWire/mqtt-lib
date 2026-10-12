#!/usr/bin/env bash
set -euo pipefail

package="$1"
wait_secs=600

version=$(cargo metadata --no-deps --format-version 1 \
  | jq -r --arg p "$package" '.packages[] | select(.name == $p) | .version')
if [ -z "$version" ]; then
  echo "package $package not found in the workspace" >&2
  exit 1
fi

index_url="https://index.crates.io/${package:0:2}/${package:2:2}/$package"
entries=$(mktemp)
trap 'rm -f "$entries"' EXIT

index_status() {
  local code
  code=$(curl -s --connect-timeout 10 --max-time 30 \
    -A "LabOverWire/mqtt-lib release workflow" \
    -o "$entries" -w '%{http_code}' "$index_url") || code="curl exit $?"
  case $code in
    200)
      if grep -qF "\"vers\":\"$version\"" "$entries"; then
        echo listed
      else
        echo absent
      fi
      ;;
    404) echo absent ;;
    *) echo "unreachable ($code)" ;;
  esac
}

wait_for_index() {
  local wanted=$1 deadline=$((SECONDS + wait_secs)) status
  status=$(index_status)
  until [ "$status" = listed ] || { [ "$wanted" = known ] && [ "$status" = absent ]; }; do
    if [ "$SECONDS" -ge "$deadline" ]; then
      echo "$package $version: crates.io index still $status after ${wait_secs}s" >&2
      exit 1
    fi
    sleep 10
    status=$(index_status)
  done
  echo "$status"
}

state=$(wait_for_index known)
if [ "$state" = listed ]; then
  echo "$package $version is already on crates.io, skipping"
  exit 0
fi

cargo publish --package "$package"

wait_for_index listed > /dev/null
echo "$package $version is on crates.io"
