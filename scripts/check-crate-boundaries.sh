#!/usr/bin/env bash
# The load-bearing architectural rule: the two API surfaces may depend on
# haloid-service and haloid-core only. If either could reach haloid-db directly,
# the legacy and v2 surfaces could drift apart without anyone noticing.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
for crate in haloid-api-legacy haloid-api-v2; do
  for forbidden in haloid-db haloid-storage haloid-media sqlx; do
    if grep -qE "^${forbidden}\b" "crates/${crate}/Cargo.toml"; then
      echo "ERROR: ${crate} depends on ${forbidden} — API crates go through haloid-service." >&2
      fail=1
    fi
  done
done

# haloid-core is the domain. It must not know about transport or persistence.
for forbidden in axum sqlx tower reqwest; do
  if grep -qE "^${forbidden}\b" crates/haloid-core/Cargo.toml; then
    echo "ERROR: haloid-core depends on ${forbidden} — core stays I/O-free." >&2
    fail=1
  fi
done

[ "$fail" -eq 0 ] && echo "crate boundaries ok"
exit "$fail"
