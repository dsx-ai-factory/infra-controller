# `nico-admin-cli attestation measured-boot profile`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › **profile**_

## NAME

nico-admin-cli-attestation-measured-boot-profile - Work with machine
hardware profiles

## SYNOPSIS

**nico-admin-cli attestation measured-boot profile** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with machine hardware profiles

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`create`](./attestation-measured-boot-profile-create.md) | Create a new profile with a given config. |
| [`delete`](./attestation-measured-boot-profile-delete.md) | Delete a profile by ID or name. |
| [`rename`](./attestation-measured-boot-profile-rename.md) | Rename a profile. |
| [`show`](./attestation-measured-boot-profile-show.md) | Show profiles in different ways. |
| [`list`](./attestation-measured-boot-profile-list.md) | List profiles by various ways. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
