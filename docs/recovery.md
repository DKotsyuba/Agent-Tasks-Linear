# Recovering an interrupted write

Retain each mutation's idempotency_key until its outcome is known.

1. Stop the previous gateway before starting another writer.
2. Call at_resume to find pending operation keys.
3. Call at_reconcile with mode=inspect and the operation_key. Inspection makes no writes and needs no new key.
4. Continue with mode=resume_pending, the original operation_key, and a new idempotency_key for the reconciliation request.
5. Retry the original intent using its original key only after reconciliation confirms the outcome.

Receipts retain object IDs and completed steps. A verified existing create is reused; an absent ambiguous create remains unknown. A metadata upsert may repeat the same saved ID and content under the one-writer assumption. No new object ID is invented to work around a timeout.

If a required object was deleted, a stored record is corrupt or a create remains ambiguous, inspect the object and pending recipe before further writes. Do not clear pending metadata manually or claim success. The original request and history remain available.

restore_projection is an explicit owner/root action: with work_id and idempotency_key it restores the recorded issue status when work membership is unchanged and no other operation is pending. It does not create a result or recover deleted artifacts.

Back up the deployment configuration and signing key as secrets. There is no local business-data cache to recover. A lost signing key must not be silently replaced with a new key.
