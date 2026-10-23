# `nico-admin-cli attestation spdm`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › **spdm***

## NAME

nico-admin-cli-attestation-spdm - Perform SPDM attestation

## SYNOPSIS

```text
nico-admin-cli attestation spdm [--sort-by]
[-h|--help] <subcommands>
```

## DESCRIPTION

Perform SPDM attestation

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
| [`cancel`](./attestation-spdm-cancel.md) | Cancel an attestation job |
| [`coverage`](./attestation-spdm-coverage.md) | Show which hardware classes the site has and what would attest each |
| [`job`](./attestation-spdm-job.md) | Inspect SPDM attestation job |
| [`machines`](./attestation-spdm-machines.md) | Inspect predicted SPDM attestation status per machine |
| [`profile`](./attestation-spdm-profile.md) | Manage the attestation policy stored for each hardware class |
| [`trigger`](./attestation-spdm-trigger.md) | Trigger attestation for a given machine with id |

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
