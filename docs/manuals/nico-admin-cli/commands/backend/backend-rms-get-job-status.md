# `nico-admin-cli backend rms get-job-status`

*[Hardware commands](../../hardware.md) › [backend](./backend.md) › [rms](./backend-rms.md) › **get-job-status***

## NAME

nico-admin-cli-backend-rms-get-job-status - Inspect RMS job records
through NICo

## SYNOPSIS

```text
nico-admin-cli backend rms get-job-status <--job-id>
[--include-child-job-states] [--extended] [--sort-by]
[-h|--help]
```

## DESCRIPTION

Inspect an RMS job through the RMS backend configured in NICo. This
diagnostic command requires the admin CLI role. It prints the requested
job and optionally its child records, including state, errors, result
JSON, device identity, timestamps, and parent/child references. A
successful query exits successfully even when the reported job failed.
Job expiry or RMS restart can make records unavailable. Component reset
status is normally queried by device using component-manager
get-reset-status.

## OPTIONS

`--job-id <JOB_ID>`

RMS job identifier to inspect

`--include-child-job-states`

Include child job records; omitted returns only the requested job

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

Print help (see a summary with '-h')

## Examples

```sh
nico-admin-cli backend rms get-job-status --job-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli -f json backend rms get-job-status --job-id 12345678-1234-5678-90ab-cdef01234567 --include-child-job-states
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
