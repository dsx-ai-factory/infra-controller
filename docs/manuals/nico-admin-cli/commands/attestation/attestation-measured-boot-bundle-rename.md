# `nico-admin-cli attestation measured-boot bundle rename`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **rename***

## NAME

nico-admin-cli-attestation-measured-boot-bundle-rename - Rename a
bundle.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle rename**
\[**--is-id**\] \[**--is-name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*IDENTIFIER*\> \<*NEW_BUNDLE_NAME*\>

## DESCRIPTION

Rename a bundle.

## OPTIONS

**--is-id**  
Explicitly say the identifier is bundle ID.

**--is-name**  
Explicitly say the identifier is a bundle name.

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

\<*IDENTIFIER*\>  
The existing bundle ID or name.

\<*NEW_BUNDLE_NAME*\>  
The new bundle name.

## Examples

```sh
nico-admin-cli attestation measured-boot bundle rename old-name new-name
nico-admin-cli attestation measured-boot bundle rename 12345678-1234-5678-90ab-cdef01234567 new-name --is-id
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
