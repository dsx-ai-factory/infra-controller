# `nico-admin-cli component-manager get-switch-sdn-factory-reset-status`

*[Hardware commands](../../hardware.md) › [component-manager](./component-manager.md) › **get-switch-sdn-factory-reset-status***

## NAME

nico-admin-cli-component-manager-get-switch-sdn-factory-reset-status -
Get the parent and child job result for a switch SDN factory reset

## SYNOPSIS

```text
nico-admin-cli component-manager get-switch-sdn-factory-reset-status
<--job-id> [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Observe a switch SDN resets parent and child jobs. Pending means a
parent or child is not complete. Completed requires all submitted
children and does not override submission errors returned when
requesting the reset. Failed prints diagnostics and exits
unsuccessfully. RMS restart can lose job state; an unavailable
observation does not establish whether the reset ran.

## OPTIONS

`--job-id <JOB_ID>`

Opaque parent job ID returned by the SDN reset submission

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
nico-admin-cli component-manager get-switch-sdn-factory-reset-status --job-id JOB_ID
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
