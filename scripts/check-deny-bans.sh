#!/usr/bin/env bash
# Proves the cargo-deny policy rejects a payment SDK, even as a test-only dependency.
# Passes only when `cargo deny check bans` FAILS on tests/deny-fixture and names async-stripe.
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="$(cargo deny \
  --manifest-path "$repo_root/tests/deny-fixture/Cargo.toml" \
  --config "$repo_root/deny.toml" \
  check bans 2>&1)"
status=$?

if [[ $status -eq 0 ]]; then
  echo "FAIL: cargo deny accepted a payment SDK dependency" >&2
  echo "$output" >&2
  exit 1
fi
if ! grep -q "async-stripe" <<<"$output"; then
  echo "FAIL: cargo deny failed, but not because of async-stripe" >&2
  echo "$output" >&2
  exit 1
fi
echo "ok: cargo deny rejects async-stripe as a test dependency"
