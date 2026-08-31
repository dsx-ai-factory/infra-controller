# `nico-admin-cli attestation measured-boot profile list`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [profile](./attestation-measured-boot-profile.md) › **list***

## NAME

nico-admin-cli-attestation-measured-boot-profile-list - List profiles by
various ways.

## SYNOPSIS

**nico-admin-cli attestation measured-boot profile list**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

List profiles by various ways.

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
nico-admin-cli attestation measured-boot profile list all
nico-admin-cli attestation measured-boot profile list bundles my-profile
nico-admin-cli attestation measured-boot profile list machines my-profile
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`all`](./attestation-measured-boot-profile-list-all.md) | List all profiles |
| [`bundles`](./attestation-measured-boot-profile-list-bundles.md) | List all bundles for a given profile ID or name. |
| [`machines`](./attestation-measured-boot-profile-list-machines.md) | List all machines for a given profile ID or name. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
