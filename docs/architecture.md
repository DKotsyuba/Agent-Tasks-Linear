# Native Linear workflow

## Data model

A native Project is a permanent product container. Epic, Module, Task and Atomic are native Issues distinguished by `EPIC`, `MODULE`, `TASK`, `ATOMIC` labels. Existing exact team/workspace labels are reused; missing labels are created. No custom workflow statuses, control Issues, companion Issues or Initiatives are created.

```text
Project
├── Epic
│   ├── Module
│   │   ├── Task
│   │   └── Atomic
│   └── Atomic
├── Module
│   ├── Task
│   └── Atomic
└── Atomic
```

Project creation also creates native `Runbook` and `Решения` documents. Requirements/contracts live in issue descriptions. Further documents are explicit. Native issues are the TODO list; Linear history is the change log.

## Public data fields

Create calls require `request_id`, `actor`, `title`, `project_id` and `team_id`; Task also requires `parent_id`. Issue titles are stored with exactly one leading kind marker (`[EPIC]`, `[MODULE]`, `[TASK]`, `[ATOMIC]`); repeated or wrong recognized markers are normalized, unrelated markers such as `[UI]` are preserved, and a marker without a title is rejected. Project titles remain unchanged. Issue `priority` is native Linear priority: 0 none, 1 urgent, 2 high, 3 medium, 4 low. Omitted create priority is 0; omitted edit priority preserves the native value and 0 clears it. Issue fields may be prepared in Backlog/Todo. Project creation instead requires `team_id`, `title`, `description`, `repository_url` (GitHub HTTPS).

Issue create/edit calls accept a `fields` object. Omitted values are preserved; null removes a nullable value. `work_type` defaults to `code` for Module/Task/Atomic and `non_code` for Epic. Use `non_code` explicitly for document/administrative work and `integration` for an integration Atomic.

| Fields | Meaning |
|---|---|
| `description`, `scope` | Readable explanation and boundaries |
| `business_requirements` | Epic business need |
| `expected_result`, `acceptance_criteria` | Observable outcome and acceptance |
| `required_contract`, `provided_contract` | Module contract description/link or explicit “not required” |
| `lead`, `executor`, `session_url` | Session reference such as `codex:…` or `agent-run:…`, optional real transcript URL |
| `repository_url`, `branch`, `worktree` | Execution checkout; Task inherits from Module |
| `local_check` | Planned local verification |
| `result`, `check_result` | Actual outcome and check summary |
| `commit_url`, `artifact_url` | Code commit or non-code result link |
| `pr_url`, `merge_report` | Module review target and reported merge |
| `after_epic` | Optional prerequisite Epic UUID for a Project-level Module |
| `integration_modules`, `scenarios`, `environment` | Participating Module UUIDs and actual interaction checks |
| `reason`, `duplicate_of` | Retirement reason and original issue URL |

Descriptions have readable Russian level-two section headings. Use level-three or deeper headings inside field values. Unrelated sections/prose remain intact during partial edits. A manually changed description is reported; a content edit can adopt the current recognized fields after full schema validation. Title/priority-only edits do not send or adopt descriptions and preserve review identity, results and revision, including while In Review or Done. Native Markdown escaping, link formatting and `-`/`*` unordered list markers are accounted for without removing unrelated prose. List-marker equivalence does not rewrite fenced or indented code, escaped leading hyphens or thematic breaks; changed words and link destinations still conflict.

## Priority views

`list_items` keeps native pagination by default. `order_by: "priority"` requires an issue, Project, and kind, and optionally scopes to one parent; omitted or null parent means Project root. The complete live sibling group is loaded within the normal page budget, filtered and sorted by priority (1, 2, 3, 4, then 0), native `prioritySortOrder`, and UUID. Its cursor binds the filters and last UUID, so subsequent pages are sorted after the whole group and reject changed filters or missing anchors. `priority` may filter a native priority value 0–4. Priority is advisory ordering and never changes transition conditions or starts work. `get_context` reports sorted peers in the same Project, parent and kind group.

Machine data lives on one small native attachment per managed issue: kind, expected parent/project/status, known children, frozen membership, implementation/review round, current review, integration completion snapshot and any prepared write. Recorded children remain visible to guards if moved to a different native Project. No signatures or proof certificates are used. The attachment links back to the issue. Native dates/history remain available in Linear; MCP does not maintain a second time-in-status system.

## Epic composition

