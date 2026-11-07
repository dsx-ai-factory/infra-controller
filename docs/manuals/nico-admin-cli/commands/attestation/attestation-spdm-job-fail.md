# `nico-admin-cli attestation spdm job fail`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [job](./attestation-spdm-job.md) › **fail***

## NAME

nico-admin-cli-attestation-spdm-job-fail - Request failure for an
attestation job

## SYNOPSIS

```text
nico-admin-cli attestation spdm job fail [--sort-by]
[-h|--help] <JOB_ID>
```

## DESCRIPTION

Fail an attestation job, including completed results. Unfinished devices
finish BMC evidence collection cleanup before failure takes effect.
Terminal results immediately count as Failed. Later waive or fail
requests replace the previous target and refresh the request time.
Changing the request does not restart ongoing cleanup. Stored
attestation results and completion times stay available as history. This
request does not undo a machine lifecycle transition that has already
completed.

## OPTIONS

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

`<JOB_ID>`

Attestation job ID

## Examples

```sh
nico-admin-cli attestation spdm job fail 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
