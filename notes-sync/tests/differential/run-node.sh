#!/usr/bin/env bash
# Run icloud-md through driver.mts and capture everything a comparison needs:
#   tests/differential/run-node.sh CASSETTE OUTDIR [--now MS] -- <icloud-md args>
# Writes OUTDIR/{stdout,stderr,exit,requests.json} and uses OUTDIR/home as
# HOME. Paths in the icloud-md args may use @OUT@ for OUTDIR (e.g. the vault).
# Always runs with --deterministic.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
ICLOUD_MD=${ICLOUD_MD:-$(cd "$repo/../../coddingtonbear/icloud-md" && pwd)}
export ICLOUD_MD
cassette=$1 out=$2
shift 2
mkdir -p "$out"
out=$(cd "$out" && pwd)
driver_args=()
while [[ $# -gt 0 && $1 != -- ]]; do driver_args+=("$1"); shift; done
[[ ${1:-} == -- ]] && shift
args=()
for a in "$@"; do args+=("${a//@OUT@/$out}"); done
set +e
"$ICLOUD_MD/node_modules/.bin/tsx" "$here/driver.mts" --cassette "$cassette" --requests "$out/requests.json" \
  --home "$out/home" --deterministic "${driver_args[@]}" -- "${args[@]}" >"$out/stdout" 2>"$out/stderr"
code=$?
set -e
echo "$code" >"$out/exit"
echo "exit $code"
