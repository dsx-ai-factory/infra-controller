# `nico-admin-cli attestation measured-boot site`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › [measured-boot](./attestation-measured-boot.md) › **site***

## NAME

nico-admin-cli-attestation-measured-boot-site - Work with site-wide
things.

## SYNOPSIS

**nico-admin-cli attestation measured-boot site** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with site-wide things.

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
| [`import`](./attestation-measured-boot-site-import.md) | Import a site from an export file. |
| [`export`](./attestation-measured-boot-site-export.md) | Export a site to an export file. |
| [`trusted-machine`](./attestation-measured-boot-site-trusted-machine.md) | Managed trusted machines. |
| [`trusted-profile`](./attestation-measured-boot-site-trusted-profile.md) | Managed trusted profiles. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
