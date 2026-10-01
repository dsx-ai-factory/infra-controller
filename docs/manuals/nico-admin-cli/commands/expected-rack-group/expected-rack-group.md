# `nico-admin-cli expected-rack-group`

*[Tenant commands](../../tenant.md) › **expected-rack-group***

## NAME

nico-admin-cli-expected-rack-group - Expected rack group handling

## SYNOPSIS

```text
nico-admin-cli expected-rack-group [--extended]
[--sort-by] [-h|--help] <subcommands>
```

## DESCRIPTION

Expected rack group handling

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`show`](./expected-rack-group-show.md) | Show one or all expected rack groups |
| [`add`](./expected-rack-group-add.md) | Add an expected rack group |
| [`delete`](./expected-rack-group-delete.md) | Delete an expected rack group |
| [`update`](./expected-rack-group-update.md) | Replace all fields of an existing expected rack group |
| [`replace-all`](./expected-rack-group-replace-all.md) | Replace all expected rack groups from a JSON file |
| [`erase`](./expected-rack-group-erase.md) | Erase all expected rack groups |

---

**Related:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
