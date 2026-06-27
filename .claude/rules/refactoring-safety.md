# Refactoring Safety

Refactors must be small, witnessed, and reversible.

## Required checks

| Before doing this | Run first |
| --- | --- |
| Change a function signature | caller/callee impact check |
| Rename a public symbol | reference/caller check |
| Modify an interface/type field | impact check and targeted tests |
| Delete a symbol | dead-code proof or explicit deprecation rationale |
| Touch protected paths (`.github/`, policy, ADRs, schemas, `.claude/rules/`) | gatekeeper/steward evidence |

## Workflow

1. Claim the task and grit-lock the symbol/file scope.
2. Make the smallest coherent patch.
3. Update tests and docs alongside code.
4. Run `cargo test --workspace` or a narrower positively-counted test, then `hf test <id>` when applicable.
5. Check `hf drift` before handoff.
