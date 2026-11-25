# `nico-admin-cli nvlink-domain`

*[Tenant commands](../../tenant.md) › **nvlink-domain***

## NAME

nico-admin-cli-nvlink-domain - Administrator-owned NVLink domain
declarations

## SYNOPSIS

```text
nico-admin-cli nvlink-domain [-h|--help]
<subcommands>
```

## DESCRIPTION

Administrator-owned NVLink domain declarations

A domain is an administrator-managed group of compute and switch trays
within a RackGroup. New declarations enter Created. On reconciliation,
the domain controller moves them to Discovering and looks up their
declared trays in inventory.

## OPTIONS

`-h, --help`

Print help (see a summary with '-h')

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./nvlink-domain-create.md) | Create immutable domain membership without configuring hardware |
| [`show`](./nvlink-domain-show.md) | Show one declaration or list declarations in UUID order |
| [`delete`](./nvlink-domain-delete.md) | Request deletion of a domain; its positions remain reserved until cleanup |

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
