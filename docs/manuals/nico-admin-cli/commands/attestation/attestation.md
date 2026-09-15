# `nico-admin-cli attestation`

_[Hardware commands](../../hardware.md) › **attestation**_

## NAME

nico-admin-cli-attestation - MeasuredBoot or SPDM attestations

## SYNOPSIS

**nico-admin-cli attestation** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

MeasuredBoot or SPDM attestations

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`spdm`](./attestation-spdm.md) | Perform SPDM attestation |
| [`measured-boot`](./attestation-measured-boot.md) | Work with measured boot data (bundles, journals, reports, profiles, site). |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
