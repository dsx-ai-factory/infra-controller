# `nico-admin-cli ssh show-obmc-log`

_[Admin commands](../../admin.md) › [ssh](./ssh.md) › **show-obmc-log**_

## NAME

nico-admin-cli-ssh-show-obmc-log - Show the DPUs BMCs OBMC log

## SYNOPSIS

**nico-admin-cli ssh show-obmc-log** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*BMC_IP_ADDRESS*\>
\<*BMC_USERNAME*\> \<*BMC_PASSWORD*\>

## DESCRIPTION

Show the DPUs BMCs OBMC log

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
nico-admin-cli ssh show-obmc-log 192.0.2.10:22 admin mypassword
```

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