At the first `In Progress` transition, the Epic fixes its Module IDs, including an empty list. The list is never unlocked by reopening. Modules cannot subsequently be attached, created under it or detached through MCP. Canceling a Module preserves membership and history.

Atomics may be added to active Epics. A new out-of-scope Module belongs directly to Project and starts in Todo. It can start independently, or name `after_epic` and wait for that Epic's Done status. The orchestrator starts it explicitly.

## Transition conditions

Normal cycle: Backlog → Todo → In Progress → In Review → Done. Task skips In Review. Draft work may start directly from Backlog when ready. Returning to In Progress is the explicit rework path. Project has no work lifecycle through these tools.

| Kind | In Progress | In Review | Done |
|---|---|---|---|
| Epic | Business requirements, expected result, acceptance; freeze Module list | Children finished, business result recorded, current integration covering delivered Modules | Positive current review; orchestrator |
| Module | Parent Epic In Progress if present; waiting Epic Done; expected result, acceptance, lead, repo, branch, worktree, both contract fields | All Tasks/Atomics finished; PR, implementation result and checks | Positive current review and merge report; orchestrator |
| Task | Module In Progress; expected result, acceptance and local check | Unsupported | Result, checks, commit for code or artifact for non-code |
| Atomic | Parent work In Progress if present; executor, expected result, acceptance, local check; coding checkout (inherited under Module) | Result, checks, commit or non-code artifact | Positive current review; orchestrator |

Completed children may remain Done while parents are still In Progress. Canceled and Duplicate children do not contribute unfinished scope. Parent retirement requires all children to be terminal. Canceled requires a reason; Duplicate also requires an original-work link. No transition cascades to children.

Duplicate is a [system-managed Linear status](https://linear.app/docs/configuring-workflows). Its transition resolves `duplicate_of` to a native Issue and creates an `issueRelationCreate` relation with `type: duplicate`, the retiring issue as `issueId` and the original as `relatedIssueId`. It does not directly assign the reserved state. A retry checks the complete outgoing relation list, preserves a conflicting original link and confirms both relation and native status before finalizing the saved intent. Invalid issue links and self-links are rejected before preparing a new transition.

## Review and rework

`record_review` requires In Review and a Module, Atomic or Epic. It records a native comment with reviewer, summary, findings, artifact links and `accepted`/`changes_requested`. It never changes status. A Task is reviewed only within its Module.

The orchestrator returns work to In Progress after changes are requested. A new work round clears its current results/checks/artifacts and current review. Earlier native reports and history remain. New outputs and a new review are required. Editing reviewed content requires reopening; a Module's `merge_report` can be added after positive review without invalidating it.

GitHub links and merge facts are trusted agent reports. MCP neither executes Git nor contacts GitHub to prove contents. Shared bearer clients are trusted; reported roles are workflow attribution, not separate authorization principals.

## Integration Atomic

The orchestrator creates an Atomic with `work_type: integration` under an Epic or directly under Project. It names at least two Modules, interaction scenarios, environment, expected outcome and local check. Epic integration uses that Epic's Modules.

Start requires participating Modules Done with merge reports. The Atomic stores their work-round/content/completion identities. It tests combined behavior and supplies an artifact report; no new commit is needed if it only runs checks. Its report is reviewed normally.

A changed or reopened Module invalidates earlier integration. Repeat the Atomic explicitly by returning it to In Progress, running its scenarios again and submitting new results/review. A stale integration already In Progress can explicitly restart in that same status with a fresh round. Participating Modules must remain free of native discrepancies through integration review and closure. Epics with multiple delivered Modules require current successful integration coverage before final review.

## Manual changes and failures

Every guarded operation reads fresh Linear state. Status, parent/project, type-label, completion and frozen-membership inconsistencies are reported and block forward progress. Reads do not fix state. Restore structural changes explicitly in Linear; use the documented reopen/edit paths for state/content repair.

Exactly one loopback gateway serializes requests. Stdio clients share it. Native writes across an issue and its attachment are not a distributed transaction: issue edits/transitions persist a prepared update and source snapshot before the native mutation and finalize only after the returned fields match the target. A cold gateway resumes only on an identical explicit request. It finalizes an already applied target, retries an unchanged source, or rejects conflicting native edits without overwriting them.

The service does not protect against a malicious agent editing its native records, launch agents, schedule work, create integration automatically or migrate v1 pilot data.
