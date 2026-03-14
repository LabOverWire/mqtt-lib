#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${REPO_ROOT}/comnet"
OUT="${REPO_ROOT}/submission"

if [ ! -d "$SRC" ]; then
    echo "ERROR: comnet/ directory not found at ${SRC}"
    exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT"

cp "$SRC"/sections/*.tex "$OUT/"

sed \
    -e 's|\\input{sections/\(.*\)}|\\input{\1}|g' \
    -e '/\\graphicspath/d' \
    "$SRC/main.tex" > "$OUT/main.tex"

cp "$SRC"/figures/*.pdf "$OUT/"

cp "$SRC/references.bib" "$OUT/"

if [ -f "$SRC/highlights.txt" ]; then
    cp "$SRC/highlights.txt" "$OUT/"
fi

cd "$OUT"
zip -r "${REPO_ROOT}/submission.zip" .

echo "Submission prepared:"
echo "  Directory: ${OUT}"
echo "  Archive:   ${REPO_ROOT}/submission.zip"
echo "  Files:     $(find "$OUT" -type f | wc -l | tr -d ' ')"
