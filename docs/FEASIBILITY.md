# Current checks and limits

## Current contract

The service tracks trusted reports and execution/artifact references. It does not certify code, verify commit contents, prove external artifact availability or require independent approval. The earlier proof-oriented 40-scenario matrix is not the current product contract.

## Checks

- Local workflows cover minimal create/assign/begin/checkpoint/complete, restored activity, optional review, handoff scope, notes and repeat-safe writes.
- Transport checks use the official MCP client over authenticated HTTP and an executable stdio bridge.
- GraphQL operations are validated against the pinned official public SDL. Prior authenticated pilot reads and mutations established the used Linear primitives in a disposable workspace.
- The isolated live_activity_cycle passed on 2026-09-24: create, assign, begin, checkpoint, direct completion, context and a fresh gateway/stdio read retained the agent, checkout and artifact links. Its bindings are synthetic test clients.
- Historical live module/epic pilots describe the previous workflow; they do not substitute for a new activity pilot.

## Intentional limits

- One active writer; no distributed locks or multi-replica failover.
- Requests read a product tree capped at 200 works and 100 attachment pages per work. Selective reads are the next step if measured API use warrants them.
- Records are capped at 256 KiB, prepared recipes at 220 KiB, HTTP bodies at 512 KiB. These are application caps, not measured Linear maximums. Keep large logs as artifact references.
- Note editing uses an internal compare-before-write against accidental concurrent changes, not a native atomic compare-and-swap guarantee.
- Bootstrap and binding changes are administrator commands; binding changes require restart.
- Atomic-under-task is disabled until the native hierarchy is enabled for the product.
- Artifact URLs, commit strings and runtime details are reported references. The server does not fetch arbitrary URLs or execute Git to validate them.
- Existing data and legacy receipt signatures remain readable.
- Source-product migration, automatic credential delivery for every runtime and product-wide lifecycle administration are separate features, not prerequisites for the normal cycle.

Authentication failures, ordinary API errors, missing required objects and uncertain writes remain explicit. A failed network operation is never reported as success.
