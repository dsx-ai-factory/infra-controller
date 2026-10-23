# `nico-admin-cli firmware nic tenant clear`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [tenant](./firmware-nic-tenant.md) › **clear***

## NAME

nico-admin-cli-firmware-nic-tenant-clear - Clear a NIC firmware
selection at an observed tenant version

## SYNOPSIS

```text
nico-admin-cli firmware nic tenant clear [OPTIONS] --tenant-org-id <ID> --if-version-match <VERSION>
```

## DESCRIPTION

Clear a tenants NIC firmware profile selection using its complete
observed tenant version from firmware nic tenant show. The token is
shared with ordinary tenant updates and identity-configuration updates,
not the profiles edit version. Every successful clear advances the
tenant version, even if no profile is selected. A missing tenant returns
NOT_FOUND; a stale token returns FAILED_PRECONDITION.

This clears the live tenant selection, not saved allocation
requirements. Allocations that saved a tenant profile still reject added
SPX attachments with FAILED_PRECONDITION. It leaves site, machine, and
card selections unchanged and does not flash firmware, edit profile
definitions, or change existing instances.

The CLI neither fetches a fresh token nor retries a failed write. After
a connection error, inspect firmware nic tenant show before another
write; the clear may have committed. Keep the original token when
repeating the same request.

Put --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON/YAML returns one object with
tenant_org_id, a null profile_id, and the new config_version. Table/CSV
returns Tenant Org ID, Tenant Version, and Profile ID, with an empty
profile cell. Output defaults to stdout. --output requires a path that
does not exist and creates it before the clear, even if the clear fails.
--sort-by and --extended have no effect.

## OPTIONS

`--tenant-org-id <ID>`

Case-sensitive tenant organization ID; nonempty ASCII letters, digits,
underscores, or hyphens

`--if-version-match <VERSION>`

Complete observed tenant version, such as V2-T1789080000000001; keep
unchanged when repeating this request

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
nico-admin-cli firmware nic tenant clear --tenant-org-id fds34511233a --if-version-match V2-T1789080000000001
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
