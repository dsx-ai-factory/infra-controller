# `nico-admin-cli managed-host power-options allow-ingestion-and-power-on`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [power-options](./managed-host-power-options.md) › **allow-ingestion-and-power-on***

## NAME

nico-admin-cli-managed-host-power-options-allow-ingestion-and-power-on -
Allow a machine to power on

## SYNOPSIS

**nico-admin-cli managed-host power-options
allow-ingestion-and-power-on** \<**-m**\|**--mac-address**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Allow a machine to power on

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
nico-admin-cli managed-host power-options allow-ingestion-and-power-on --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
