# `nico-admin-cli attestation measured-boot bundle set-state`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **set-state**_

## NAME

nico-admin-cli-attestation-measured-boot-bundle-set-state - Set a new
state for a bundle.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle set-state**
\[**--is-id**\] \[**--is-name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*IDENTIFIER*\> \<*STATE*\>

## DESCRIPTION

Set a new state for a bundle.

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
The bundle ID or name to update.

\<*STATE*\>  
The state to set for this bundle.\

\
*Possible values:*

- pending

- active

- obsolete

- retired

- revoked

## Examples

```sh
nico-admin-cli attestation measured-boot bundle set-state my-bundle obsolete
nico-admin-cli attestation measured-boot bundle set-state 12345678-1234-5678-90ab-cdef01234567 revoked --is-id
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
