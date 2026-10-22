# `nico-admin-cli attestation spdm job list`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [job](./attestation-spdm-job.md) › **list***

## NAME

nico-admin-cli-attestation-spdm-job-list - List SPDM attestation jobs by
status

## SYNOPSIS

```text
nico-admin-cli attestation spdm job list <--selector>
[--sort-by] [-h|--help]
```

## DESCRIPTION

List SPDM attestation jobs by status

## OPTIONS

`--selector <SELECTOR>`

Filter attestation jobs by status

*Possible values:*

> - in-progress
>
> - cancelled
>
> - failed
>
> - passed

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
nico-admin-cli attestation spdm job list --selector in-progress
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
