# `nico-admin-cli credential force-bmc set`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › [force-bmc](./credential-force-bmc.md) › **set**_

## NAME

nico-admin-cli-credential-force-bmc-set - Request an immediate BMC
credential rotation of a machine, switch, or power shelf.

## SYNOPSIS

**nico-admin-cli credential force-bmc set** \[**-i**\|**--id**\]
\[**--bmc-mac**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Request an immediate BMC credential rotation of a machine, switch, or
power shelf.

## OPTIONS

**-i**, **--id** *\<ID\>*  
ID of the machine, DPU, switch, or power shelf that owns the BMC.
Provide this or --bmc-mac.

**--bmc-mac** *\<BMC_MAC\>*  
MAC of the BMC to target (machine, switch, or power shelf). Provide this
or --id; if an id is also given they must identify the same device.

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--sort-by** *\<SORT_BY\>* \[default: primary-id\]  
Sort output by specified field  

  
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
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
