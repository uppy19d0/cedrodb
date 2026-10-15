# Compatibility and upgrades

## 0.2 to 0.3

The transaction log format is unchanged. Existing Node/Python methods remain available; 0.3 adds integrity and backup/restore APIs. There is no automatic document-schema transformation.

Close a database before opening its directory from another process or language. Preserve a tested copy before upgrades. A backup container is not a raw `cedro.log`; restore through the dedicated API into a new directory.

Recreate indexes after reopening or restoration. Complete backups retain transaction cursors, but a cursor after an older backup's final transaction is invalid in that copy. Numeric ranges, null/missing distinctions and floating-point aggregate behavior are unchanged.

## Pre-1.0 policy

Incompatible format/API changes must have release and migration notes. A stable backward/forward compatibility contract is a gate for 1.0, not a guarantee already established by the project.
