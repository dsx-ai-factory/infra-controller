# `nico-admin-cli redfish create-bmc-user`

*[Hardware commands](../../hardware.md) › [redfish](./redfish.md) › **create-bmc-user***

## NAME

nico-admin-cli-redfish-create-bmc-user - Create new BMC user

## SYNOPSIS

**nico-admin-cli redfish create-bmc-user** \<**--new-password**\>
\<**--user**\> \[**--role-id**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Create new BMC user

## OPTIONS

**--new-password** *\<NEW_PASSWORD\>*  
BMC password

**--user** *\<USER\>*  
BMC user

**--role-id** *\<ROLE_ID\>*  
BMC role for the new account (default: administrator)\

\
*Possible values:*

- administrator

- operator

- readonly

- noaccess

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword create-bmc-user --user svc-ops --new-password 'mynewpassword'
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword create-bmc-user --user auditor --new-password 'mynewpassword' --role-id readonly
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
