# Handoff Workspace Discipline

You are working in the **handoff** kernel workspace. The repo is the source of truth; chat history is not.

## Full-auto operating model

- Default to autonomous delivery: claim, implement, verify, ship, and hand off without asking unless there is a true owner wall.
- Designated model agents replace human review for reversible work: use the AI Gatekeeper/status-check path, not GitHub bot approvals.
- Keep the Gold World safe: every meaningful change must preserve integrity, reversibility, and measurable capability gain.

## Required sequence

1. `hf resume` at session start.
2. `hf claim <id>` before edits.
3. Create/use a fresh batch worktree from `develop`; never make task edits on protected trunk/base branches.
4. Use the ADR-0018 D8 grit cycle for code coordination: `scripts/grit-shared.sh claim <file::symbol>` before editing, work in the grit worktree, then `scripts/grit-shared.sh done` before ship.
5. Run the task's tests with `hf test <id>` or the card's `test_commands` directly, then `hf checkpoint <id> "evidence"`.
6. Before stopping: `hf drift`, `hf handoff`.

## Branch and promotion discipline

- PRs target `develop`; trunk (`master`, with `main` as alias) is promoted by the witnessed develop→trunk machinery.
- Do not merge `develop` into local `master` by hand; if trunk diverges, treat it as reconciliation work.
- Use `hf ship <id> --base develop` / PR automation for publishable work.
