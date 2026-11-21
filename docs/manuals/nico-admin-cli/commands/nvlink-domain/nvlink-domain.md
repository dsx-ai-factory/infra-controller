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
within a RackGroup. No NVLink management state machine consumes these
declarations yet. NVLink management is planned to move from rack-based
to domain-based state machines, which will use these declarations to
define domains.

## OPTIONS

`-h, --help`

Print help (see a summary with '-h')

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./nvlink-domain-create.md) | Create immutable domain membership without configuring hardware |
| [`show`](./nvlink-domain-show.md) | Show one declaration or list declarations in UUID order |
| [`delete`](./nvlink-domain-delete.md) | Delete a declaration and release its members without changing hardware |

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
