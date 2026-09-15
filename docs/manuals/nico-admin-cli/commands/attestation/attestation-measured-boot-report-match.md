# `nico-admin-cli attestation measured-boot report match`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › **match**_

## NAME

nico-admin-cli-attestation-measured-boot-report-match - Match reports
with the provided PCR register values.

## SYNOPSIS

**nico-admin-cli attestation measured-boot report match**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \<*VALUES*\>

## DESCRIPTION

Match reports with the provided PCR register values.

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

\<*VALUES*\>  
Comma-separated list of {pcr_register:value,...} to match on.

## Examples

```sh
nico-admin-cli attestation measured-boot report match 0:abc123,7:def456
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
