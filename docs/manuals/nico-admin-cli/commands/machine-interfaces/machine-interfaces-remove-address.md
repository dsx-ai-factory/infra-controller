# `nico-admin-cli machine-interfaces remove-address`

*[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **remove-address***

## NAME

nico-admin-cli-machine-interfaces-remove-address - Remove a static
address from a machine interface

## SYNOPSIS

**nico-admin-cli machine-interfaces remove-address** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*INTERFACE_ID*\>
\<*IP_ADDRESS*\>

## DESCRIPTION

Remove a static address from a machine interface

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

\<*INTERFACE_ID*\>  
The machine interface ID to remove the address from

\<*IP_ADDRESS*\>  
The IP address to remove

## Examples

```sh
nico-admin-cli machine-interfaces remove-address 12345678-1234-5678-90ab-cdef01234567 10.0.0.5
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
