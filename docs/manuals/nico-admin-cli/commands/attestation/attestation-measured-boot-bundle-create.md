# `nico-admin-cli attestation measured-boot bundle create`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **create**_

## NAME

nico-admin-cli-attestation-measured-boot-bundle-create - Create a new
bundle with a given values, for a given profile ID.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle create**
\[**--state**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*NAME*\> \<*PROFILE_ID*\> \<*VALUES*\>

## DESCRIPTION

Create a new bundle with a given values, for a given profile ID.

## OPTIONS

**--state** *\<STATE\>*  
The state for this bundle (default: active).\

\
*Possible values:*

- pending

- active

- obsolete

- retired

- revoked

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

\<*NAME*\>  
A human-readable name to give this bundle.

\<*PROFILE_ID*\>  
The profile ID of the profile to associate this bundle with.

\<*VALUES*\>  
Comma-separated list of {pcr_register:value,...} to associate with this
bundle.

## Examples

```sh
nico-admin-cli attestation measured-boot bundle create my-bundle 12345678-1234-5678-90ab-cdef01234567 0:abc123,7:def456
nico-admin-cli attestation measured-boot bundle create my-bundle 12345678-1234-5678-90ab-cdef01234567 0:abc123 --state pending
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
