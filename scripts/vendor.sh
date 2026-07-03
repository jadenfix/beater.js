#!/usr/bin/env bash
# Vendor single-file ESM builds of React for SSR inside the embedded isolate (M4).
# Fetched once, checked in — no npm resolution in the framework.
#
# esm.sh entry URLs return tiny re-export shims; we fetch the real es2022
# builds directly and rewrite their absolute imports to the bare specifiers
# the beater module loader maps ("react", "react/jsx-runtime").
set -euo pipefail

REACT_V=19.2.7
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="$SCRIPT_DIR/../crates/beater-runtime/assets/vendor"

rewrite_imports() { # file
  local file="$1"
  local tmp
  tmp="$(mktemp "${file}.XXXXXX")"
  sed \
    -e "s|\"/react@${REACT_V}/es2022/react.mjs\"|\"react\"|g" \
    -e "s|\"/react@${REACT_V}/es2022/jsx-runtime.mjs\"|\"react/jsx-runtime\"|g" \
    -e "s|'/react@${REACT_V}/es2022/react.mjs'|'react'|g" \
    -e "s|'/react@${REACT_V}/es2022/jsx-runtime.mjs'|'react/jsx-runtime'|g" \
    "$file" >"$tmp"
  mv "$tmp" "$file"
}

fetch() { # url out
  curl -fsSL "$1" -o "$2"
  # absolute-path imports -> bare specifiers our loader resolves
  rewrite_imports "$2"
  chmod 0644 "$2"
}

check_no_absolute_imports() { # dir
  local dir="$1"
  local remaining
  local grep_status
  set +e
  remaining="$(grep -H -E -o "from[[:space:]]*['\"]/[^'\"]*['\"]|import[[:space:]]*['\"]/[^'\"]*['\"]|import[[:space:]]*\\([[:space:]]*['\"]/[^'\"]*['\"]" "$dir"/*.mjs)"
  grep_status=$?
  set -e
  if [[ "$grep_status" -eq 0 ]]; then
    printf '%s\n' "$remaining"
    echo "error: vendored modules still contain absolute imports" >&2
    return 1
  fi
  if [[ "$grep_status" -ne 1 ]]; then
    echo "error: failed to scan vendored modules for absolute imports" >&2
    return "$grep_status"
  fi
  echo "  none"
}

main() {
  mkdir -p "$OUT"

  fetch "https://esm.sh/react@${REACT_V}/es2022/react.mjs" "$OUT/react.mjs"
  fetch "https://esm.sh/react@${REACT_V}/es2022/jsx-runtime.mjs" "$OUT/react-jsx-runtime.mjs"
  fetch "https://esm.sh/react-dom@${REACT_V}/es2022/server.edge.bundle.mjs" "$OUT/react-dom-server.mjs"

  echo "vendored into $OUT:"
  ls -la "$OUT"
  echo "remaining absolute imports (should be none):"
  check_no_absolute_imports "$OUT"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
