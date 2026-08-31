# `nico-admin-cli attestation measured-boot site trusted-profile remove`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › [trusted-profile](./attestation-measured-boot-site-trusted-profile.md) › **remove***

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-profile-remove -
Remove a trusted profile approval.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-profile remove**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Remove a trusted profile approval.

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
nico-admin-cli attestation measured-boot site trusted-profile remove by-approval-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli attestation measured-boot site trusted-profile remove by-profile-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`by-approval-id`](./attestation-measured-boot-site-trusted-profile-remove-by-approval-id.md) | Remove by approval ID. |
| [`by-profile-id`](./attestation-measured-boot-site-trusted-profile-remove-by-profile-id.md) | Remove by profile ID. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
