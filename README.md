# Agent-Tasks-Linear

A Rust MCP service for tracking trusted agent work in Linear: the task, assigned agent, checkout and run, reported result, and commit/PR/file links. Linear stores the work and provides its human interface. The service does not execute agents or prove their results.

## Normal cycle

1. at_work_create — create a module, task, atomic work or epic.
2. at_assign — assign an agent; a module assignment covers its tasks.
3. at_begin — record the repository, branch, worktree, agent, runtime, run_id and run_url when known.
4. at_checkpoint — leave useful progress and artifact links.
5. at_complete — record a summary and artifacts, and mark the work done.
6. at_resume / at_context — read current activity and earlier reports, including after a restart.

The authenticated recorder is stored separately from the reported agent name. A new begin resets current run IDs and artifacts; old events remain in history. A completion requires a summary and an artifacts array, which may be empty for work without a separate artifact. Commit identifiers are navigation references.

~~~json
{
  "product_id": "PRODUCT_UUID",
  "idempotency_key": "REQUEST_UUID",
  "work_id": "TASK_UUID",
  "summary": "Implemented the requested change",
  "artifacts": [
    {"kind": "git_commit", "locator": "https://example.com/repository/commit/abc1234", "commit": "abc1234"},
    {"kind": "pull_request", "locator": "https://example.com/repository/pull/42"}
  ]
}
~~~

Use that payload with at_complete; replace the UUID placeholders. For at_begin, send product_id, idempotency_key, work_id and execution details. Unknown locations are omitted, never invented.

Plans, contract notes, review, questions and publication are optional. at_submit requests review; at_review_open/report retain review history; at_accept records approval without mandatory independent review. Existing tool names remain available alongside at_complete.

No plan hashes, evidence digests, criterion coverage certificates, mandatory knowledge outputs or result-equivalence checks are required. The existing stored/native state label "accepted" means reported done, not independently certified. Later corrections and new runs preserve history.

## Build and connect

~~~sh
cargo build --release --locked
./target/release/agent-tasks-linear init-config
./target/release/agent-tasks-linear doctor
./target/release/agent-tasks-linear serve
~~~

Configuration defaults to ~/.config/agent-tasks-linear/config.toml; override with --config PATH or ATL_CONFIG. It is created with mode 0600. Keep its signing key: it authenticates stored records and connections, not agent result correctness. Do not distribute the full server configuration to unrelated clients.

Set LINEAR_API_KEY or LINEAR_OAUTH_TOKEN in the gateway environment. Without a token, tool discovery works and data calls report the missing credential. Never put the API key in tool arguments, Linear documents or prompts.

Run one gateway on loopback, default 127.0.0.1:8777. A stdio bridge connects to that same process:

~~~json
{
  "mcpServers": {
    "agent-tasks-linear": {
      "command": "/absolute/path/to/agent-tasks-linear",
      "args": ["stdio", "--binding", "owner"]
    }
  }
}
~~~

HTTP clients use their own provisioned bearer credential at http://127.0.0.1:8777/mcp/BINDING_NAME. Product and assignment scope still apply.

After at_assign, an administrator provisions access with add-binding --name lead --principal AGENT --role lead --product PRODUCT_UUID --assignment ASSIGNMENT_UUID --generation 1, then restarts the gateway. Transfer returns the new generation and records the handoff; it does not stop external processes.

## Initialize a product

1. Run doctor and select a dedicated team with parent/child auto-close disabled.
2. Run bootstrap-plan --team-id TEAM_UUID --name "Product name" --out bootstrap.json.
3. Inspect the reserved-object plan, then run bootstrap-apply --plan bootstrap.json.
4. Use the returned product ID with at_resume.

Reapply the same bootstrap plan after interruption. Existing stored work and earlier history remain readable. The service has no local business database, scheduler or custom task UI.

## Checks

~~~sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
~~~

Local checks cover the activity cycle, trace/history after restart, optional review, handoff/scope, notes, authenticated HTTP/stdio, and replay/recovery.

The opt-in live pilot needs LINEAR_API_KEY, ATL_LIVE_CONFIG (absolute protected config path), and ATL_LIVE_PRODUCT (disposable product UUID):

~~~sh
cargo test --locked --test live live_activity_cycle -- --ignored --exact --nocapture
~~~

ATL_LIVE_REPORT resumes an interrupted report using saved request keys; reconcile pending operations first. Test-role bindings are synthetic clients, not independent agents doing development.

See [recovery](docs/recovery.md), [architecture](docs/architecture.md), and [current limits](docs/FEASIBILITY.md).
