# `nico-admin-cli attestation measured-boot report promote`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › **promote**_

## NAME

nico-admin-cli-attestation-measured-boot-report-promote - Promote a
specific journal entry to an active bundle

## SYNOPSIS

**nico-admin-cli attestation measured-boot report promote**
\[**--pcr-registers**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*REPORT_ID*\>

## DESCRIPTION

Promote a specific journal entry to an active bundle

## OPTIONS

**--pcr-registers** *\<PCR_REGISTERS\>*  
Select a specific PCR range to use for the promoted bundle.

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
The report ID to promote.

## Examples

```sh
nico-admin-cli attestation measured-boot report promote 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation measured-boot report promote 12345678-1234-5678-90ab-cdef01234567 --pcr-registers 0,7,11-14
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
