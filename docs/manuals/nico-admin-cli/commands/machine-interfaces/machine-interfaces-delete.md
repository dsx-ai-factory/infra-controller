# `nico-admin-cli machine-interfaces delete`

_[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **delete**_

## NAME

nico-admin-cli-machine-interfaces-delete - Delete Machine interface.

## SYNOPSIS

**nico-admin-cli machine-interfaces delete** \[**--mac-address**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\[*INTERFACE_ID*\]

## DESCRIPTION

Delete Machine interface.

## OPTIONS

**--mac-address** *\<MAC_ADDRESS\>*  
Delete every interface carrying this MAC address instead of selecting by
ID.

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

\[*INTERFACE_ID*\]  
The interface ID to delete.

## Examples

```sh
nico-admin-cli machine-interfaces delete 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine-interfaces delete --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
