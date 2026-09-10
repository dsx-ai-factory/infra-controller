# `nico-admin-cli attestation measured-boot site import`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › **import**_

## NAME

nico-admin-cli-attestation-measured-boot-site-import - Import a site
from an export file.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site import**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\] \<*PATH*\>

## DESCRIPTION

Import a site from an export file.

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

\<*PATH*\>  
The path of the input JSON file.

## Examples

```sh
nico-admin-cli attestation measured-boot site import ./site.json
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
