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

Create calls require `request_id`, `actor`, `title`, `project_id` and `team_id`; Task also requires `parent_id`. Issue titles are stored with exactly one leading kind marker (`[EPIC]`, `[MODULE]`, `[TASK]`, `[ATOMIC]`); repeated or wrong recognized markers are normalized, unrelated markers such as `[UI]` are preserved, and a marker without a title is rejected. Project titles remain unchanged. Issue `priority` is native Linear priority: 0 none, 1 urgent, 2 high, 3 medium, 4 low. Omitted create priority is 0; omitted edit priority preserves the native value and 0 clears it. Issue fields may be prepared in Backlog/Todo. Project creation instead requires `team_id`, `title` and `description`; both `repository_path` and `repository_url` are optional for planning. A supplied path must be an absolute existing local Git checkout; the optional external URL accepts HTTP(S), including non-GitHub hosts. Legacy URL-only calls remain valid.

Issue create/edit calls accept a `fields` object. Omitted values are preserved; null removes a nullable value. `work_type` defaults to `code` for Module/Task/Atomic and `non_code` for Epic. Use `non_code` explicitly for document/administrative work and `integration` for an integration Atomic.

| Fields | Meaning |
|---|---|
| `description`, `scope` | Readable explanation and boundaries |
| `business_requirements` | Epic business need |
| `expected_result`, `acceptance_criteria` | Observable outcome and acceptance |
| `required_contract`, `provided_contract` | Module contract description/link or explicit “not required” |
| `lead`, `executor`, `session_url` | Session reference such as `codex:…` or `agent-run:…`, optional real transcript URL |
| `repository_path`, `repository_url`, `branch`, `worktree` | Local repository, optional external link and execution checkout; Task inherits from Module |
| `local_check` | Planned local verification |
| `result`, `check_result` | Actual outcome and check summary |
| `commit_url`, `artifact_url` | Code commit or non-code result link |
| `pr_url`, `merge_report` | Module review target and reported merge |
| `after_epic` | Optional prerequisite Epic UUID for a Project-level Module |
| `integration_modules`, `scenarios`, `environment` | Participating Module UUIDs and actual interaction checks |
| `reason`, `duplicate_of` | Retirement reason and original issue URL |

Descriptions have readable Russian level-two section headings. Use level-three or deeper headings inside field values. Unrelated sections/prose remain intact during partial edits. A manually changed description is reported; a content edit can adopt the current recognized fields after full schema validation. Title/priority-only edits do not send or adopt descriptions and preserve review identity, results and revision, including while In Review or Done. Native Markdown escaping, link formatting and `-`/`*` unordered list markers are accounted for without removing unrelated prose. A CommonMark parser identifies actual list boundaries, which remain distinct from escaped literal markers and code contents. Unparsed backticks conservatively disable marker folding. Changed words and link destinations still conflict.

`repository_path` uses the dedicated `Локальный репозиторий` section. Project edits preserve omitted fields and documents; null removes either repository field. Adding a path preserves old prose in `Репозиторий`. Modules and code Atomics outside Modules inherit omitted repository fields at creation; only valid external URLs are inherited from that legacy section. Existing work can be updated explicitly with `edit_module`/`edit_atomic`; later Project edits do not rewrite existing work. Task and nested Atomic context includes the current Module repository path, URL, branch, worktree and lead.

Supplied local paths are validated before create/edit writes. Starting a Module or standalone code Atomic requires a local path or legacy URL. With `repository_path`, readiness also validates its worktree. Normal repositories and linked worktrees are accepted; missing, relative, non-Git and bare directories are rejected. Validation runs only local `git rev-parse --show-toplevel`, without shell interpolation, with bounded output and a two-second process deadline. Git must be installed on the gateway host. Context and check-only transition calls use the same read-only readiness check. URL-only legacy records retain field-based readiness; supplying a local path opts into local checks. Branch remains a required declared field. Planning and non-code work need no repository.

