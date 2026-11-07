# `nico-admin-cli machine-validation logs show`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [logs](./machine-validation-logs.md) › **show***

## NAME

nico-admin-cli-machine-validation-logs-show - Show stored logs for a
validation attempt

## SYNOPSIS

```text
nico-admin-cli machine-validation logs show [--extended] [--sort-by <SORT_BY>] [--stdout-only | --stderr-only] [--raw] [-h|--help] (--attempt-id <ATTEMPT_ID> | --validation-id <VALIDATION_ID> --test-id <TEST_ID>)
```

## DESCRIPTION

Show stored logs for a validation attempt. Provide --attempt-id, or
provide both --validation-id and --test-id.

## OPTIONS

`--attempt-id <ATTEMPT_ID>`

Attempt UUID, including completed or retried attempts

`--validation-id <VALIDATION_ID>`

Run ID containing the test

`--test-id <TEST_ID>`

Test ID within the run

`--stdout-only`

Print stdout chunks only

`--stderr-only`

Print stderr chunks only

`--raw`

Print chunk content without timestamp, stream, or sequence

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
nico-admin-cli machine-validation logs show --attempt-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine-validation logs show --validation-id 12345678-1234-5678-90ab-cdef01234567 --test-id basic-machine-validation
nico-admin-cli machine-validation logs show --attempt-id 12345678-1234-5678-90ab-cdef01234567 --stderr-only --raw
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
