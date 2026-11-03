# `nico-admin-cli attestation spdm job stop`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [job](./attestation-spdm-job.md) › **stop***

## NAME

nico-admin-cli-attestation-spdm-job-stop - Stop an attestation job,
failing unfinished device attestations after BMC cleanup

## SYNOPSIS

```text
nico-admin-cli attestation spdm job stop [--sort-by]
[-h|--help] <JOB_ID>
```

## DESCRIPTION

Stop an attestation job, failing unfinished device attestations after
BMC cleanup

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
nico-admin-cli attestation spdm job stop 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
