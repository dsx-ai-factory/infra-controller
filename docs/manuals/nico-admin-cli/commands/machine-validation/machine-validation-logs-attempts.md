# `nico-admin-cli machine-validation logs attempts`

*[Hardware commands](../../hardware.md) › [machine-validation](./machine-validation.md) › [logs](./machine-validation-logs.md) › **attempts***

## NAME

nico-admin-cli-machine-validation-logs-attempts - List attempt IDs for
one test in a validation run

## SYNOPSIS

```text
nico-admin-cli machine-validation logs attempts
<--validation-id> <--test-id> [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

List attempt IDs for one test in a validation run

## OPTIONS

`--validation-id <VALIDATION_ID>`

Run ID containing the test

`--test-id <TEST_ID>`

Test ID within the run

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
nico-admin-cli machine-validation logs attempts --validation-id 12345678-1234-5678-90ab-cdef01234567 --test-id basic-machine-validation
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
