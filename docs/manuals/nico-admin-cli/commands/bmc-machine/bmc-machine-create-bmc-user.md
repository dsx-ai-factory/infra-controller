# `nico-admin-cli bmc-machine create-bmc-user`

_[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **create-bmc-user**_

## NAME

nico-admin-cli-bmc-machine-create-bmc-user

## SYNOPSIS

**nico-admin-cli bmc-machine create-bmc-user**
\[**-i**\|**--ip-address**\] \[**--mac-address**\]
\[**-m**\|**--machine**\] \<**-u**\|**--username**\>
\<**-p**\|**--password**\> \[**-r**\|**--role-id**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

## OPTIONS

**-i**, **--ip-address** *\<IP_ADDRESS\>*  
IP of the BMC where we want to create a new user

**--mac-address** *\<MAC_ADDRESS\>*  
MAC of the BMC where we want to create a new user

**-m**, **--machine** *\<MACHINE\>*  
ID of the machine where we want to create a new user

**-u**, **--username** *\<USERNAME\>*  
Username of new BMC account

**-p**, **--password** *\<PASSWORD\>*  
Password of new BMC account

**-r**, **--role-id** *\<ROLE_ID\>*  
Role of new BMC account (default: administrator)\

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
nico-admin-cli bmc-machine create-bmc-user --machine 12345678-1234-5678-90ab-cdef01234567 --username admin --password mynewpassword
nico-admin-cli bmc-machine create-bmc-user --ip-address 192.0.2.20 --username admin --password mynewpassword
nico-admin-cli bmc-machine create-bmc-user --mac-address 00:11:22:33:44:55 --username admin --password mynewpassword
nico-admin-cli bmc-machine create-bmc-user --machine 12345678-1234-5678-90ab-cdef01234567 --username admin --password mynewpassword --role-id readonly
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
