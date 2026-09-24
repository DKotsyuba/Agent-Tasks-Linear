# Agent-Tasks-Linear

A Rust workflow MCP gateway for agent work in Linear. Linear owns the durable work records and the human interface. The gateway checks roles, current facts, and workflow gates before it performs a bounded Linear operation. Agent execution stays in an external runtime.

The Rust MCP server runs over authenticated Streamable HTTP on loopback. A stdio bridge connects clients to that same writer. All 27 workflow intents are implemented and role filtered. Local HTTP fixtures exercise the work/review cycle, recovery, scope isolation, knowledge snapshots and transfer. Live Linear behavior remains unverified until an API token and dedicated test team are provided.

## Product boundary

- Linear holds projects, issues, documents, assignments, plans, reviews, decisions, and operation receipts. The gateway has no persistent domain database, custom task UI, scheduler, or embedded agent runner.
- One active gateway performs writes for a product. Short writes are serialized in process; this does not claim distributed transactions or compare-and-swap in Linear.
- A task's local completion, module acceptance, and epic acceptance are separate decisions backed by saved evidence. A native Done status alone proves none of them.
- The proposed gateway behavior is summarized in [the architecture](docs/architecture.md). Its Linear API assumptions require live feasibility checks before implementation.

## Source material and precedence

The supplied design package is reference material. This is a separate Rust implementation. It uses the official Rust MCP SDK (`rmcp`), Tokio, Reqwest, Serde, JSON Schema validation and HMAC-SHA256. Dependency versions are pinned by `Cargo.lock`.

## Build and start

```sh
cargo build --release --locked
./target/release/agent-tasks-linear init-config
./target/release/agent-tasks-linear doctor
./target/release/agent-tasks-linear serve
```

`init-config` creates `~/.config/agent-tasks-linear/config.toml` with mode `0600`, a random signing key and an owner binding. It refuses to overwrite existing credentials and never prints them. Override the path with `--config PATH` or `ATL_CONFIG`. Keep the signing key: existing signed Linear records depend on it.

Without a Linear token, the server supports MCP initialization and tool discovery; data calls return `LINEAR_TOKEN_MISSING`. When ready, set `LINEAR_API_KEY` for a personal API key or `LINEAR_OAUTH_TOKEN` for OAuth **in the gateway process environment**, then restart `serve`. Do not put the API token in tool arguments, documents, repository files, or client prompts.

Use exactly one `serve` process for a product. The default address is `127.0.0.1:8777`. The occupied port rejects a second local instance at that address; this is not distributed fencing across machines or alternate ports.

## Connect an MCP client

For a stdio client, use the absolute installed binary path and these arguments:

```json
{
  "mcpServers": {
    "agent-tasks-linear": {
      "command": "/absolute/path/to/agent-tasks-linear",
      "args": ["stdio", "--binding", "owner"]
    }
  }
}
```

Start `serve` first. The bridge reads its private binding credential from configuration; it never starts a second writer. HTTP clients can instead connect to `http://127.0.0.1:8777/mcp/owner` with that binding's bearer token from the protected configuration. Requests validate both bearer authentication and Host/Origin.

## Initialize a Linear product

1. Run `doctor` with the API token present to list native team IDs and unsafe auto-close settings.
2. Choose a dedicated test team with parent/child auto-close disabled.
3. Run `bootstrap-plan --team-id TEAM_UUID --name "Product name" --out bootstrap.json`. This is offline and reserves all creation IDs.
4. Inspect the plan, then run `bootstrap-apply --plan bootstrap.json`. This creates the Initiative, general Project, control Issue, workflow states, classification labels and signed product records. Reuse the same plan if interrupted. A failed read never becomes permission to create a new object.
5. Use the returned `product_id` with `at_resume`. See [feasibility and limits](docs/FEASIBILITY.md) before using production work.

## Bind a worker or reviewer

After `at_assign` or `at_review_open` returns the assignment ID and generation:

```sh
agent-tasks-linear add-binding --name lead --principal lead-rust --role lead \
  --product PRODUCT_UUID --assignment ASSIGNMENT_UUID --generation 1
```

Restart the gateway, then connect the worker with `stdio --binding lead`. Give the independent reviewer a distinct principal and binding using role `reviewer`. After transfer, issue a new binding using the returned generation. Stale credentials cannot resume the former assignment, even by replaying a successful receipt. Root and owner bindings cannot call independent review reporting tools.

## Workflow contracts and checks

Refresh expected tokens with `at_context(section="work")`. Mutations require an idempotency key and current `expected.work_revision`; plan-sensitive actions also require `plan_hash`, executor actions require `assignment_generation`. The same key cannot name another actor, tool or payload.

The result `subject_hash` is SHA-256 of canonical JSON for the exact `artifacts` array: sorted object keys, compact UTF-8 encoding, original array order, integers only. Evidence must name this subject and its covered criteria. Reports attest external tests and artifacts; the gateway does not run code or fetch arbitrary artifact URLs.

`at_reconcile(mode="inspect")` is read-only. After an ambiguous write, inspect the original operation key before retrying. Recovery verifies recorded IDs and effects; it leaves a started native create unresolved if the object cannot yet be found. Never replace an uncertain request with a fresh idempotency key.

```sh
cargo test --locked
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
```

Tests include a real MCP HTTP client and executable stdio bridge. HTTP fixtures simulate Linear persistence and a lost mutation response across a fresh gateway instance. They are not evidence of live Linear permissions or behavior.
