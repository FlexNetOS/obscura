#!/usr/bin/env bash
# differential-drive.sh — handoff-central LIVE differential-drive verification harness.
#
# The relay-#134 lesson (harness-agent-rs, owner-relayed 2026-06-21): a LIVE differential drive
# — run the REAL binary/CLI and DIFF its actual output against an expectation — caught what
# 1000+ green unit tests missed on every provider binding. This harness institutionalizes that
# method as a fleet-deployable handoff action workflow, enforcing the FAIL-OPEN doctrine the
# kernel already lives by (LESSONS L7–L10): green is not proof; cases-run must be > 0; ABSENCE
# is a FAILURE, never a silent pass.
#
# CONTRACT — define cases in scripts/differential-drive.cases.sh (sourced if present). Each case:
#     drive "<name>" "<command>" "<expected-substring>"
# A case PASSES iff <command> exits 0 AND its combined stdout+stderr contains <expected-substring>
# (an empty expected-substring asserts exit 0 only). The harness then:
#   - asserts total cases > 0 (fail-closed: no cases / no cases file => FAIL, actionable message),
#   - emits a libtest-compatible `test result:` summary line so `hf test` COUNT-verifies the run
#     (the tests-ran>0 completion gate, HFTASK-0045/0063) instead of trusting the exit code alone,
#   - exits non-zero if any case fails OR zero cases ran.
#
# Deployed verbatim fleet-wide by scripts/handoff-loop-init.sh::deploy_diff_drive (HFTASK-0065).
# A freshly-deployed repo has NO cases file yet, so an explicit invocation fails closed until it
# authors real differential cases — forcing adoption, never a quiet pass.
set -uo pipefail

DD_PASS=0
DD_FAIL=0
DD_TOTAL=0
DD_FAILED_NAMES=()

# drive NAME COMMAND [EXPECTED_SUBSTRING]
# Runs COMMAND, capturing combined stdout+stderr; PASS iff exit 0 and (no expectation, or the
# output contains EXPECTED_SUBSTRING as a fixed string).
drive() {
  local name="$1" cmd="$2" expect="${3:-}"
  DD_TOTAL=$((DD_TOTAL + 1))
  local out code
  out="$(eval "$cmd" 2>&1)"
  code=$?
  if [ "$code" -eq 0 ] && { [ -z "$expect" ] || printf '%s' "$out" | grep -qF -- "$expect"; }; then
    DD_PASS=$((DD_PASS + 1))
    echo "  ✓ $name"
  else
    DD_FAIL=$((DD_FAIL + 1))
    DD_FAILED_NAMES+=("$name")
    echo "  ✗ $name (exit=$code; expected substring: '${expect}')"
    printf '%s\n' "$out" | head -20 | sed 's/^/      | /'
  fi
}

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CASES="$SELF_DIR/differential-drive.cases.sh"

echo "[differential-drive] LIVE differential verification — drive the REAL binary, diff its output"
if [ -f "$CASES" ]; then
  # shellcheck disable=SC1090
  source "$CASES"
else
  echo "::error::no differential cases at scripts/differential-drive.cases.sh — FAIL-CLOSED"
  echo "Author live differential cases (drive your real CLI/binary and diff its output) in"
  echo "scripts/differential-drive.cases.sh, e.g.:"
  echo "    drive \"usage exposes the CLI contract\" \"\$YOUR_BIN --help 2>&1 || true\" \"expected-text\""
  echo "Absence of cases is a failure, never a silent pass (relay-#134 / FAIL-OPEN doctrine)."
  echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;"
  exit 1
fi

if [ "$DD_TOTAL" -eq 0 ]; then
  echo "::error::cases file present but defined 0 cases — FAIL-CLOSED (cases-run must be > 0)"
  echo "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out;"
  exit 1
fi

if [ "$DD_FAIL" -eq 0 ]; then
  echo "test result: ok. $DD_PASS passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;"
  echo "[differential-drive] PASS ($DD_PASS/$DD_TOTAL live cases)"
  exit 0
else
  echo "test result: FAILED. $DD_PASS passed; $DD_FAIL failed; 0 ignored; 0 measured; 0 filtered out;"
  echo "[differential-drive] FAIL (${DD_FAILED_NAMES[*]})"
  exit 1
fi
