# `nico-admin-cli switch force-delete`

_[Hardware commands](../../hardware.md) › [switch](./switch.md) › **force-delete**_

## NAME

nico-admin-cli-switch-force-delete - Force delete a switch and
optionally its interfaces

## SYNOPSIS

**nico-admin-cli switch force-delete**
\[**-d**\|**--delete-interfaces**\] \[**--delete-bmc-suppressions**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]
\<*SWITCH_ID*\>

## DESCRIPTION

Force delete a switch and optionally its interfaces

## OPTIONS

**-d**, **--delete-interfaces**  
Delete machine interfaces associated with this switch, including
interfaces whose MACs match the switch BMC MAC or declared NVOS MACs.

**--delete-bmc-suppressions**  
Delete BMC suppressions (DHCP and Site Explorer) for the switch BMC MAC
and declared NVOS MACs.

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

\<*SWITCH_ID*\>  
Switch ID to force delete.

## Examples

```sh
nico-admin-cli switch force-delete 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli switch force-delete 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces
nico-admin-cli switch force-delete 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces --delete-bmc-suppressions
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
