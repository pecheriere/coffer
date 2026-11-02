#!/usr/bin/env bash
# The full pre-commit gate, for the checkout it's run in. Worktrees share one
# lock, so two gates never compete for the machine, and each worktree keeps its
# own log, which starts with the worktree and commit it describes.
set -euo pipefail

root=$(git rev-parse --show-toplevel)
cd "$root"
unset CARGO_TARGET_DIR

lock="$(git rev-parse --path-format=absolute --git-common-dir)/gate.lock"
log="$(git rev-parse --path-format=absolute --git-dir)/gate.log"

acquire_lock() {
  until mkdir "$lock" 2>/dev/null; do
    holder=$(cat "$lock/pid" 2>/dev/null || true)
    # A holder that died can't release: remove its lock. An empty pid means
    # the holder is between mkdir and writing it, which takes moments, so an
    # empty lock older than a minute was left by a gate killed while releasing.
    if { [ -n "$holder" ] && ! kill -0 "$holder" 2>/dev/null; } ||
      { [ -z "$holder" ] && [ -n "$(find "$lock" -maxdepth 0 -mmin +1)" ]; }; then
      rm -f "$lock/pid"
      rmdir "$lock" 2>/dev/null || true
      continue
    fi
    sleep 15
  done
  echo $$ >"$lock/pid"
  trap 'rm -f "$lock/pid"; rmdir "$lock"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
}

step() {
  local name=$1
  shift
  echo "--- $name: $*" >>"$log"
  if "$@" >>"$log" 2>&1; then
    echo "ok    $name"
  else
    echo "FAIL  $name (see $log)"
    exit 1
  fi
}

openapi_is_current() {
  cargo run --quiet --locked -p coffer-server -- openapi | diff -q - docs/api/openapi.json
}

acquire_lock
echo "gate: $root at $(git rev-parse --short HEAD)" | tee "$log"

step fmt cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
step tests cargo nextest run --workspace --all-features --locked
# nextest skips doctests.
step doctests cargo test --doc --workspace --all-features --locked
step docs env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
step deny cargo deny check
step openapi openapi_is_current

grep -m 1 'Summary \[' "$log" || true
echo "gate passed"
