# Feasibility and current limits

## Evidence

| Surface | Evidence | Status |
|---|---|---|
| Rust MCP server and stdio bridge | Real official MCP client against the local authenticated server and bridge executable | Locally verified |
| Input schemas and roles | 27 embedded tool schemas; role-filtered discovery and invalid-input rejection | Locally verified |
| Module workflow | Real Linear task-local completion, submission, separately provisioned reviewer identity and owner acceptance | Live synthetic pilot passed |
| Epic workflow | Native Project plus companion, exact accepted component candidate, composition review, epic acceptance and integration record | Live synthetic pilot passed |
| Write recovery | Local failure injection plus real uncertain outcomes; exact replay and mismatch rejection; new CLI gateway in an empty working directory through stdio | Verified with the operator limitation below |
| Knowledge and transfer | Real snapshots, contract access/gates, owner-PAT attribution rejection, transfer, revoked generation and recovery confirmation; local tamper regressions | Pilot passed |
| GraphQL operations | Automated field/argument/variable validation against the official public schema snapshot | Statically verified |
| Linear workspace and permissions | Read-only doctor and live bootstrap of Initiative, Projects, Issues, states, labels, Documents and attachments | Verified in an authorized disposable workspace |
| Attachment UUID/upsert/addressing and limits | Reserved UUIDs and namespaced fragments worked in the live pilot; four direct upsert/read probe revisions matched | Basic behavior verified; maximum size unmeasured |
| Canonical Markdown round-trip | Plain notes and generated structured snapshot documents round-tripped exactly | Observed samples verified; broader normalization still open |
| Independent agent runtime | Separate protected test bindings were provisioned with the production configuration code; one harness exercised the roles | Real independent-agent execution remains unverified |
| Full 40-scenario specification matrix | Only the concrete scenarios listed above are exercised locally or against live Linear | Incomplete |

The public schema was downloaded from [Linear's official repository](https://raw.githubusercontent.com/linear/linear/master/packages/sdk/src/schema.graphql) on 2026-09-24. Its SHA-256 is `bf6ccbb9143591af0a24d4f2f58f71a44a0f020d28ddca4e96c7c1e1b38dbb37`. Static compatibility does not establish the permissions or behavior of an authorized workspace.

Primary API references: [authentication and errors](https://linear.app/developers/graphql), [attachment upsert and metadata](https://linear.app/developers/attachments). The implementation uses static public operations and rejects GraphQL partial errors, empty mutation data and unconfirmed success.

Authenticated introspection captured a 1,214-type name map and all 19 input types referenced by the adapter operations. One unrestricted full-schema query exceeded Linear's complexity limit, so it was replaced with bounded reads. The pinned public SDL remains the complete static schema fixture. The live API also confirmed its `INPUT_ERROR` / `invalid input` / `Entity not found: …` read-error form, which is now distinguished from other invalid input.

## Recovery observations

Four live intents returned an uncertain outcome during the pilot. Three were completed through the normal inspect/resume tool using their original keys and IDs. One timed-out question update left the last-operation work-head marker unconfirmed while all workflow fields were unchanged; in the disposable workspace, an explicitly authorized operator completed that exact saved marker and then used normal reconciliation. This is not evidence that every interrupted write can recover automatically.

Record read-back uses bounded read-only retries for an older valid revision; writes are not blindly retried. Immediate duplicate verification after an already verified record upsert was removed. Provisional knowledge/plan versions do not grant scope, work-head rollback cannot allocate another healthy lead, and a late older assignment record cannot reactivate revoked credentials. Dedicated local regressions cover these conditions.

## Deployment and scale

- One active writer per product is an operational requirement. Per-request serialization is process local. There is no distributed CAS, lease, or multi-replica guarantee.
- A request loads a bounded product tree from Linear into disposable memory. The initial cap is 200 committed works and 100 attachment pages per work. Large products need selective dependency reads before this implementation is suitable.
- The initial record cap is 256 KiB; a prepared recipe must be below 220 KiB and incoming HTTP bodies below 512 KiB. These are application safety caps, **not measured Linear limits**. Oversized records are rejected; automatic payload spillover to Documents is not implemented.
- Native Document round trips must preserve the exact prepared content. A normalization difference produces an explicit unresolved outcome, and requires an observed normalization policy before live use.
- Bootstrap uses a saved non-secret plan until completion. After successful bootstrap, all workflow continuation facts live in Linear. Credentials and signing keys remain deployment configuration.
- Bootstrap and binding changes are administrative commands. Updating bindings requires a gateway restart. Product lifecycle administration, data migration and automated provider-specific binding delivery are not implemented in this first build.
- Atomic-under-task creation remains gated until live hierarchy behavior is verified. Independent review defaults to every submitted result. Delta reports must explicitly restate current coverage and identify applicable evidence; an old incomplete round is never silently promoted to full coverage.
- Evidence is attributed external observation. The gateway does not independently prove that a referenced Git commit, file or external URL still exists. It does verify signed Linear facts and native snapshot content used by its gates.

## First live acceptance

The disposable pilot is complete for the scenarios listed above. Before production rollout, finish the remaining 40-scenario matrix and failure injection around every external step; verify attachment identity across native rename, actual metadata limits, deeper atomic hierarchy, broader Markdown normalization and real runtime credential isolation. Existing-product migration and multiwriter deployment are outside this build's acceptance.
