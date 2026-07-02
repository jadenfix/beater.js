#!/usr/bin/env bash
# Vendor single-file ESM builds of React for SSR inside the embedded isolate (M4).
# Fetched once, checked in — no npm resolution in the framework.
set -euo pipefail

OUT="$(dirname "$0")/../crates/beater-runtime/assets/vendor"
mkdir -p "$OUT"

# esm.sh single-file bundles ("?bundle" inlines transitive deps).
curl -fsSL "https://esm.sh/react@19?bundle&target=es2022" -o "$OUT/react.mjs"
curl -fsSL "https://esm.sh/react@19/jsx-runtime?bundle&target=es2022" -o "$OUT/react-jsx-runtime.mjs"
curl -fsSL "https://esm.sh/react-dom@19/server.edge?bundle&target=es2022" -o "$OUT/react-dom-server.mjs"

echo "vendored into $OUT:"
ls -la "$OUT"
