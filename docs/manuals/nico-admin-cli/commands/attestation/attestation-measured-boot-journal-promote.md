# `nico-admin-cli attestation measured-boot journal promote`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [journal](./attestation-measured-boot-journal.md) › **promote***

## NAME

nico-admin-cli-attestation-measured-boot-journal-promote - Promote a
journal entry report to a bundle.

## SYNOPSIS

**nico-admin-cli attestation measured-boot journal promote**
\[**--pcr-registers**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*JOURNAL_ID*\>

## DESCRIPTION

Promote a journal entry report to a bundle.

## OPTIONS

**--pcr-registers** *\<PCR_REGISTERS\>*  
Select specific PCR range(s) to use for the promoted bundle.

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

\<*JOURNAL_ID*\>  
The journal entry ID to promote a report from.

## Examples

```sh
nico-admin-cli attestation measured-boot journal promote 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation measured-boot journal promote 12345678-1234-5678-90ab-cdef01234567 --pcr-registers 0,7,11-14
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
