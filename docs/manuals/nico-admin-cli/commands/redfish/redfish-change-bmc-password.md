# `nico-admin-cli redfish change-bmc-password`

_[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **change-bmc-password**_

## NAME

nico-admin-cli-redfish-change-bmc-password - Change password for a BMC
user

## SYNOPSIS

**nico-admin-cli redfish change-bmc-password** \<**--new-password**\>
\<**--user**\> \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Change password for a BMC user

## OPTIONS

**--new-password** *\<NEW_PASSWORD\>*  
New BMC password

**--user** *\<USER\>*  
BMC user

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword change-bmc-password --user svc-ops --new-password 'mynewpassword'
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
