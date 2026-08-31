# `nico-admin-cli power-shelf force-delete`

*[Hardware commands](../../hardware.md) › [power-shelf](./power-shelf.md) › **force-delete***

## NAME

nico-admin-cli-power-shelf-force-delete - Force delete a power shelf and
optionally its interfaces and BMC suppressions

## SYNOPSIS

**nico-admin-cli power-shelf force-delete**
\[**-d**\|**--delete-interfaces**\] \[**--delete-bmc-suppressions**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*POWER_SHELF_ID*\>

## DESCRIPTION

Force delete a power shelf and optionally its interfaces and BMC
suppressions

## OPTIONS

**-d**, **--delete-interfaces**  
Delete machine interfaces associated with this power shelf.

**--delete-bmc-suppressions**  
Delete BMC suppressions (site explorer and DHCP) for this power shelf
BMC MAC.

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

\<*POWER_SHELF_ID*\>  
Power Shelf ID to force delete.

## Examples

```sh
nico-admin-cli power-shelf force-delete 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli power-shelf force-delete 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces
nico-admin-cli power-shelf force-delete 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces --delete-bmc-suppressions
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
