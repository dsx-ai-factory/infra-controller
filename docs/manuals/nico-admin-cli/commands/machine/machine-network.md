# `nico-admin-cli machine network`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **network***

## NAME

nico-admin-cli-machine-network - Networking information

## SYNOPSIS

**nico-admin-cli machine network** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Networking information

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

## Examples

```sh
nico-admin-cli machine network status
nico-admin-cli machine network config --machine-id 12345678-1234-5678-90ab-cdef01234567
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`status`](./machine-network-status.md) | Print network status of all machines |
| [`config`](./machine-network-config.md) | Machine network configuration, used by VPC. |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
