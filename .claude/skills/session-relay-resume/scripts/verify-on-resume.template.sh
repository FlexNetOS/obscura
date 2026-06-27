#!/usr/bin/env bash
# verify-on-resume.sh (TEMPLATE) — the fast, fail-closed baseline a resuming session runs FIRST,
# before any feature work, to confirm the tree is green. Copied/seeded into .handoff/loop/ by the
# loop's DISCOVER step and kept in sync with HANDOFF.md:verify_on_resume. Keep it FAST (a gate, not
# the full suite) and STRICT (nonzero exit => the resume halts to NEEDS-HUMAN).
#
# Adapt the checks to the harness. Examples:
#   rust-port:   cargo build --quiet && cargo clippy --quiet -- -D warnings
#   meta-plugin: bash scripts/validate.sh   (+ cargo check on touched crates)
set -euo pipefail
echo "[verify-on-resume] $(date -u +%H:%M:%S) starting baseline…"

# --- harness-specific checks go here (replace this block) ---
# cargo build --quiet
# cargo clippy --quiet -- -D warnings
# bash scripts/validate.sh
# -----------------------------------------------------------

echo "[verify-on-resume] baseline GREEN"
