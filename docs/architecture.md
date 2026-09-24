# Architecture

Agent-Tasks-Linear tracks trusted agent activity. Independent proof of a reported result's correctness is outside this MCP service.

## Stored facts

- Product: Initiative, general Project, control Issue.
- Epic: Project and companion Issue.
- Module/task/atomic: Issue with an explicit parent and agent assignment.
- Activity: attributed begin, checkpoint, result, transfer and optional review events.
- Execution: repository, branch, worktree, agent, runtime, run ID and URL when reported.
- Artifacts: navigable commit, pull request, file, document or other result references.
- Knowledge: editable native Documents and optional separate publication copies.

The work head contains a compact activity summary. Earlier events remain available through history. Human-readable comments show execution location and artifacts directly in Linear. Starting another run resets current run-specific facts without deleting earlier results.

## Boundaries

The authenticated principal determines authority. A reported external agent name is attribution, not a credential. Workers stay within their assignment subtree. Explicit handoff changes the generation; old bindings become stale. Handoff does not claim to stop an external process.

Completion requires neither a plan nor passed evidence, content hashes, independent review or acceptance of every child. Closing a parent does not silently close children. Optional plans and review record context and discussion.

The old "accepted" state and legacy tool names remain readable for compatibility. New completion records have basis=agent_report and mean reported done. Legacy hash/evidence fields are descriptive metadata.

## Reliable writes

One active gateway serializes intents. Each mutation saves its request key, reserved IDs and finite effects in Linear before applying them. Same-key retries return the recorded outcome; using that key for different input is rejected. Internal hashes and record MACs protect stored receipts and credentials, not the truth of agent work.

An uncertain native create is looked up by reserved ID, never replaced blindly. A pending metadata upsert can resume with its identical ID and contents under the single-writer rule. There is no distributed transaction or multiwriter fencing.

Native project/parent moves remain explicit conflicts to avoid writing into a moved work. Native statuses are shown separately from activity; manual UI status cannot fabricate an agent result.

There is no domain database, execution runtime, arbitrary shell tool, URL fetcher or custom UI. The original proof-oriented design package is historical reference; this document describes the current trusted activity contract.
