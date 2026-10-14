# `nico-admin-cli expected-nvlink-domain`

*[Tenant commands](../../tenant.md) › **expected-nvlink-domain***

## NAME

nico-admin-cli-expected-nvlink-domain - Administrator-owned expected
NVLink domain declarations

## SYNOPSIS

```text
nico-admin-cli expected-nvlink-domain [--extended]
[--sort-by] [-h|--help] <subcommands>
```

## DESCRIPTION

Administrator-owned expected NVLink domain declarations

## OPTIONS

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

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
