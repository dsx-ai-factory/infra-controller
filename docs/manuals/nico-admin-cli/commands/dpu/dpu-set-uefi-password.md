# `nico-admin-cli dpu set-uefi-password`

_[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › **set-uefi-password**_

## NAME

nico-admin-cli-dpu-set-uefi-password - Set DPU UEFI password directly on
the device (via Redfish)

## SYNOPSIS

**nico-admin-cli dpu set-uefi-password** \<**-q**\|**--query**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set DPU UEFI password directly on the device (via Redfish)

## OPTIONS

**-q**, **--query** *\<QUERY\>*  
ID, IPv4, MAC or hostnmame of the machine to query

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
nico-admin-cli dpu set-uefi-password --query fm100ds038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg
nico-admin-cli dpu set-uefi-password --query 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
