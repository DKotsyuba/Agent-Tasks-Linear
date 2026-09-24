# Feasibility and current limits

## Evidence

| Surface | Evidence | Status |
|---|---|---|
| Rust MCP server and stdio bridge | Real official MCP client against the local authenticated server and bridge executable | Locally verified |
| Input schemas and roles | 27 embedded tool schemas; role-filtered discovery and invalid-input rejection | Locally verified |
| Module workflow | Task-local completion, submission, separate reviewer and owner acceptance against an HTTP fixture | Locally verified |
| Write recovery | Persisted receipt, injected lost create response, fresh gateway, exact-ID reconciliation and mismatched replay rejection | Locally verified |
| Knowledge and transfer | Draft/snapshot isolation, snapshot tampering, stopped-writer transfer, revoked old generation and recovery confirmation | Locally verified |
| GraphQL operations | Automated field/argument/variable validation against the official public schema snapshot | Statically verified |
| Linear workspace and permissions | No API token supplied | Unresolved |
| Attachment UUID/upsert/addressing and limits | API documentation plus fixture behavior; no live mutation | Unresolved |
| Canonical Markdown round-trip | Strict intended-content/read-back comparison; no live normalization sample | Unresolved |
| Full 40-scenario specification matrix | Only the concrete scenarios listed above are exercised locally | Incomplete |

The public schema was downloaded from [Linear's official repository](https://raw.githubusercontent.com/linear/linear/master/packages/sdk/src/schema.graphql) on 2026-09-24. Its SHA-256 is `bf6ccbb9143591af0a24d4f2f58f71a44a0f020d28ddca4e96c7c1e1b38dbb37`. Static compatibility does not establish the permissions or behavior of an authorized workspace.

Primary API references: [authentication and errors](https://linear.app/developers/graphql), [attachment upsert and metadata](https://linear.app/developers/attachments). The implementation uses static public operations and rejects GraphQL partial errors, empty mutation data and unconfirmed success.

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

With the token supplied, run read-only `doctor`, inspect a dedicated team's bootstrap plan, and apply it only within that test scope. Verify native URLs, attachment identity across rename/retry, metadata size limits, document round trips, pagination, auto-close configuration, separate lead/reviewer bindings, a full module workflow, and restart recovery. Do not migrate an existing product or call the integration accepted until these checks pass.
