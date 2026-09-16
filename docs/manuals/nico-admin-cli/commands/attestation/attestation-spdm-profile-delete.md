# `nico-admin-cli attestation spdm profile delete`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [profile](./attestation-spdm-profile.md) › **delete**_

## NAME

nico-admin-cli-attestation-spdm-profile-delete - Remove the attestation
profile for a hardware class

## SYNOPSIS

**nico-admin-cli attestation spdm profile delete**
\[**--if-version-match**\] \[**--force**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*HARDWARE_CLASS*\>

## DESCRIPTION

Remove the attestation profile for a hardware class

## OPTIONS

**--if-version-match** *\<IF_VERSION_MATCH\>*  
Apply only if the stored version still matches this. Omitted applies to
whatever is stored now

**--force**  
Required to remove the any fallback, which leaves every class without a
profile of its own attesting nothing

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
nico-admin-cli attestation spdm profile delete Gb200 --if-version-match V7-T1789080000000000
nico-admin-cli attestation spdm profile delete any --force
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
