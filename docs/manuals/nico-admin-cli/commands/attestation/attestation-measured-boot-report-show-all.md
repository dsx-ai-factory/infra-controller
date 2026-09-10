# `nico-admin-cli attestation measured-boot report show all`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › [show](./attestation-measured-boot-report-show.md) › **all**_

## NAME

nico-admin-cli-attestation-measured-boot-report-show-all - Show all
reports.

## SYNOPSIS

**nico-admin-cli attestation measured-boot report show all**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Show all reports.

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

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
