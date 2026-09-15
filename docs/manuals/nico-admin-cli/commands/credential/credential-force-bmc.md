# `nico-admin-cli credential force-bmc`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **force-bmc**_

## NAME

nico-admin-cli-credential-force-bmc - Force-converge a single BMCs
credentials now (operator escape hatch)

## SYNOPSIS

**nico-admin-cli credential force-bmc** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Force-converge a single BMCs credentials now (operator escape hatch)

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
nico-admin-cli credential force-bmc set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli credential force-bmc set --id sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli credential force-bmc set --id ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli credential force-bmc set --bmc-mac 00:11:22:33:44:55
nico-admin-cli credential force-bmc clear --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`set`](./credential-force-bmc-set.md) | Request an immediate BMC credential rotation of a machine, switch, or power shelf. |
| [`clear`](./credential-force-bmc-clear.md) | Clear a pending BMC force-converge request for a machine, switch, or power shelf. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
