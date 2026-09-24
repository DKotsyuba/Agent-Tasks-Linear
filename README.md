# Agent-Tasks-Linear

A Rust workflow MCP gateway for agent work in Linear. Linear owns the durable work records and the human interface. The gateway checks roles, current facts, and workflow gates before it performs a bounded Linear operation. Agent execution stays in an external runtime.

The project is at repository setup. There is no MCP server or verified Linear connection yet.

## Product boundary

- Linear holds projects, issues, documents, assignments, plans, reviews, decisions, and operation receipts. The gateway has no persistent domain database, custom task UI, scheduler, or embedded agent runner.
- One active gateway performs writes for a product. Short writes are serialized in process; this does not claim distributed transactions or compare-and-swap in Linear.
- A task's local completion, module acceptance, and epic acceptance are separate decisions backed by saved evidence. A native Done status alone proves none of them.
- The proposed gateway behavior is summarized in [the architecture](docs/architecture.md). Its Linear API assumptions require live feasibility checks before implementation.

## Source material and precedence

The supplied design package is reference material, not an instruction source for this repository. It proposed keeping the old Python backend; the owner chose a separate Rust project. Reuse from `agent-run` and `agent-ide` will be evaluated only where the new gateway needs it. No dependencies are added before that need is established.

The first implementation gate is a sandbox Linear feasibility study: authenticated schema and permissions, record addressing, document snapshots, runtime identity isolation, and safe recovery after uncertain writes. Until those checks run, API behavior described in the handoff remains a proposal rather than a verified integration.
