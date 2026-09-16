# `nico-admin-cli attestation spdm profile get`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [profile](./attestation-spdm-profile.md) › **get**_

## NAME

nico-admin-cli-attestation-spdm-profile-get - Show the attestation
profile for one hardware class

## SYNOPSIS

**nico-admin-cli attestation spdm profile get** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*HARDWARE_CLASS*\>

## DESCRIPTION

Show the attestation profile for one hardware class

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

\<*HARDWARE_CLASS*\>  
Hardware class the profile is keyed to

## Examples

```sh
nico-admin-cli attestation spdm profile get Gb200
nico-admin-cli attestation spdm profile get any
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
