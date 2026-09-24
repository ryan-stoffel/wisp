# Decision records

A decision that affects more than one issue gets a record here. Records supersede `docs/PLAN.md` where they differ.

| Record | Decision |
| --- | --- |
| [0001](0001-ci-before-product-code.md) | CI is built before product code, against a stand-in Electron app |
| [0002](0002-editor-fork-strategy.md) | The editor is upstream Code - OSS at a pinned tag plus a patch series |
| [0003](0003-naming.md) | Wisp, `wisp`, and `wispd` |
| [0005](0005-shared-context-folder.md) | Shared context is a daemon-owned folder outside the repo |

Reserved: 0004 (#15).

Numbers are assigned in order. Take the next free number when you start the record, add a row to this table in the same PR, and link the record from its issue.

## Template

```markdown
# NNNN: Title

- Status: accepted | superseded by NNNN
- Date: YYYY-MM-DD
- Issue: #n

## Context

What forces the decision.

## Decision

What we will do.

## Consequences

What becomes easier, what becomes harder, and what follows from it.
```
