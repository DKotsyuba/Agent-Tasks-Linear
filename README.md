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
| `get_context` | Native content, workflow state, children, current reports, Module/PR draft, discrepancies and transitions; `view=lead/reviewer` role views, `detail=brief/full` body depth, `section` reads one named Document section |
| `get_overview` | Complete Project overview, change cursors, unpublished update draft and explicit drift failures |
| `list_items`, `search` | Native lists/search with opaque pagination; `list_items(type: team)` for team discovery, `list_items(type: project, repository_path: ...)` to find a Project by local checkout identity, optional priority ordering for one issue sibling group, document search project/currentness scoping |
| `save_document` | Native documents attached to a Project or Issue; guarded whole-body or single-section replace, hide/unhide, under an `expected_updated_at` precondition |
| `upload_file`, `list_files`, `get_file` | Native file attachments: upload one local host file, list a work item's user artifacts, download one artifact to an absolute host destination |
| `move_status` | Explicit guarded transition or `check_only` validation |
| `record_review` | Reviewer report, findings, verdict and direct native permalink |
| `record_commits` | Local Git snapshots, current-round results and one progress comment per report |
| `add_comment`, `get_comment`, `resolve_comment` | Role-tagged activity, native direct links, replies and thread resolution |
| `save_project_update` | Explicit native ProjectUpdate with health, reason and body |

Twenty-five tools. Issue titles use one leading `[EPIC]`, `[MODULE]`, `[TASK]` or `[ATOMIC]` marker; project titles are unchanged. Issue priority uses Linear's 0–4 scale. `edit_*` never changes status. Discovery contains the complete input schemas; [the workflow reference](docs/architecture.md) explains the fields and conditions.

## Role skills

Two concise role skills ship as committed repository files: `skills/agent-tasks-linear-orchestrator/` for whoever coordinates the project, and `skills/agent-tasks-linear-module-lead/` for each persistent module lead. Install them through your agent's supported skill mechanism by referencing or copying those committed directories into its skills location; no plugin or framework is added. Each skill describes its role cycle only and points to the discovery mini-docs for exact call shapes.

MCP tool calls return one concise plain-text block rendered from embedded MiniJinja templates. The agent-facing response has no duplicate `structuredContent`; `isError` reflects the operation outcome. Internal Gateway outcomes and input schemas remain structured. See [the output contract](docs/architecture.md#mcp-result-presentation).

## Entering a Project manually

From an existing local checkout, look for its Project before creating a new one: `list_items(type: project, repository_path: <absolute checkout path>)` matches by canonical common Git directory, so the primary checkout and any of its linked worktrees resolve to the same match. An empty result with a real answer (not an error) means no native Project is linked yet. `list_items(type: team)` lists the workspace's teams so `create_project` has a real `team_id` without guessing an internal ID. A found Project's `get_context(type: project, detail: brief)` gives its repository path/url, teams and document routes in one read. This is a manual, one-checkout-at-a-time flow, not a migration importer: existing Space projects and their data are untouched and out of scope for these tools.

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
cargo fetch --locked
cargo build --release --locked
./target/release/agent-tasks-linear --config /absolute/private/config.toml init
```

`init` (alias of the older `init-config`) creates a config containing only `listen` (default `127.0.0.1:8777`) and a generated `token`, with mode 0600; it never overwrites an existing file. Version 1 configurations and `at_*` tools are incompatible; create a new configuration explicitly. Old pilot data is not migrated.

Local, credential-free diagnostics (exit 0 without any Linear key or network; `--json` for machine output):

```sh
./target/release/agent-tasks-linear --config /absolute/private/config.toml doctor
./target/release/agent-tasks-linear --config /absolute/private/config.toml config check
```

`doctor --online` additionally verifies the authenticated viewer and requires `LINEAR_API_KEY` or `LINEAR_OAUTH_TOKEN` in the environment. Then run the single writer:

HTTP endpoint: `http://127.0.0.1:8777/mcp`, authenticated with the config's bearer token. Run exactly one writer. `mcp` runs the stdio bridge to that same writer (`stdio` remains a legacy alias):

```json
{
  "mcpServers": {
    "agent-tasks-linear": {
      "command": "/absolute/path/to/agent-tasks-linear",
      "args": ["--config", "/absolute/private/config.toml", "mcp"]
    }
  }
}
```

Configuration also supports `ATL_CONFIG`. Credentials never belong in tool arguments, issues, documents or source control. Without a Linear token, tool discovery works and data calls report the missing credential.

All connected clients are trusted. `actor`, `reviewer` and `actor_role` describe who performed work; they are not independent authenticated identities. The orchestrator reports its role when closing reviewed work.

## Requests and recovery

Every mutation requires a caller-generated UUIDv4 `request_id` and `actor` session reference. Keep the same arguments and ID when retrying an uncertain request. For creation, the request ID also becomes the native entity ID. `get_context` exposes pending issue updates after a cold restart. See [recovery](docs/recovery.md).

Schemas, mini-docs and examples in this repository describe its coordinated release contract. An installed server accepts a newer input only once its runtime implements it: check the installed discovery descriptions for the capability — for example `detail` on `get_context`, the `handoff` comment kind, or permalinks on reference fields — before relying on it. UUID references and `get_context` Issue URLs work on every v2 runtime.

## Checks

```sh
cargo fetch --locked
cargo xtask check
```

The single gate covers formatting, Clippy (default and all-features), tests (default and all-features), rustdoc, the schema-first contract check against real-binary discovery/dispatch, and family structural checks. Supply-chain checks: `cargo deny --locked check`.

The opt-in live test writes a disposable project in an explicitly selected team:

```sh
LINEAR_API_KEY_FILE=/absolute/private/linear-api-key \
ATL_LIVE_TEAM_ID=TEAM_UUID \
ATL_LIVE_REPORT=/absolute/private/live-report.json \
cargo test --locked --test live -- --ignored --nocapture
```

The report path makes an interrupted pilot resumable. The pilot verifies real Linear writes, the two-module workflow and cold reads. Its Git artifact/merge reports are explicitly synthetic; it does not create or merge a real PR.

[Checks and limits](docs/FEASIBILITY.md).
