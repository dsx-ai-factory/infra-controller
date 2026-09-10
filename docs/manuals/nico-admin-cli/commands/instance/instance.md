# `nico-admin-cli instance`

_[Tenant commands](../../tenant.md) › **instance**_

## NAME

nico-admin-cli-instance - Instance related handling

## SYNOPSIS

**nico-admin-cli instance** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Instance related handling

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
| [`show`](./instance-show.md) | Display instance information |
| [`reboot`](./instance-reboot.md) | Reboot instance, potentially applying firmware updates |
| [`release`](./instance-release.md) | De-allocate instance |
| [`allocate`](./instance-allocate.md) | Allocate instance |
| [`update-os`](./instance-update-os.md) | Update instance OS |
| [`update-ib-config`](./instance-update-ib-config.md) | Update instance IB configuration |
| [`update-nv-link-config`](./instance-update-nv-link-config.md) | Update instance NVLink configuration |
| [`update-spx-config`](./instance-update-spx-config.md) | Update instance SPX configuration |

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../README.md)
