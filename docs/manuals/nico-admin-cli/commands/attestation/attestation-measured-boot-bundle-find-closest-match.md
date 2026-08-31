# `nico-admin-cli attestation measured-boot bundle find-closest-match`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **find-closest-match***

## NAME

nico-admin-cli-attestation-measured-boot-bundle-find-closest-match - Get
closest bundle to a report.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle find-closest-match**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Get closest bundle to a report.

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
nico-admin-cli attestation measured-boot bundle find-closest-match report 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`report`](./attestation-measured-boot-bundle-find-closest-match-report.md) | The existing report ID. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
