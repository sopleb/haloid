#!/usr/bin/env bash
# Run haloid-server for the Playwright suite: a throwaway database on the
# server in TEST_DATABASE_URL, a throwaway data directory, the built frontend.
# Playwright starts this and stops it; the trap removes what it made.
set -euo pipefail
cd "$(dirname "$0")/.."

: "${TEST_DATABASE_URL:?TEST_DATABASE_URL must name a MariaDB server, no database}"
base=${TEST_DATABASE_URL%/}
port=${HALOID_E2E_PORT:-18081}
name="haloid_e2e_$(date +%s)_$$"
data=$(mktemp -d)

proto_and_rest=${base#*://}
userinfo=${proto_and_rest%@*}
hostport=${proto_and_rest#*@}
user=${userinfo%%:*}
pass=${userinfo#*:}
host=${hostport%%:*}
dbport=${hostport##*:}
sql() { mariadb -h "$host" -P "$dbport" -u "$user" -p"$pass" -e "$1"; }

cleanup() {
  sql "DROP DATABASE IF EXISTS \`$name\`" || true
  rm -rf "$data"
}
trap cleanup EXIT
sql "CREATE DATABASE \`$name\`"

cargo build -q -p haloid-server
DATABASE_URL="$base/$name" \
HALOID_BIND="127.0.0.1:$port" \
HALOID_DATA_DIR="$data" \
HALOID_FRONTEND_DIR="frontend/build" \
HALOID_ALTCHA_COST=1 \
HALOID_RELEASES_URL="http://127.0.0.1:9" \
HALOID_IN_CONTAINER=0 \
HALOID_SITE_REGISTRATIONS_PER_HOUR=1000 \
HALOID_UPLOADS_PER_HOUR_GUEST=10000 \
HALOID_UPLOADS_PER_HOUR_USER=10000 \
HALOID_SITE_BASE_URL="http://127.0.0.1:$port" \
HALOID_LOG=warn,sqlx=error \
  ./target/debug/haloid-server
