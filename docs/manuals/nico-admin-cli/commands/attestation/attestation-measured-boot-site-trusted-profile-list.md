# `nico-admin-cli attestation measured-boot site trusted-profile list`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › [trusted-profile](./attestation-measured-boot-site-trusted-profile.md) › **list**_

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-profile-list -
List all active profile approvals.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-profile list**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List all active profile approvals.

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
nico-admin-cli attestation measured-boot site trusted-profile list
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
