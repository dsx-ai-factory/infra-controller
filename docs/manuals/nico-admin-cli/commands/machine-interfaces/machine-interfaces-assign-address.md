# `nico-admin-cli machine-interfaces assign-address`

*[Hardware commands](../../hardware.md) › [machine-interfaces](./machine-interfaces.md) › **assign-address***

## NAME

nico-admin-cli-machine-interfaces-assign-address - Assign a static
address to a machine interface

## SYNOPSIS

**nico-admin-cli machine-interfaces assign-address** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*INTERFACE_ID*\>
\<*IP_ADDRESS*\>

## DESCRIPTION

Assign a static address to a machine interface

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
The machine interface ID to assign the address to

\<*IP_ADDRESS*\>  
The IP address to assign (IPv4 or IPv6)

## Examples

```sh
nico-admin-cli machine-interfaces assign-address 12345678-1234-5678-90ab-cdef01234567 192.0.2.20
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
