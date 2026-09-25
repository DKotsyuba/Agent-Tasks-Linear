# Verification and limits

## Verification surface

- Stateful HTTP fixtures exercise full native-shaped creates/updates and guarded workflows, rather than mocking the rules themselves.
- Tests cover two-module Epic completion, frozen composition/reopening, independent/waiting Modules, code/non-code Task and Atomic results, whole-Module review/merge, corrections, cancellation, Project integration, stale integration restart, manual child moves across Projects, preserved description sections, check-only behavior, response loss, stale success payloads and cold restart.
- Official MCP client tests cover authenticated HTTP, rejected credentials/origins, tool discovery, missing Linear credentials and the executable stdio bridge.
- GraphQL operations are checked against the pinned official public SDL. This is supplemented by the opt-in real Linear pilot.
- The live pilot uses synthetic Git reports clearly identified in its issue descriptions. It proves MCP/Linear integration, not a real Git delivery or an external agent runtime.

## Deliberate limits

- One trusted writer; no distributed locking, multi-replica guarantees or adversarial-agent defenses.
- Requests read current project data with a 200-page bound. Large workspaces may need narrower fetching later; incomplete data is rejected rather than interpreted as completed work.
- Native API mutations have no general compare-and-swap transaction. Concurrent manual edits during a request cannot be made atomic with attachment persistence. Subsequent reads detect native divergence; avoid editing a card simultaneously with its agent.
- Human field bodies should use level-three or deeper headings; level-two headings identify editable sections.
- Standard English workflow names are reused. Missing/multiple names are configuration errors; MCP never creates duplicate AT states.
- Native parent/child auto-close must remain disabled. Native Git automations or manual status changes can create discrepancies and must be resolved explicitly.
- Actor/session/role names, commits, checks and merges are trusted attribution, not independently authenticated evidence.
- Search is Linear's native per-entity search and can reflect indexing delay. Native cursors are passed through.
- Old v1 tools/configuration/data are not transparently migrated.
