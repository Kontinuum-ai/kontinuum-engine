#!/usr/bin/env bash
# Reachability gate (issue #81): every workspace member must be reachable
# from a real product entry point via normal (build) dependencies, or be
# explicitly exempted in reachability-allow.toml with a stated reason.
#
# Entry points:
#   kontinuum-bridge  — what the iOS app drives through FFI
#   kontinuum-offline — the renderer
#
# Fails when:
#   - a member is reachable from neither entry point and is not exempted
#     (a silently orphaned crate);
#   - an exemption is missing or has an empty reason;
#   - an exemption is stale (the crate is not a member, or is reachable
#     and the entry should be removed).
#
# Needs only cargo metadata/tree (no build), so it is fast enough for CI.

set -euo pipefail
cd "$(dirname "$0")/.."

ENTRY_POINTS=(kontinuum-bridge kontinuum-offline)
ALLOW_FILE="reachability-allow.toml"

# All workspace members.
members=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[].name' | sort -u)

# Union of the entry points' normal-dependency trees (package names).
reachable=$(
  for ep in "${ENTRY_POINTS[@]}"; do
    cargo tree -p "$ep" --edges normal --prefix none --format '{p}'
  done | awk '{ print $1 }' | sort -u
)

# Exemptions: one TOML table per crate, each with a mandatory reason.
exemptions=$(python3 - "$ALLOW_FILE" <<'PY'
import sys, tomllib

try:
    with open(sys.argv[1], "rb") as f:
        data = tomllib.load(f)
except FileNotFoundError:
    sys.exit(f"allow-list error: {sys.argv[1]} not found")
except tomllib.TOMLDecodeError as e:
    sys.exit(f"allow-list error: {sys.argv[1]} is not valid TOML: {e}")

for crate, entry in data.items():
    if not isinstance(entry, dict) or "reason" not in entry:
        sys.exit(f"allow-list error: [{crate}] must be a table with a 'reason' key")
    reason = entry["reason"]
    if not isinstance(reason, str) or not reason.strip():
        sys.exit(f"allow-list error: [{crate}] has an empty 'reason'")
    print(crate)
PY
)

fail=0

# Orphans: members reachable from neither entry point and not exempted.
while read -r m; do
  if grep -qxF "$m" <<<"$reachable"; then continue; fi
  if grep -qxF "$m" <<<"$exemptions"; then continue; fi
  echo "ERROR: workspace member '$m' is reachable from neither ${ENTRY_POINTS[*]}"
  echo "       wire it in, or declare it standalone in $ALLOW_FILE with a reason."
  fail=1
done <<<"$members"

# Stale exemptions: the list must not outlive the orphanhood it excuses.
while read -r x; do
  [ -z "$x" ] && continue
  if ! grep -qxF "$x" <<<"$members"; then
    echo "ERROR: '$x' is exempted in $ALLOW_FILE but is not a workspace member"
    fail=1
  elif grep -qxF "$x" <<<"$reachable"; then
    echo "ERROR: '$x' is exempted in $ALLOW_FILE but is now reachable — remove the exemption"
    fail=1
  fi
done <<<"$exemptions"

if [ "$fail" -ne 0 ]; then
  echo "reachability gate: FAIL"
  exit 1
fi
echo "reachability gate: OK (every member reachable or explicitly exempted)"
