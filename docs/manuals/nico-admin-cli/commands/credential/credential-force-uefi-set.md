# `nico-admin-cli credential force-uefi set`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › [force-uefi](./credential-force-uefi.md) › **set**_

## NAME

nico-admin-cli-credential-force-uefi-set - Request an immediate UEFI
credential rotation of a machine (host).

## SYNOPSIS

**nico-admin-cli credential force-uefi set** \[**-i**\|**--id**\]
\[**--bmc-mac**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Request an immediate UEFI credential rotation of a machine (host).

## OPTIONS

**-i**, **--id** *\<ID\>*  
Machine ID that owns the UEFI credential (a host machine). Provide this
or --bmc-mac.

**--bmc-mac** *\<BMC_MAC\>*  
MAC of the machines BMC. Provide this or --id; if both are given they
must identify the same machine.

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
nico-admin-cli credential force-uefi set --id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli credential force-uefi set --bmc-mac 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
