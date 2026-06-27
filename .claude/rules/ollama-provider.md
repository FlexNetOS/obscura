# Local/Lower-Cost Provider Discipline

When using a local or lower-cost model, preserve the same full-auto contract while reducing token burn.

1. Start from source truth: `hf resume`, task cards, ADRs, and relevant code.
2. Use `rtk` for large logs and summarize before handing context to larger models.
3. Split work into bounded sidecars: research, verification, doc/rule updates, and independent file scopes.
4. Escalate only the hard judgment/code-review slice to a stronger model; keep mechanical edits and searches local.
5. Never accept absence of failure as proof. Require positive artifacts: tests run, diffs, check names, and witnessed ledger events.
