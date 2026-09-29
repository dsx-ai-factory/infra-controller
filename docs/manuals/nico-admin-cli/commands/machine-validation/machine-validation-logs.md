# `nico-admin-cli machine-validation logs`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › **logs***

## NAME

nico-admin-cli-machine-validation-logs - Show or follow Machine
Validation attempt logs

## SYNOPSIS

```text
nico-admin-cli machine-validation logs [--extended]
[--sort-by] [-h|--help] <subcommands>
```

## DESCRIPTION

Show or follow Machine Validation attempt logs

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
| [`show`](./machine-validation-logs-show.md) | Show stored logs for a validation attempt |
| [`follow`](./machine-validation-logs-follow.md) | Show stored logs and follow an active validation attempt |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
