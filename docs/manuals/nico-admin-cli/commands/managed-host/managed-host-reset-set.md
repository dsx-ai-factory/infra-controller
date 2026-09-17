# `nico-admin-cli managed-host reset set`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [reset](./managed-host-reset.md) › **set**_

## NAME

nico-admin-cli-managed-host-reset-set - Request a reset of a managed
host.

## SYNOPSIS

**nico-admin-cli managed-host reset set** \<**--machine**\>
\[**--allow-reset-with-instance**\] \[**--update-message**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Request a reset of a managed host.

## OPTIONS

**--machine** *\<MACHINE\>*  
Managed host machine ID to reset.

**--allow-reset-with-instance**  
Acknowledge that resetting an assigned host destroys the live instance
and its data. Required when the host has an instance.

**--update-message** *\<UPDATE_MESSAGE\>*  
If set, a HostUpdateInProgress health alert with this message is applied
to the host. The alert is a precondition for the reset.

**--extended**  
Extended result output.

This used by measured boot, where basic output contains just what you
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
nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 --update-message "recovering wedged DPU"
nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 --allow-reset-with-instance --update-message "forced recovery"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
