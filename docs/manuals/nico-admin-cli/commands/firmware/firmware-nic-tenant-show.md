# `nico-admin-cli firmware nic tenant show`

*[Hardware commands](../../hardware.md) › [firmware](./firmware.md) › [nic](./firmware-nic.md) › [tenant](./firmware-nic-tenant.md) › **show***

## NAME

nico-admin-cli-firmware-nic-tenant-show - Show a tenants NIC firmware
selection and shared version

## SYNOPSIS

```text
nico-admin-cli firmware nic tenant show [OPTIONS] --tenant-org-id <ID>
```

## DESCRIPTION

Read one tenants saved NIC firmware profile selection and complete
shared tenant version. An existing tenant without a selection still
returns its configuration. Tenant selections leave site, machine, and
card selections unchanged. A missing tenant returns NOT_FOUND. This
command does not inspect hardware, fetch artifacts, or flash firmware.

Use the returned version with tenant set or clear. Ordinary tenant
updates and identity-configuration updates also change this token; it is
independent of the profiles edit version.

Put --format ascii-table (default), csv, json, or yaml and --output PATH
before the firmware subcommand. JSON/YAML returns one object with
tenant_org_id, profile_id, and config_version; an unset profile is null.
Table/CSV returns Tenant Org ID, Tenant Version, and Profile ID, with an
empty profile cell when unset. Output defaults to stdout. --output
requires a path that does not exist and creates it before the read, even
if the read fails. --sort-by and --extended have no effect.

## OPTIONS

`--tenant-org-id <ID>`

Case-sensitive tenant organization ID; nonempty ASCII letters, digits,
underscores, or hyphens

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
nico-admin-cli --format json firmware nic tenant show --tenant-org-id fds34511233a
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
