# `nico-admin-cli attestation measured-boot`

*[Hardware commands](../../hardware.md) › [attestation](./attestation.md) › **measured-boot***

## NAME

nico-admin-cli-attestation-measured-boot - Work with measured boot data
(bundles, journals, reports, profiles, site).

## SYNOPSIS

**nico-admin-cli attestation measured-boot** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Work with measured boot data (bundles, journals, reports, profiles,
site).

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
| [`bundle`](./attestation-measured-boot-bundle.md) | Work with golden measurement bundles. |
| [`journal`](./attestation-measured-boot-journal.md) | Work with machine meausrement journals |
| [`report`](./attestation-measured-boot-report.md) | Work with machine reports |
| [`machine`](./attestation-measured-boot-machine.md) | Work with mock-machine entries |
| [`profile`](./attestation-measured-boot-profile.md) | Work with machine hardware profiles |
| [`site`](./attestation-measured-boot-site.md) | Work with site-wide things. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
