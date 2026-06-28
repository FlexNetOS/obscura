# Knowledge Management

Maintain handoff's planning plane and execution plane together, but never confuse authority: `hf status`/ledger truth wins over `.kb` prose.

## handoff's local `.kb` (ADR-0018 D7)

handoff has its OWN durable `.kb/`. Load context before non-trivial work:

```bash
git kb list --path context/
git kb checkout --path context/
git kb board
```

If no task exists for non-trivial work, create the kb task first; for existing HFTASK cards, use the card/ledger and mirror progress back.

## Durable state residency

Commit durable text; ignore rebuilt caches:

- Commit: `.kb/store/**`, `.handoff/ledger.events.jsonl`, task cards, ADRs, rendered handoff views when policy says they are durable.
- Ignore: `.kb/.cache/**`, `.kb/workspaces/**`, `.kb/config.toml`, `.handoff/ledger.db*`, `.grit/**`, `target/**`.

## Two-way seam, one-way authority

- **IN:** `hf task mint --from-kb <slug>` mints a witnessed task card.
- **OUT:** `hf claim` / `checkpoint` / `done` / `release` mirror execution progress to kb.
- `.kb` informs planning only. It never overrides ledger/Git truth.

## Progress evidence

- Add progress lines as you work.
- Commit messages should reference the card or kb task.
- Do not mark done without tests or an explicit witnessed waiver.
- Store lessons in ICM when mandatory triggers fire.
