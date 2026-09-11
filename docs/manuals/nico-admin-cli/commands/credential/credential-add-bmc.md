# `nico-admin-cli credential add-bmc`

_[Hardware commands](../../hardware.md) › [credential](./credential.md) › **add-bmc**_

## NAME

nico-admin-cli-credential-add-bmc - Add BMC credentials

## SYNOPSIS

**nico-admin-cli credential add-bmc** \<**--kind**\> \<**--password**\>
\[**--username**\] \[**--mac-address**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Add BMC credentials

## OPTIONS

**--kind**=*\<KIND\>*  
The BMC Credential kind\

\
*Possible values:*

- site-wide-root

- bmc-root

- bmc-forge-admin

**--password** *\<PASSWORD\>*  
The password of BMC

**--username** *\<USERNAME\>*  
The username of BMC

**--mac-address** *\<MAC_ADDRESS\>*  
The MAC address of the BMC

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
nico-admin-cli credential add-bmc --kind=site-wide-root --username admin --password mypassword
nico-admin-cli credential add-bmc --kind=bmc-root --username admin --password mypassword --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
