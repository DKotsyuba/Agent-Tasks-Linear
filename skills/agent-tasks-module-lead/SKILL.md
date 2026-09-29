---
name: agent-tasks-module-lead
description: "Implement one assigned Linear Module as its persistent lead: read the assignment, run its Tasks and Atomics in the assigned checkout, record verified commits, prepare the PR, hand off for review and fix findings. Use when you receive a Module assignment link or resume an interrupted Module; not for coordinating other modules, reviewing your own work or merging PRs."
metadata:
  version: 1
---

# Module lead cycle

You own one Module and its children, in the assigned worktree and branch only. The orchestrator reviews, merges and closes.

1. Read the assignment: `get_context` with the Module link, `view: lead`, `detail: brief` — scope, contracts, checkout, children, current round and any pending recovery. The brief also routes to Project documents (`list_items(type: document, project_id: ...)`) and, if your checkout's Project isn't already known, `list_items(type: project, repository_path: <your checkout>)` finds it by local Git identity instead of guessing.
2. Start your Tasks explicitly (`move_status`, `actor_role: worker`) once the Module allows it. Only the orchestrator starts Atomics — ask them when your Atomic is ready.
3. Implement in the assigned checkout only; preserve peer changes and keep the smallest sufficient change.
4. Verify locally, then commit with standalone `Result:` and `Checks:` sections in each commit message (`Notes:` optional).
5. `record_commits` the exact hashes — it fills result/checks and journals one progress comment per report — then complete each Task. Tasks have no separate review.
6. Prepare the real PR, set `pr_url`, move the Module to In Review, and hand off with `add_comment` `kind: handoff`; round and revision are stamped for you.
7. Fix findings after the orchestrator reopens: new commits, new round, never amend recorded history.
8. Report exact state: delivered artifacts, checks run, unfinished steps, real blockers. Accepted review or a ready PR alone is not Done.

## Rules

- Never merge your own PR or close your own Module; never edit outside your Module's scope.
- On `outcome_unknown`, retry the identical `request_id` and arguments; if stuck, report the exact pending request.
- Record knowledge once, in Linear: findings, decisions and continuation context go in a `save_document` (or a `kind: handoff` comment for a short checkpoint), not in Space, for anything assigned here. `search`/`list_items(type: document)` scoped to your Project finds existing material before you write a new one.
- A guarded Document/Project edit needs a fresh read first, not a blind retry: `PRECONDITION_REQUIRED` means supply `expected_updated_at` from a read you just did; a stale-state conflict means someone else's concurrent edit was preserved — read the current content again before writing. `upload_file` replays by `request_id` and intent instead, same as any other mutation retry. `get_file` takes no `request_id` at all — it downloads by `id`/`destination`, is safe to repeat as-is, and only conflicts if that destination already holds different content.
- Ask questions with `add_comment` `kind: question` and a named recipient.
- For argument shapes and conditions, read each tool's mini-doc in discovery; this skill does not duplicate the API.
- Newer inputs (`detail: brief`, `kind: handoff`, permalink references, guarded document edits, document search scoping, the file tools) work only once the installed catalogue advertises them; until then omit `detail`, use `kind: progress` and pass UUIDs.
