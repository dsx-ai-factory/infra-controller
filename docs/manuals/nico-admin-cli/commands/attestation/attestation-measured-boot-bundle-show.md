# `nico-admin-cli attestation measured-boot bundle show`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **show***

## NAME

nico-admin-cli-attestation-measured-boot-bundle-show - Show a bundle (or
all).

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle show** \[**--is-id**\]
\[**--is-name**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \[*IDENTIFIER*\]

## DESCRIPTION

Show a bundle (or all).

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

\[*IDENTIFIER*\]  
The optional bundle ID or name.

## Examples

```sh
nico-admin-cli attestation measured-boot bundle show
nico-admin-cli attestation measured-boot bundle show 12345678-1234-5678-90ab-cdef01234567 --is-id
nico-admin-cli attestation measured-boot bundle show my-bundle --is-name
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
