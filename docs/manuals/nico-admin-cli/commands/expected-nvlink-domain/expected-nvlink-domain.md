# `nico-admin-cli expected-nvlink-domain`

*[Tenant commands](../../tenant.md) › **expected-nvlink-domain***

## NAME

nico-admin-cli-expected-nvlink-domain - Administrator-owned expected
NVLink domain declarations

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain [-h|--help]
<subcommands>
```

## DESCRIPTION

Administrator-owned expected NVLink domain declarations

A domain is an administrator-managed group of compute and switch trays
within a RackGroup. No NVLink management state machine consumes these
declarations yet. NVLink management is planned to move from rack-based
to domain-based state machines, which will use these declarations to
define domains.

## OPTIONS

`-h, --help`

Print help (see a summary with -h)

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./expected-nvlink-domain-create.md) | Create immutable expected membership without configuring hardware |
| [`show`](./expected-nvlink-domain-show.md) | Show one declaration or list declarations in UUID order |
| [`delete`](./expected-nvlink-domain-delete.md) | Delete a declaration and release its members without changing hardware |

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
