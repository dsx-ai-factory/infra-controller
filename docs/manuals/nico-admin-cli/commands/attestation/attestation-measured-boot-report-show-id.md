# `nico-admin-cli attestation measured-boot report show id`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › [show](./attestation-measured-boot-report-show.md) › **id**_

## NAME

nico-admin-cli-attestation-measured-boot-report-show-id - Show a report
ID.

## SYNOPSIS

**nico-admin-cli attestation measured-boot report show id**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*REPORT_ID*\>

## DESCRIPTION

Show a report ID.

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

\<*REPORT_ID*\>  
The report ID.

## Examples

```sh
nico-admin-cli attestation measured-boot report show id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
