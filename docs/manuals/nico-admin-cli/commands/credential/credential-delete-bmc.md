# `nico-admin-cli credential delete-bmc`

*[Hardware commands](../../hardware.md) › [credential](./credential.md) › **delete-bmc***

## NAME

nico-admin-cli-credential-delete-bmc - Delete BMC credentials

## SYNOPSIS

**nico-admin-cli credential delete-bmc** \<**--kind**\>
\[**--mac-address**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Delete BMC credentials

## OPTIONS

**--kind**=*\<KIND\>*  
The BMC Credential kind\

\
*Possible values:*

- site-wide-root

- bmc-root

- bmc-forge-admin

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
nico-admin-cli credential delete-bmc --kind=site-wide-root
nico-admin-cli credential delete-bmc --kind=bmc-root --mac-address 00:11:22:33:44:55
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
