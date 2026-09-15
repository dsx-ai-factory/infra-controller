# `nico-admin-cli attestation measured-boot bundle list`

_[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › [bundle](./attestation-measured-boot-bundle.md) › **list**_

## NAME

nico-admin-cli-attestation-measured-boot-bundle-list - List bundles by
various ways.

## SYNOPSIS

**nico-admin-cli attestation measured-boot bundle list**
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*subcommands*\>

## DESCRIPTION

List bundles by various ways.

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
nico-admin-cli attestation measured-boot bundle list all
nico-admin-cli attestation measured-boot bundle list machines my-bundle
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`all`](./attestation-measured-boot-bundle-list-all.md) | List all bundles |
| [`machines`](./attestation-measured-boot-bundle-list-machines.md) | List all machines for a given bundle ID. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
