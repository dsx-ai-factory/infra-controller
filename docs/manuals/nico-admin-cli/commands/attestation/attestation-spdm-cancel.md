# `nico-admin-cli attestation spdm cancel`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › **cancel***

## NAME

nico-admin-cli-attestation-spdm-cancel - Cancel attestation for a given
machine id

## SYNOPSIS

**nico-admin-cli attestation spdm cancel** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE_ID*\>

## DESCRIPTION

Cancel attestation for a given machine id

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

\<*MACHINE_ID*\>  
Machine ID

## Examples

```sh
nico-admin-cli attestation spdm cancel 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
