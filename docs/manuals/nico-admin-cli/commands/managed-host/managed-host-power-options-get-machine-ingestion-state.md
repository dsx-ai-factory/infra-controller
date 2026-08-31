# `nico-admin-cli managed-host power-options get-machine-ingestion-state`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [power-options](./managed-host-power-options.md) › **get-machine-ingestion-state***

## NAME

nico-admin-cli-managed-host-power-options-get-machine-ingestion-state -
Get machine ingestion state

## SYNOPSIS

**nico-admin-cli managed-host power-options
get-machine-ingestion-state** \<**-m**\|**--mac-address**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Get machine ingestion state

## OPTIONS

**-m**, **--mac-address** *\<MAC_ADDRESS\>*  
MAC Address of host BMC endpoint

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
nico-admin-cli managed-host power-options get-machine-ingestion-state --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
