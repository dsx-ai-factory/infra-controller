# `nico-admin-cli attestation measured-boot bundle delete`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **delete***

## NAME

nico-admin-cli-attestation-measured-boot-bundle-delete - Delete a bundle
based on ID

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle delete**
\[**--purge-journals**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*BUNDLE_ID*\>

## DESCRIPTION

Delete a bundle based on ID

## OPTIONS

**--purge-journals**  
Also purge any journal records for this bundle.

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

\<*BUNDLE_ID*\>  
The bundle ID.

## Examples

```sh
nico-admin-cli attestation measured-boot bundle delete 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation measured-boot bundle delete 12345678-1234-5678-90ab-cdef01234567 --purge-journals
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
