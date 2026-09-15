# `nico-admin-cli attestation measured-boot site trusted-profile`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [site](./attestation-measured-boot-site.md) › **trusted-profile**_

## NAME

nico-admin-cli-attestation-measured-boot-site-trusted-profile - Managed
trusted profiles.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site trusted-profile**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

Managed trusted profiles.

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
nico-admin-cli attestation measured-boot site trusted-profile approve 12345678-1234-5678-90ab-cdef01234567 persist
nico-admin-cli attestation measured-boot site trusted-profile list
nico-admin-cli attestation measured-boot site trusted-profile remove by-profile-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`approve`](./attestation-measured-boot-site-trusted-profile-approve.md) | Allow auto-promoting of measurements from machines matching a profile. |
| [`remove`](./attestation-measured-boot-site-trusted-profile-remove.md) | Remove a trusted profile approval. |
| [`list`](./attestation-measured-boot-site-trusted-profile-list.md) | List all active profile approvals. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
