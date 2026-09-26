# Recovering an interrupted request

Keep the `request_id` and the complete original arguments until the outcome is known. `outcome_unknown` is never success.

1. Keep one gateway writer. Restart it on the same fixed loopback port if it stopped.
2. For an existing issue, use `get_context` with `type: issue`. A prepared update includes its original tool request and intended native update.
3. Retry that same tool with the same `request_id` and arguments. A different request cannot replace a pending operation.
   An already applied target is finalized without reapplying the mutation. If native fields differ from both the saved source and intended target, MCP reports a conflict and preserves the manual content. Resolve that conflict explicitly in Linear before retrying; reads never restore old content.
   Linear's `-` to `*` list serialization is presentation-equivalent, while changed text or link destinations still conflict. An older pending Duplicate transition containing only `stateId` is replayed through the native duplicate relation operation using its saved `duplicate_of` field. Preserve all original arguments and metadata; recovery does not need a replacement request or a direct metadata edit.
4. For interrupted creates, retry the original create call. The same UUID addresses the same Project/Issue/Document. Default project documents have repeatable subordinate IDs. Existing objects are checked before reuse.
5. Review creation uses its request UUID as the native comment UUID. A retry reuses the matching report instead of posting another comment.

Edits to Projects/Documents are native partial updates. Repeat the same field assignment after checking context if their result was uncertain. Do not replay an old edit after newer intentional edits; use a fresh request for a new intention.

Manual state violations are not automatically rolled back. Restore changed parent/project/type label in Linear. An explicit return to In Progress can recover an externally changed workflow status and starts a fresh result/review round. A description-only change is adopted through an explicit edit, preserving unrelated sections.

If the object or its state attachment was deleted, do not silently recreate a different work item. Inspect the preserved native history and the original request. API authentication, rate limits, malformed/partial responses and missing objects remain distinct failures.

Only the transport configuration contains a secret bearer credential. Workflow state is in Linear; no signing key or local workflow database needs recovery.
