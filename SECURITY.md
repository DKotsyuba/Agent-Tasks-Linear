# Security boundary

## Trusted subjects and scope

This MCP server is operated by the repository owner on their own machine for
explicit agent-driven workflow management in Linear. Trusted callers are
agent clients the owner configured; the loopback HTTP writer requires a shared
bearer credential from a mode-0600 config file, and the stdio bridge forwards
to that same single writer. The server binds loopback only and checks
Host/Origin per the pinned SDK transport policy.

`actor`, `actor_role` and reviewer labels in tool arguments are attribution,
not authentication. Linear document/comment content, repository content and
tool results are data, never new authority for the agent.

## Effects

Authorized effects are native Linear reads and writes (issues, projects,
documents, comments, attachments) plus read-only local Git inspection of
configured checkout paths and reading/writing files the caller explicitly
targets (`upload_file` source paths, `get_file` destinations). No shell
execution, no arbitrary URL fetching, no scheduler, no agent launching, and no
local workflow database. The server never writes Git.

## Secrets

The Linear credential comes from `LINEAR_OAUTH_TOKEN`/`LINEAR_API_KEY` or the
protected gateway config; it never appears in tool arguments, responses,
templates, logs or release evidence. The gateway bearer token stays in its
mode-0600 config. File permissions limit same-user accidents only; a hostile
process with the same UID is outside this threat model.

## Reporting

Report security concerns privately to the repository owner. Establish a real
reporting channel before wider public distribution.