The Rust `git::read_commit(path, hash)` reader accepts an unambiguous hexadecimal object hash (4–64 digits), never a branch or revision expression. Each Git process has a two-second deadline and a 64 KiB stdout limit. It reads the commit object's exact UTF-8 message, resolves the full SHA, and records the canonical common Git directory so linked worktrees share an identity. Git replacement objects and inherited repository overrides are ignored. No Git writes or network operations occur. The message requires a Conventional Commit subject and nonempty `Result:` and `Checks:` sections; `Notes:` is optional. Original text, Git author/date and structured sections are returned as `git::GitCommit`. Checks remain the author's report, not independent verification. Missing/ambiguous objects, malformed messages, unavailable Git, encoding errors and limit failures are explicit.

`record_commits(work_id, commits, actor, request_id)` imports 1–20 concrete hashes for an In Progress code Task or Atomic. It reads the assigned Module worktree (or the standalone Atomic's own worktree); if repository_path is configured, both must share the same canonical common Git directory. Each source is validated before any report write. Reports are appended in requested order and deduplicated by work, round, common directory and full SHA. A new round requires explicit reimport; old reports are retained as history. Imported Result/Checks text fills the ordinary result/check_result fields, with level-two source headings rendered deeper to preserve native section boundaries. Existing field-length limits still apply. The tool never changes status.

`model::LocalGitReport` serializes `round` alongside the flattened `git::GitCommit` fields. `Meta.git_reports` stores ordered immutable history in the existing native attachment; absent fields default to an empty list for legacy records. `Meta::current_git_reports()` and issue context's top-level `git_reports` expose only the current round. Full snapshots, including original_message, remain readable without Git after history rewrites or checkout removal. The normal pending-write path persists snapshots before native description edits and resumes an identical request without re-reading Git. Imported current-round commits allow code Task completion or Atomic submission without commit_url. Manual commit links and non-code artifacts retain their existing path.

`reports::module_report(&Work, &[Work]) -> Result<ModuleReport>` is the pure shared Module composer. Module context exposes its result as `module_report`: summary, reported_checks, notes, tasks_done/tasks_total, excluded_count, source_commits, unfinished, excluded and pr_draft. Results are grouped by child with shared commit content emitted once; each source reference names all contributing work IDs. Current-round imports are used when present, otherwise manual/non-code result fields and artifact links are retained. Native Task statuses determine Done/total; Atomics are reported but not counted as Tasks. Canceled/Duplicate direct children are visible separately and counted in excluded_count. Unfinished work and old-round-only snapshots never count as completed results. Empty or fully excluded Modules may retain their existing manual summary/checks. Missing child metadata/status and native field-size overflow fail explicitly.

Module review readiness uses that same composer and still requires every child finished and a real pr_url. The In Review transition copies its derived summary/checks to native fields through the existing pending-write path, invalidating an old review when the content changes. No second manual report is required. pr_draft is Markdown text only; no PR is created or published. Review acceptance and the real merge report remain required for Module Done.

## Priority views

`list_items` keeps native pagination by default. `order_by: "priority"` requires an issue, Project, and kind, and optionally scopes to one parent; omitted or null parent means Project root. The complete live sibling group is loaded within the normal page budget, filtered and sorted by priority (1, 2, 3, 4, then 0), native `prioritySortOrder`, and UUID. Its cursor binds the filters and last UUID, so subsequent pages are sorted after the whole group and reject changed filters or missing anchors. `priority` may filter a native priority value 0–4. Priority is advisory ordering and never changes transition conditions or starts work. `get_context` reports sorted peers in the same Project, parent and kind group.

Machine data lives on one small native attachment per managed issue: kind, expected parent/project/status, known children, frozen membership, implementation/review round, current review, integration completion snapshot and any prepared write. The attachment is selected by its deterministic UUID, never by title or current placement. Native Duplicate merges transfer attachments to the original issue; `Attachment.originalIssue` preserves their originating issue and takes precedence over current `issue` when validating reads and writes. Each original issue retains its own distinct canonical record. Recorded children remain visible to guards if moved to a different native Project. No signatures or proof certificates are used. The attachment links back to the issue. Native dates/history remain available in Linear; MCP does not maintain a second time-in-status system.

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

`add_comment` writes a native Linear Comment on an Issue, Project or ProjectUpdate. Its `request_id` is the native comment ID, so an identical retry reads the created comment after an uncertain response. A reply's `parent_id` must belong to the same target. `get_comment` accepts a full UUID or a native Linear permalink and returns the comment, root and one native page of replies. Ordinary fragments contain a short comment hash; ProjectUpdate comments use a composite project-update UUID and short comment hash. Lookup scopes by the native target when available and compares the complete returned URL, so a changed Project or update URL cannot select another comment. `list_items(type: comment)` supports native cursors and target/parent filters. `resolve_comment` resolves or reopens a root thread through Linear's native operations. Comments do not change work status or act as review approval.

Activity comments show Kind, Role and Actor on separate lines, with optional Session, Recipient and source links. A question requires a recipient. The typed ActivityRecord projection contains the native ID/URL/target/thread/times, content, and optional review details; ordinary unformatted comments read as notes. The current workflow attachment alone identifies a formal review, so an arbitrary comment saying “accepted” cannot approve work. Code commit imports add one deterministic progress comment per persisted current-round LocalGitReport. If the comment response is lost after the task result is saved, replay of the original import finishes the journal from that snapshot without rereading Git.

`save_project_update` creates or edits a native ProjectUpdate only by explicit call. The caller selects onTrack, atRisk or offTrack and supplies a visible author, health reason and body; no read publishes an update. Creation uses request_id as the native ID, so retrying an unknown outcome reads the existing update. Edits require id, project_id and the last observed updatedAt as expected_updated_at. A changed native update with a different timestamp blocks the edit instead of overwriting it. `get_context(type: project_update)` and `list_items(type: project_update)` return native health, URLs and typed activity records; lists retain native cursor pagination. A Project comment is still a separate Comment without health, and none of these operations changes Issue status.

`record_review` requires In Review and a Module, Atomic or Epic. It records a native comment with reviewer, summary, findings, artifact links and `accepted`/`changes_requested`. It never changes status. A Task is reviewed only within its Module.

The orchestrator returns work to In Progress after changes are requested. A new work round clears its current results/checks/artifacts and current review. Earlier native reports and history remain. New outputs and a new review are required. Editing reviewed content requires reopening; a Module's `merge_report` can be added after positive review without invalidating it.

PR links and merge facts remain trusted agent reports. MCP reads local commits but does not contact a remote host to verify a PR or merge. A local repository does not replace the Module's real PR, review or merge requirements. Shared bearer clients are trusted; reported roles are workflow attribution, not separate authorization principals.

## Integration Atomic

The orchestrator creates an Atomic with `work_type: integration` under an Epic or directly under Project. It names at least two Modules, interaction scenarios, environment, expected outcome and local check. Epic integration uses that Epic's Modules.

Start requires participating Modules Done with merge reports. The Atomic stores their work-round/content/completion identities. It tests combined behavior and supplies an artifact report; no new commit is needed if it only runs checks. Its report is reviewed normally.

A changed or reopened Module invalidates earlier integration. Repeat the Atomic explicitly by returning it to In Progress, running its scenarios again and submitting new results/review. A stale integration already In Progress can explicitly restart in that same status with a fresh round. Participating Modules must remain free of native discrepancies through integration review and closure. Epics with multiple delivered Modules require current successful integration coverage before final review.

## Manual changes and failures

Every guarded operation reads fresh Linear state. Status, parent/project, type-label, completion and frozen-membership inconsistencies are reported and block forward progress. Reads do not fix state. Restore structural changes explicitly in Linear; use the documented reopen/edit paths for state/content repair.

Exactly one loopback gateway serializes requests. Stdio clients share it. Native writes across an issue and its attachment are not a distributed transaction: issue edits/transitions persist a prepared update and source snapshot before the native mutation and finalize only after the returned fields match the target. A cold gateway resumes only on an identical explicit request. It finalizes an already applied target, retries an unchanged source, or rejects conflicting native edits without overwriting them.

The service does not protect against a malicious agent editing its native records, launch agents, schedule work, create integration automatically or migrate v1 pilot data.
