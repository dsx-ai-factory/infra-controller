# `nico-admin-cli firmware nic tenant`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › **tenant***

## NAME

nico-admin-cli-firmware-nic-tenant - Manage tenant NIC firmware profile
selections

## SYNOPSIS

```text
nico-admin-cli firmware nic tenant [OPTIONS] <COMMAND>
```

## DESCRIPTION

Read or change the operator-selected NIC firmware profile for a tenant.
The selection uses the tenants shared configuration version: ordinary
tenant updates and identity-configuration updates also invalidate an
observed token. Every successful set or clear advances that version,
including writes that leave the selection unchanged. Tenant selections
leave site, machine, and card selections unchanged.

This stores a requirement for future selected SPX attachments and does
not flash firmware. Until SPX attachment preparation is supported
([issue #6686](https://github.com/dsx-ai-factory/infra-controller/issues/6686)), a
tenant with a selection cannot allocate an instance requesting SPX
attachments. Allocations without SPX attachments are not blocked by this
check.

Allocations with saved firmware requirements keep them after tenant
selection or profile edits. Removals and partition edits are allowed if
retained attachments keep their saved device bindings. Adding
attachments is rejected when the saved requirements include a tenant
profile.

Instances without saved requirements use the live tenant selection. With
a profile selected, edits that leave a nonempty SPX attachment list are
rejected. Unchanged attachments and clearing the list are not blocked.
Both saved and live firmware-check rejections return
FAILED_PRECONDITION.

The live tenant-selection check is read-time: a request already past the
selection read may complete after a later selection commits. Finish
upgrading all Core replicas before setting tenant selections; do not
roll back to Core versions that ignore them.

Show, set, and clear return one object with tenant_org_id, profile_id,
and config_version in JSON/YAML. An unset profile is null. Table/CSV
always returns one row with Tenant Org ID, Tenant Version, and Profile
ID columns; an unset profile has an empty cell. Put --format ascii-table
(default), csv, json, or yaml and --output PATH before the firmware
subcommand. Output defaults to stdout. --output requires a path that
does not exist and creates it before the operation, even if the
operation fails. --sort-by and --extended have no effect.

## OPTIONS

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli firmware nic tenant show --tenant-org-id fds34511233a
nico-admin-cli firmware nic tenant set --tenant-org-id fds34511233a --profile connectx-production --if-version-match V1-T1789080000000000
nico-admin-cli firmware nic tenant clear --tenant-org-id fds34511233a --if-version-match V2-T1789080000000001
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./firmware-nic-tenant-show.md) | Show a tenant's NIC firmware selection and shared version |
| [`set`](./firmware-nic-tenant-set.md) | Select a NIC firmware profile at an observed tenant version |
| [`clear`](./firmware-nic-tenant-clear.md) | Clear a NIC firmware selection at an observed tenant version |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
