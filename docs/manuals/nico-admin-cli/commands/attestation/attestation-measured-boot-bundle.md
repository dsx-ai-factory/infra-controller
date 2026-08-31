# `nico-admin-cli attestation measured-boot bundle`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › **bundle***

## NAME

nico-admin-cli-attestation-measured-boot-bundle - Work with golden
measurement bundles.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with golden measurement bundles.

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
| [`create`](./attestation-measured-boot-bundle-create.md) | Create a new bundle with a given values, for a given profile ID. |
| [`delete`](./attestation-measured-boot-bundle-delete.md) | Delete a bundle based on ID |
| [`rename`](./attestation-measured-boot-bundle-rename.md) | Rename a bundle. |
| [`set-state`](./attestation-measured-boot-bundle-set-state.md) | Set a new state for a bundle. |
| [`show`](./attestation-measured-boot-bundle-show.md) | Show a bundle (or all). |
| [`find-closest-match`](./attestation-measured-boot-bundle-find-closest-match.md) | Get closest bundle to a report. |
| [`list`](./attestation-measured-boot-bundle-list.md) | List bundles by various ways. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
