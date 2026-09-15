# `nico-admin-cli network-security-group`

_[Network commands](../../network.md) › **network-security-group**_

## NAME

nico-admin-cli-network-security-group - Network security group
management

## SYNOPSIS

**nico-admin-cli network-security-group** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Network security group management

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
| [`create`](./network-security-group-create.md) | Create a network security group |
| [`show`](./network-security-group-show.md) | Show one or more network security groups |
| [`delete`](./network-security-group-delete.md) | Delete a network security group |
| [`update`](./network-security-group-update.md) | Update a network security group |
| [`show-attachments`](./network-security-group-show-attachments.md) | Show info about the objects referencing a network security group |
| [`attach`](./network-security-group-attach.md) | Attach a network security group to a VPC or instance |
| [`detach`](./network-security-group-detach.md) | Remove a network security group from a VPC or instance |

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
