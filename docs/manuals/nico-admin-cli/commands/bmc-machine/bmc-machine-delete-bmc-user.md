# `nico-admin-cli bmc-machine delete-bmc-user`

*[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **delete-bmc-user***

## NAME

nico-admin-cli-bmc-machine-delete-bmc-user

## SYNOPSIS

**nico-admin-cli bmc-machine delete-bmc-user**
\[**-i**\|**--ip-address**\] \[**--mac-address**\]
\[**-m**\|**--machine**\] \<**-u**\|**--username**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

## OPTIONS

**-i**, **--ip-address** *\<IP_ADDRESS\>*  
IP of the BMC where we want to delete a user

**--mac-address** *\<MAC_ADDRESS\>*  
MAC of the BMC where we want to delete a user

**-m**, **--machine** *\<MACHINE\>*  
ID of the machine where we want to delete a user

**-u**, **--username** *\<USERNAME\>*  
Username of BMC account to delete

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
nico-admin-cli bmc-machine delete-bmc-user --machine 12345678-1234-5678-90ab-cdef01234567 --username admin
nico-admin-cli bmc-machine delete-bmc-user --ip-address 192.0.2.20 --username admin
nico-admin-cli bmc-machine delete-bmc-user --mac-address 00:11:22:33:44:55 --username admin
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
