# Destructive Command Safety

Destructive commands can lose work or corrupt the continuity baseline. Fail closed.

## Before destructive operations

1. Make a witnessed checkpoint: `hf checkpoint <id> "before risky change"`.
2. Preview first: `git clean -nd`, `git diff --name-status`, or a dry-run flag.
3. Target precisely; never blanket-delete worktrees, branches, `.handoff`, `.kb`, `.grit`, `.github`, or `.claude` state.
4. Verify merge status before branch/worktree cleanup. Keep retained batch worktrees until a verified PR merge or explicit reconcile.

## Blocked by agent guard

- `git push --force` (use proper forward integration; no cherry-pick shortcuts in this workspace)
- `git reset --hard`
- `git clean -fd`
- `rm -rf` on repo roots or state/control directories

## Recovery

Use `git reflog`, `git worktree list`, `hf drift`, and `hf doctor`. Preserve failure evidence; do not hide it with cleanup.
