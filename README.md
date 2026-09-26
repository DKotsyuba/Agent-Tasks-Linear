# Agent-Tasks-Linear

Rust MCP for an explicit agent workflow using native Linear Projects, Issues and Documents. Work has readable names, requirements, results and artifact links. Agents execute outside this service.

## Tools

| Tools | Responsibility |
|---|---|
| `create_project`, `edit_project` | Permanent product container, optional local repository/link, Runbook and Decisions documents |
| `create_epic`, `edit_epic` | Business requirements, expected outcome, scope and acceptance criteria |
| `create_module`, `edit_module` | Contracts, lead session, branch/worktree, PR and merge report |
| `create_task`, `edit_task` | Local work, checks and commit or non-code artifact |
| `create_atomic`, `edit_atomic` | Independent work, including explicit integration checks |
| `get_context` | Native content, workflow state, children, current reports, Module/PR draft, discrepancies and transitions |
| `list_items`, `search` | Native lists/search with opaque pagination; optional priority ordering for one issue sibling group |
| `save_document` | Native documents attached to a Project or Issue |
| `move_status` | Explicit guarded transition or `check_only` validation |
| `record_review` | Reviewer report, findings, verdict and direct native permalink |
| `record_commits` | Local Git snapshots, current-round results and one progress comment per report |
| `add_comment`, `get_comment`, `resolve_comment` | Role-tagged activity, native direct links, replies and thread resolution |
| `save_project_update` | Explicit native ProjectUpdate with health, reason and body |

Twenty-two tools. Issue titles use one leading `[EPIC]`, `[MODULE]`, `[TASK]` or `[ATOMIC]` marker; project titles are unchanged. Issue priority uses Linear's 0–4 scale. `edit_*` never changes status. Discovery contains the complete input schemas; [the workflow reference](docs/architecture.md) explains the fields and conditions.

MCP tool calls return one concise plain-text block rendered from embedded MiniJinja templates. The agent-facing response has no duplicate `structuredContent`; `isError` reflects the operation outcome. Internal Gateway outcomes and input schemas remain structured. See [the output contract](docs/architecture.md#mcp-result-presentation).

## Cycle

1. Create Project → Epic → Modules → Tasks; Modules may also belong directly to Project. Atomics belong to Project, Epic or Module.
2. Prepare the Epic, then start it. Its current Module membership is permanently fixed.
3. Prepare and start Modules, then their Tasks. Tasks use their Module's checkout.
4. Import code Task results from local commits (or supply legacy commit links), then explicitly complete Tasks. Non-code work uses artifacts. Tasks have no separate review.
5. Review each whole Module. Record its PR merge, then explicitly close it.
6. Create an integration Atomic for the completed Modules, run its scenarios and review the report.
7. Review and close the Epic. New Modules remain outside that Epic, independently queued in Todo or waiting for it to finish.

Start top-down and finish bottom-up. Parent closure never closes children. Native parent/child auto-close must be disabled. Projects can use an absolute local `repository_path` without a hosted URL, or omit both while planning. Local checkout validation uses read-only Git; no Git writes, agent launcher, background watcher, automatic integration creation or scheduler is included.

## Build and connect

```sh
cargo build --release --locked
./target/release/agent-tasks-linear --config /absolute/private/config.toml init-config
```

The new config contains only `listen` (default `127.0.0.1:8777`) and a generated `token`. It is created with mode 0600 and never overwrites an existing file. Version 1 configurations and `at_*` tools are incompatible; create a new configuration explicitly. Old pilot data is not migrated.

Set `LINEAR_API_KEY` or `LINEAR_OAUTH_TOKEN` in the gateway environment, then run:

```sh
./target/release/agent-tasks-linear --config /absolute/private/config.toml doctor
./target/release/agent-tasks-linear --config /absolute/private/config.toml serve
```

HTTP endpoint: `http://127.0.0.1:8777/mcp`, authenticated with the config's bearer token. Run exactly one writer. The stdio bridge connects to that same writer:

```json
{
  "mcpServers": {
    "agent-tasks-linear": {
      "command": "/absolute/path/to/agent-tasks-linear",
      "args": ["--config", "/absolute/private/config.toml", "stdio"]
    }
  }
}
```

Configuration also supports `ATL_CONFIG`. Credentials never belong in tool arguments, issues, documents or source control. Without a Linear token, tool discovery works and data calls report the missing credential.

All connected clients are trusted. `actor`, `reviewer` and `actor_role` describe who performed work; they are not independent authenticated identities. The orchestrator reports its role when closing reviewed work.

## Requests and recovery

Every mutation requires a caller-generated UUIDv4 `request_id` and `actor` session reference. Keep the same arguments and ID when retrying an uncertain request. For creation, the request ID also becomes the native entity ID. `get_context` exposes pending issue updates after a cold restart. See [recovery](docs/recovery.md).

## Checks

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

The opt-in live test writes a disposable project in an explicitly selected team:

```sh
LINEAR_API_KEY_FILE=/absolute/private/linear-api-key \
ATL_LIVE_TEAM_ID=TEAM_UUID \
ATL_LIVE_REPORT=/absolute/private/live-report.json \
cargo test --locked --test live -- --ignored --nocapture
```

The report path makes an interrupted pilot resumable. The pilot verifies real Linear writes, the two-module workflow and cold reads. Its Git artifact/merge reports are explicitly synthetic; it does not create or merge a real PR.

[Checks and limits](docs/FEASIBILITY.md).
