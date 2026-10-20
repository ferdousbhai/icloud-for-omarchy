# Shared support copies

These products vendor a small set of identical support files. Their installed
apps and CLIs stay independent: there is no new runtime package or cross-repo
import. Product services, application UI, provider logic, commands and embedded
skill text are outside this shared boundary.

| Canonical implementation | Copies |
| --- | --- |
| `cloud-contacts/cloud_contacts/{common,cli_output,theme,recall_client,agent_skill}.py` | Same filenames under `cloud-tasks/cloud_tasks/` |
| `icloud-for-omarchy/sessiond/src/agent_skill.rs` | `cloud-mail/crates/cloudmail/src/`, `cloud-calendar/crates/cloud-calendar/src/`, `omacloud/crates/omacloud/src/` |
| `icloud-for-omarchy/photos/src/recall_client.rs` | `cloud-mail/crates/cloudmail-gtk/src/`, `cloud-calendar/crates/cloud-calendar-gtk/src/` |

`bin/check-shared-support` is itself identical in all six repositories; its
canonical copy is in cloud-contacts. `bin/check` runs it before product checks.
It reads files only, compares available sibling copies byte for byte, and checks
canonical SHA-256 digests in each product’s scoped `shared-support.json`.
Thus a standalone checkout still detects unrecorded local edits. Normal checks
cover only the current product's shared groups. Missing sibling checkouts are
reported without failing; a missing shared file within an available checkout
is an error.

The local manifest identifies its product, so this checkout can have any
name (including a release archive or worktree name). Available peer checkouts
are discovered under the repository names above in the same parent directory.
Use `python3 bin/check-shared-support --workspace-root /path/to/siblings` to
select a peer directory explicitly; the invoking product still resolves to
its actual checkout path. Use `--require-all` for a full workspace or release
audit. The checker does not fetch repositories or contact accounts.

To change a shared implementation:

1. Edit its canonical file and run the owning product's relevant tests.
2. Review and copy only that file to its listed destinations; run their relevant tests.
   Do not copy app, service, provider, or product skill files.
3. Run `python3 bin/check-shared-support --record common.py` (substitute the
   changed group name, or list several). Only checkouts containing those groups
   must be present: a Python change needs Contacts and Tasks, not the Rust apps.
   This refuses differing source copies and checker code, and updates only the
   participating `shared-support.json` records. It never overwrites product source.
4. Run the affected products' `bin/check` (use `--require-all` on the shared
   checker for a full workspace audit), review the diffs, and include each
   updated manifest with its product's source change.

Changing the checker itself follows the same explicit copy/review process;
copy the canonical script to all six destinations before normal checks.
Checker code drift is reported as `Checker sync mismatch`; support file drift
is reported separately as a product source copy/digest mismatch.
`cloud-contacts/tests/test_shared_support.py` tests drift detection, standalone
checks, missing files and digest recording using disposable fixture checkouts.
