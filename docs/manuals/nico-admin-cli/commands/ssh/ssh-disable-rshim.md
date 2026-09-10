# `nico-admin-cli ssh disable-rshim`

_[Admin commands](../../admin.md) › [ssh](./ssh.md) › **disable-rshim**_

## NAME

nico-admin-cli-ssh-disable-rshim - Disable Rshim

## SYNOPSIS

**nico-admin-cli ssh disable-rshim** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*BMC_IP_ADDRESS*\>
\<*BMC_USERNAME*\> \<*BMC_PASSWORD*\>

## DESCRIPTION

Disable Rshim

## OPTIONS

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

\<*BMC_IP_ADDRESS*\>  
BMC IP Address

\<*BMC_USERNAME*\>  
BMC Username

\<*BMC_PASSWORD*\>  
BMC Password

## Examples

```sh
nico-admin-cli ssh disable-rshim 192.0.2.10:22 admin mypassword
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
