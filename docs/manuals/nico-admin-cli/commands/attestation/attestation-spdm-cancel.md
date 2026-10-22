# `nico-admin-cli attestation spdm cancel`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **cancel***

## NAME

nico-admin-cli-attestation-spdm-cancel - Cancel an attestation job

## SYNOPSIS

```text
nico-admin-cli attestation spdm cancel [--sort-by]
[-h|--help] <JOB_ID>
```

## DESCRIPTION

Cancel an attestation job

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
nico-admin-cli attestation spdm cancel 12345678-1234-5678-90ab-cdef01234567
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
