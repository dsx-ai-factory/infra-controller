# `nico-admin-cli managed-host quarantine on`

_[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › [quarantine](./managed-host-quarantine.md) › **on**_

## NAME

nico-admin-cli-managed-host-quarantine-on - Put this machine into
quarantine. Prevents any network access on the host machine

## SYNOPSIS

**nico-admin-cli managed-host quarantine on** \<**--host**\>
\<**--reason**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Put this machine into quarantine. Prevents any network access on the
host machine

## OPTIONS

**--host** *\<HOST\>*  
Managed Host ID

**--reason** *\<REASON\>*  
Reason for quarantining this host

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
nico-admin-cli managed-host quarantine on --host 12345678-1234-5678-90ab-cdef01234567 --reason "suspected compromise"
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
