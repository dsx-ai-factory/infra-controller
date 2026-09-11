# `nico-admin-cli attestation measured-boot report list all`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [report](./attestation-measured-boot-report.md) › [list](./attestation-measured-boot-report-list.md) › **all***

## NAME

nico-admin-cli-attestation-measured-boot-report-list-all - List all
reports

## SYNOPSIS

**nico-admin-cli attestation measured-boot report list all**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all reports

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
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
