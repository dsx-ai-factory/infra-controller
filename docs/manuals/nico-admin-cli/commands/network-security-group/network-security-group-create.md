# `nico-admin-cli network-security-group create`

_[Network commands](../../network.md) › [network-security-group](./network-security-group.md) › **create**_

## NAME

nico-admin-cli-network-security-group-create - Create a network security
group

## SYNOPSIS

**nico-admin-cli network-security-group create** \[**-i**\|**--id**\]
\<**-t**\|**--tenant-organization-id**\> \[**-n**\|**--name**\]
\[**-d**\|**--description**\] \[**-l**\|**--labels**\]
\[**-s**\|**--stateful-egress**\] \[**-r**\|**--rules**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Create a network security group

## OPTIONS

**-i**, **--id** *\<ID\>*  
Optional, unique ID to use when creating the network security group

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID of the network security group

**-n**, **--name** *\<NAME\>*  
Name of the network security group

**-d**, **--description** *\<DESCRIPTION\>*  
Description of the network security group

**-l**, **--labels** *\<LABELS\>*  
JSON map of simple key:value pairs to be applied as labels to the
network security group

**-s**, **--stateful-egress**  
Optional, whether egress rules are stateful

**-r**, **--rules** *\<RULES\>*  
Optional, JSON array containing a defined set of network security group
rules

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
nico-admin-cli network-security-group create --tenant-organization-id fds34511233a --name web-tier
nico-admin-cli network-security-group create --tenant-organization-id fds34511233a --name web-tier --stateful-egress --labels '{"env":"prod"}'
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
