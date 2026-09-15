# `nico-admin-cli bmc-machine set-root-password`

_[Hardware commands](../../hardware.md) › [bmc-machine](./bmc-machine.md) › **set-root-password**_

## NAME

nico-admin-cli-bmc-machine-set-root-password - Set a BMCs root password
out-of-band (for fleet rotation use \`credential rotate\`)

## SYNOPSIS

**nico-admin-cli bmc-machine set-root-password**
\[**-i**\|**--ip-address**\] \[**--mac-address**\]
\[**-m**\|**--machine**\] \<**--new-password**\> \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Set a BMCs root password out-of-band (for fleet rotation use
\`credential rotate\`)

## OPTIONS

**-i**, **--ip-address** *\<IP_ADDRESS\>*  
IP of the BMC whose root password to set

**--mac-address** *\<MAC_ADDRESS\>*  
MAC of the BMC whose root password to set

**-m**, **--machine** *\<MACHINE\>*  
ID of the machine whose BMC root password to set

**--new-password** *\<NEW_PASSWORD\>*  
New BMC root password to set

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
nico-admin-cli bmc-machine set-root-password --machine 12345678-1234-5678-90ab-cdef01234567 --new-password mynewpassword
nico-admin-cli bmc-machine set-root-password --ip-address 192.0.2.20 --new-password mynewpassword
nico-admin-cli bmc-machine set-root-password --mac-address 00:11:22:33:44:55 --new-password mynewpassword
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
