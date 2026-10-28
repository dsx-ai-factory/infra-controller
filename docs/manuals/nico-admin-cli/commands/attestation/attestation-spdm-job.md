# `nico-admin-cli attestation spdm job`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **job***

## NAME

nico-admin-cli-attestation-spdm-job - Manage SPDM attestation jobs

## SYNOPSIS

```text
nico-admin-cli attestation spdm job [--sort-by]
[-h|--help] <subcommands>
```

## DESCRIPTION

Manage SPDM attestation jobs

## OPTIONS

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
| [`cancel`](./attestation-spdm-job-cancel.md) | Cancel an attestation job |
| [`get`](./attestation-spdm-job-get.md) | Get SPDM attestation details for a given job id |
| [`list`](./attestation-spdm-job-list.md) | List SPDM attestation jobs by status |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
