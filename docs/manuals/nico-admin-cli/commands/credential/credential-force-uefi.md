# `nico-admin-cli credential force-uefi`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **force-uefi***

## NAME

nico-admin-cli-credential-force-uefi - Force-converge the UEFI
credential for a single machine now (operator escape hatch)

## SYNOPSIS

**nico-admin-cli credential force-uefi** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Force-converge the UEFI credential for a single machine now (operator
escape hatch)

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
nico-admin-cli credential force-uefi set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli credential force-uefi set --bmc-mac 00:11:22:33:44:55
nico-admin-cli credential force-uefi clear --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./credential-force-uefi-set.md) | Request an immediate UEFI credential rotation of a machine (host). |
| [`clear`](./credential-force-uefi-clear.md) | Clear a pending UEFI force-converge request for a machine (host). |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
