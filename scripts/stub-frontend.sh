#!/usr/bin/env bash
# A stand-in frontend/build for CI jobs that compile with --all-features,
# which turns on embed-frontend: the feature compiles the directory in and
# fails without it. Shaped like a real build — a shell that links a hashed
# script — so the embedded-frontend test has something to check. Never
# overwrites a real build.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -f frontend/build/index.html ]; then
  echo "frontend/build exists; leaving it"
  exit 0
fi
mkdir -p frontend/build/_app/immutable/entry
cat > frontend/build/index.html <<'HTML'
<!doctype html>
<html lang="en"><head><meta charset="utf-8" /><!--%haloid.head%-->
<script type="module" src="/_app/immutable/entry/start.stub.js"></script>
</head><body></body></html>
HTML
echo 'export {};' > frontend/build/_app/immutable/entry/start.stub.js
echo "stub frontend written"
