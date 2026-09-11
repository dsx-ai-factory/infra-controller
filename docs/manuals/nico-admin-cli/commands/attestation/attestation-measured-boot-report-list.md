# `nico-admin-cli attestation measured-boot report list`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › **list**_

## NAME

nico-admin-cli-attestation-measured-boot-report-list - List reports by
various ways.

## SYNOPSIS

**nico-admin-cli attestation measured-boot report list**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

List reports by various ways.

## OPTIONS

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field\

\
*Possible values:*

- primary-id: Sort by the primary ID

- state: Sort by state

**-h**, **--help**  
Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli attestation measured-boot report list all
nico-admin-cli attestation measured-boot report list machines 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`all`](./attestation-measured-boot-report-list-all.md) | List all reports |
| [`machines`](./attestation-measured-boot-report-list-machines.md) | List all reports for a given machine ID. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
