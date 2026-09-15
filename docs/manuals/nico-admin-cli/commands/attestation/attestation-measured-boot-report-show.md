# `nico-admin-cli attestation measured-boot report show`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › **show**_

## NAME

nico-admin-cli-attestation-measured-boot-report-show - Show reports in
different ways.

## SYNOPSIS

**nico-admin-cli attestation measured-boot report show**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Show reports in different ways.

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
nico-admin-cli attestation measured-boot report show all
nico-admin-cli attestation measured-boot report show id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation measured-boot report show machine 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`id`](./attestation-measured-boot-report-show-id.md) | Show a report ID. |
| [`machine`](./attestation-measured-boot-report-show-machine.md) | Show reports for a machine. |
| [`all`](./attestation-measured-boot-report-show-all.md) | Show all reports. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
