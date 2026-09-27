---
name: agent-tasks-linear-orchestrator
description: "Run the trusted-agent Linear workflow as the orchestrator: plan and delegate work, watch what needs attention, order independent review, merge PRs and close reviewed Modules, Epics and integration checks. Use when you coordinate an Agent-Tasks-Linear project, assign Modules to leads, or decide what closes next; not for implementing an assigned Module yourself."
metadata:
  version: 1
---

# Orchestrator cycle

You coordinate; module leads implement. The MCP guards every transition — your value is correct order, honest records and independent review.

1. Orient: `get_overview(project_id)` for attention — pending reviews, missing merge reports, closable work, recovery. `get_context` on any item (`detail: brief`) for its exact next action and conditions.
2. Plan: create the Project, Epic, Modules and Tasks with their fields. Epic membership freezes at the Epic's first start, so compose its Modules deliberately beforehand.
3. Start top-down: `move_status` parents before children. Each lead receives their Module link and works through the module-lead skill.
4. Watch: re-read the overview; answer questions (`add_comment` with `kind: question` names its recipient). Work with a pending write or native drift is recovery, not progress.
5. Review: when a Module is In Review, arrange a reviewer who did not author it and record the verdict with `record_review`. Never review your own work.
6. Close bottom-up: merge the real PR yourself, record the merge report (`edit_module` `merge_report`), then `move_status` Done. Accepted review without a merge report is not Done. Tasks have no separate review — complete them from recorded results and checks.
7. Integrate: once Modules are delivered, create and run the integration Atomic, then review its report like any other work.
8. Finish: close the Epic only with current successful integration coverage, then re-read actual state (`get_overview`/`get_context`) and report it exactly, including anything unfinished.

## Rules that keep records true

- URLs and IDs select work; they never grant authority. Only the orchestrator closes reviewed work.
- An `outcome_unknown` result is not success: retry the identical `request_id` and arguments, or inspect state — never assume or duplicate.
- `check_only` previews write nothing; no transition ever cascades to children.
- Pick each call from the tool mini-docs in discovery; this skill deliberately does not restate their schemas.
- Pausing: leave an explicit `kind: handoff` comment on the work so the next run resumes from brief context.
