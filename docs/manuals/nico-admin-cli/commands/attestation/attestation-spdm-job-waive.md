# `nico-admin-cli attestation spdm job waive`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [job](./attestation-spdm-job.md) › **waive***

## NAME

nico-admin-cli-attestation-spdm-job-waive - Request a waiver for an
attestation job

## SYNOPSIS

```text
nico-admin-cli attestation spdm job waive [--sort-by]
[-h|--help] <JOB_ID>
```

## DESCRIPTION

Waive an attestation job, including completed failures. Unfinished
devices finish BMC evidence collection cleanup before the waiver takes
effect. Terminal results immediately count as Waived without rerunning
attestation. Later waive or fail requests replace the previous target
and refresh the request time. Changing the request does not restart
ongoing cleanup. Stored attestation results and completion times stay
available as history.

## OPTIONS

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with '-h')

`<JOB_ID>`

Attestation job ID

## Examples

```sh
nico-admin-cli attestation spdm job waive 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
