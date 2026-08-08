#!/usr/bin/env bash
# Root smoke test: exercises the real kernel data path.
#
# Must be run as root (taskstats/ delay accounting need privileges). It drives
# the real binary through `iotop -b -n 2 -d 1` and asserts:
#   - exit code 0
#   - summary headers are printed
#   - the column header and at least one process row are present
#   - no panic in stderr
#
# When delay accounting is not available on the kernel, the `?unavailable?`
# branch is validated instead of the SWAPIN/IO columns.
#
# Usage: bash scripts/smoke-root.sh [path-to-iotop-binary]
set -u

BIN="${1:-target/release/iotop}"

tmp_out="$(mktemp)"
tmp_err="$(mktemp)"
trap 'rm -f "$tmp_out" "$tmp_err"' EXIT

# Run the real binary: batch mode, 2 iterations, 1s delay.
"$BIN" -b -n 2 -d 1 >"$tmp_out" 2>"$tmp_err"
code=$?

out="$(cat "$tmp_out")"
err="$(cat "$tmp_err")"

echo "exit code: $code"
echo "--- stdout ---"
echo "$out"
echo "--- stderr ---"
echo "$err"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# 1. Exit code must be 0.
[ "$code" -ne 0 ] && fail "iotop exited with code $code (expected 0)"

# 2. Summary headers must be present.
for needle in "Total DISK READ" "Actual DISK READ"; do
  grep -q "$needle" <<<"$out" || fail "missing summary header '$needle'"
done

# 3. Column header + at least one process data row.
#    A data row always begins with the TID (right-aligned number), then a PRIO
#    token such as "be/4" or "rt", then USER.
if grep -Eq '^[[:space:]]*[0-9]+[[:space:]]+[^[:space:]]+[[:space:]]+[^[:space:]]' <<<"$out"; then
  echo "process rows: present"
else
  fail "no process data rows found"
fi

# 4. Branch on delay accounting availability.
if grep -q '?unavailable?' <<<"$out"; then
  echo "delay accounting unavailable -> validating ?unavailable? branch"
  grep -Eq '^[[:space:]]*TID[[:space:]]+PRIO[[:space:]]+USER[[:space:]]+DISK READ[[:space:]]+DISK WRITE[[:space:]]+\?unavailable\?' <<<"$out" \
    || fail "missing ?unavailable? column header"
else
  echo "delay accounting available -> validating SWAPIN/IO columns"
  grep -Eq '^[[:space:]]*TID[[:space:]]+PRIO[[:space:]]+USER[[:space:]]+DISK READ[[:space:]]+DISK WRITE[[:space:]]+SWAPIN[[:space:]]+IO[[:space:]]+COMMAND' <<<"$out" \
    || fail "missing SWAPIN/IO column header"
fi

# 5. No panic in stderr.
if grep -qi 'panic' <<<"$err"; then
  fail "panic detected in stderr"
fi

echo "PASS: root smoke ok"