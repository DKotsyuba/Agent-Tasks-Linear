# Architecture

Agent-Tasks-Linear is a Rust workflow MCP gateway over Linear. Linear is the only durable store for work, plans, assignments, decisions, reviews, and operation receipts. Humans use the Linear interface. The gateway exposes bounded workflow actions to agents and checks each action against current Linear facts and authenticated scope.

## Work and acceptance

- A product is a Linear Initiative with a permanent general Project. An epic is a Project; a module is an Issue; a task is its sub-issue. Standalone modules and atomic work live in the general Project. Service records for entities without Issue attachments use dedicated control or companion Issues.
- A module lead keeps responsibility across its tasks. Assignment, a runtime attempt, and permission to write in a worktree are distinct facts. Transfer requires a known writer stop and explicit recovery.
- Published plans and knowledge snapshots bind exact versions. Native edits to a draft do not silently replace active obligations.
- Task completion, module acceptance, epic composition acceptance, and integration are separate steps. Review findings persist until an independent reviewer verifies a fix. Native status changes without the corresponding record are drift.

## Gateway boundary

- Each request authenticates its principal, reads authoritative facts from Linear, checks workflow rules, and performs only the operation allowed for that role and scope.
- One active gateway serializes short writes for a product. Mutations record an exact intent and effects in Linear so restart can reconcile uncertain outcomes. Partial GraphQL or network results remain unknown until verified; a repeated request cannot create a second logical result.
- Reads return bounded context with explicit continuation or incompleteness. Search results are filtered by product scope before titles or excerpts are exposed.
- Agent execution, Git operations, language intelligence, and model selection remain outside this gateway. There is no local domain database, persistent queue, custom UI, or general GraphQL escape tool.

## Feasibility gate

The write path is implemented and exercised against an HTTP fixture. Before using it for real work, test the authorized Linear schema and permissions in an isolated workspace. Verify attachment addressing, document snapshot round trips, parent depth, runtime identity separation, and recovery after ambiguous writes. These behaviors have not been verified in a live Linear workspace for this repository. [Current evidence and limits](FEASIBILITY.md) separate local checks from live acceptance.
