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

name=$(printf '%s' "$package" | tr '[:upper:]' '[:lower:]')
case ${#name} in
  1) index_path="1/$name" ;;
  2) index_path="2/$name" ;;
  3) index_path="3/${name:0:1}/$name" ;;
  *) index_path="${name:0:2}/${name:2:2}/$name" ;;
esac
index_url="https://index.crates.io/$index_path"

is_published() {
  local entries
  entries=$(curl -sf -A "LabOverWire/mqtt-lib release workflow" "$index_url") || return 1
  grep -q "\"vers\":\"$version\"" <<<"$entries"
}

if is_published; then
  echo "$package $version is already on crates.io, skipping"
  exit 0
fi

cargo publish --package "$package"

deadline=$((SECONDS + wait_secs))
until is_published; do
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "$package $version did not appear in the crates.io index within ${wait_secs}s" >&2
    exit 1
  fi
  sleep 10
done
echo "$package $version is on crates.io"
