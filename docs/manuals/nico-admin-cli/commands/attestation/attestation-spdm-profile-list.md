# `nico-admin-cli attestation spdm profile list`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [spdm](./attestation-spdm.md) › [profile](./attestation-spdm-profile.md) › **list**_

## NAME

nico-admin-cli-attestation-spdm-profile-list - List every stored
attestation profile

## SYNOPSIS

**nico-admin-cli attestation spdm profile list** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List every stored attestation profile

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

## Examples

```sh
nico-admin-cli attestation spdm profile list
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
