# `nico-admin-cli attestation measured-boot bundle find-closest-match report`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › [find-closest-match](./attestation-measured-boot-bundle-find-closest-match.md) › **report***

## NAME

nico-admin-cli-attestation-measured-boot-bundle-find-closest-match-report -
The existing report ID.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle find-closest-match
report** \[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*ID*\>

## DESCRIPTION

The existing report ID.

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

\<*ID*\>  
Report ID.

## Examples

```sh
nico-admin-cli attestation measured-boot bundle find-closest-match report 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
