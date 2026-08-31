# `nico-admin-cli power-shelf maintenance power-on`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › [maintenance](./power-shelf-maintenance.md) › **power-on***

## NAME

nico-admin-cli-power-shelf-maintenance-power-on - Request the listed
power shelves to power on

## SYNOPSIS

**nico-admin-cli power-shelf maintenance power-on**
\<**--power-shelf-id**\> \[**--reference**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Request the listed power shelves to power on

## OPTIONS

**--power-shelf-id** *\<POWER_SHELF_ID\>...*  
One or more Power Shelf IDs to drive into maintenance

**--reference** *\<REFERENCE\>*  
URL of reference (ticket, issue, etc) for this maintenance request

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
nico-admin-cli power-shelf maintenance power-on --power-shelf-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli power-shelf maintenance power-on --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789 --reference https://tickets.example.com/PS-42
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
