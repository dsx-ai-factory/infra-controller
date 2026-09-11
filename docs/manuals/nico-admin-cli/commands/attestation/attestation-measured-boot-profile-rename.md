# `nico-admin-cli attestation measured-boot profile rename`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [profile](./attestation-measured-boot-profile.md) › **rename**_

## NAME

nico-admin-cli-attestation-measured-boot-profile-rename - Rename a
profile.

## SYNOPSIS

**nico-admin-cli attestation measured-boot profile rename**
\[**--is-id**\] \[**--is-name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*IDENTIFIER*\> \<*NEW_PROFILE_NAME*\>

## DESCRIPTION

Rename a profile.

## OPTIONS

**--is-id**  
Explicitly say the identifier is profile ID.

**--is-name**  
Explicitly say the identifier is a profile name.

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
The existing profile ID or name.

\<*NEW_PROFILE_NAME*\>  
The new profile name.

## Examples

```sh
nico-admin-cli attestation measured-boot profile rename old-name new-name
nico-admin-cli attestation measured-boot profile rename 12345678-1234-5678-90ab-cdef01234567 new-name --is-id
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
