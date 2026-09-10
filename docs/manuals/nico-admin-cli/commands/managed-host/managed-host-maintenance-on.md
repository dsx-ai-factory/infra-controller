# `nico-admin-cli managed-host maintenance on`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [maintenance](./managed-host-maintenance.md) › **on**_

## NAME

nico-admin-cli-managed-host-maintenance-on - Put this machine into
maintenance mode. Prevents an instance being assigned to it

## SYNOPSIS

**nico-admin-cli managed-host maintenance on** \<**--host**\>
\<**--reference**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Put this machine into maintenance mode. Prevents an instance being
assigned to it

## OPTIONS

**--host** *\<HOST\>*  
Managed Host ID

**--reference** *\<REFERENCE\>*  
URL of reference (ticket, issue, etc) for this machines maintenance

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
nico-admin-cli managed-host maintenance on --host 12345678-1234-5678-90ab-cdef01234567 --reference https://tickets.example.com/MH-42
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
