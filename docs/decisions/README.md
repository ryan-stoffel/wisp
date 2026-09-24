# Decision records

A decision that affects more than one issue gets a record here. Records supersede `docs/PLAN.md` where they differ.

| Record | Decision |
| --- | --- |
| [0001](0001-ci-before-product-code.md) | CI is built before product code, against a stand-in Electron app, which #38 retired for the real `Wisp.app` |
| [0002](0002-editor-fork-strategy.md) | The editor is upstream Code - OSS at a pinned tag plus a patch series |
| [0003](0003-naming.md) | Wisp, `wisp`, and `wispd` |
| [0004](0004-subscription-providers.md) | Subscriptions run through each vendor's official CLI; wisp never handles consumer credentials |
| [0005](0005-shared-context-folder.md) | Shared context is a daemon-owned folder outside the repo |
| [0006](0006-release-versioning-and-packaging.md) | Versions come from release tags; releases are arm64-only and ad-hoc signed until #7; bundle id `io.github.ryan-stoffel.wisp` |
| [0007](0007-editor-wispd-protocol.md) | The editor speaks JSON-RPC 2.0 as newline-delimited JSON through `wispd attach`, locally or over the user's `ssh`; types come from the `wisp-protocol` crate |
| [0008](0008-editor-overlay.md) | `editor/product.json` and `editor/overlay/` reach the editor tree as one commit under the patches, never as a patch |

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
