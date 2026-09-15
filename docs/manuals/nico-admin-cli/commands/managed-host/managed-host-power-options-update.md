# `nico-admin-cli managed-host power-options update`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [power-options](./managed-host-power-options.md) › **update**_

## NAME

nico-admin-cli-managed-host-power-options-update

## SYNOPSIS

**nico-admin-cli managed-host power-options update**
\<**-d**\|**--desired-power-state**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*MACHINE*\>

## DESCRIPTION

## OPTIONS

**-d**, **--desired-power-state** *\<DESIRED_POWER_STATE\>*  
Desired Power State\

\
*Possible values:*

- on

- off

- power-manager-disabled

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

\<*MACHINE*\>  
ID of the host

## Examples

```sh
nico-admin-cli managed-host power-options update 12345678-1234-5678-90ab-cdef01234567 --desired-power-state off
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
