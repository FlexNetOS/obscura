# Code Intelligence

Prefer AST/code-intelligence tools for Rust call graph questions; use grep/ripgrep only for prose, config, strings, and non-code content.

## Before modifying code

- Changing a function signature: inspect callers/callees first.
- Changing a public type or module boundary: inspect impact first.
- Removing code: prove it is dead or intentionally deprecated.

## Full-auto review grounding

The AI Gatekeeper must judge blast radius, not just diff shape. When preparing a PR, collect enough evidence for the required status check: changed files, impacted files, tests, protected-path hits, and any degraded code-intelligence notes.

## Commands

Use MCP/git-kb when available:

```bash
git kb symbols <file-or-query>
git kb callers <symbol>
git kb callees <symbol>
git kb impact <file>
```

If the index is absent or stale, note the degradation and fall back to direct source inspection.
