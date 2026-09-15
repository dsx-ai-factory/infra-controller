# `nico-admin-cli ssh`

_[Admin commands](../../admin.md) › **ssh**_

## NAME

nico-admin-cli-ssh - SSH Util functions

## SYNOPSIS

**nico-admin-cli ssh** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

SSH Util functions

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

## Subcommands

| Subcommand | Description |
|---|---|
| [`get-rshim-status`](./ssh-get-rshim-status.md) | Show Rshim Status |
| [`disable-rshim`](./ssh-disable-rshim.md) | Disable Rshim |
| [`enable-rshim`](./ssh-enable-rshim.md) | EnableRshim |
| [`copy-bfb`](./ssh-copy-bfb.md) | Copy BFB to the DPU BMC's RSHIM |
| [`show-obmc-log`](./ssh-show-obmc-log.md) | Show the DPU's BMC's OBMC log |

---

**See also:** [Admin commands](../../admin.md) · [CLI reference index](../../README.md)
