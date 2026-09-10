# `nico-admin-cli network-segment`

_[Network commands](../../network.md) › **network-segment**_

## NAME

nico-admin-cli-network-segment - Network Segment related handling

## SYNOPSIS

**nico-admin-cli network-segment** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Network Segment related handling

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
| [`show`](./network-segment-show.md) | Display Network Segment information |
| [`attach-vpc`](./network-segment-attach-vpc.md) | Attach Network Segment to VPC |
| [`delete`](./network-segment-delete.md) | Delete Network Segment |
| [`create`](./network-segment-create.md) | Create Network Segment |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
