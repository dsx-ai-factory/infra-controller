# `nico-admin-cli redfish change-uefi-password`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **change-uefi-password***

## NAME

nico-admin-cli-redfish-change-uefi-password - Change UEFI password

## SYNOPSIS

**nico-admin-cli redfish change-uefi-password**
\<**--current-password**\> \<**--new-password**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Change UEFI password

## OPTIONS

**--current-password** *\<CURRENT_PASSWORD\>*  
Current UEFI password

**--new-password** *\<NEW_PASSWORD\>*  
New UEFI password

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword change-uefi-password --current-password mycurrentpassword --new-password mynewpassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
