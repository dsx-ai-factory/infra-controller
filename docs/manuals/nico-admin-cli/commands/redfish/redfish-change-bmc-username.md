# `nico-admin-cli redfish change-bmc-username`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **change-bmc-username**_

## NAME

nico-admin-cli-redfish-change-bmc-username - Rename an account

## SYNOPSIS

**nico-admin-cli redfish change-bmc-username** \<**--old-user**\>
\<**--new-user**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Rename an account

## OPTIONS

**--old-user** *\<OLD_USER\>*  
Old username

**--new-user** *\<NEW_USER\>*  
New username

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword change-bmc-username --old-user svc-ops --new-user svc-platform
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
