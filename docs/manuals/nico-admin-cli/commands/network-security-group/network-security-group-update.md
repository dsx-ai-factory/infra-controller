# `nico-admin-cli network-security-group update`

*[Network commands](../../network.md) › [network-security-group](./network-security-group.md) › **update***

## NAME

nico-admin-cli-network-security-group-update - Update a network security
group

## SYNOPSIS

**nico-admin-cli network-security-group update** \<**-i**\|**--id**\>
\<**-t**\|**--tenant-organization-id**\> \[**-n**\|**--name**\]
\[**-d**\|**--description**\] \[**-l**\|**--labels**\]
\[**-s**\|**--stateful-egress**\] \[**-r**\|**--rules**\]
\[**-v**\|**--version**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Update a network security group

## OPTIONS

**-i**, **--id** *\<ID\>*  
Network security group ID to update

**-t**, **--tenant-organization-id** *\<TENANT_ORGANIZATION_ID\>*  
Tenant organization ID of the network security group

**-n**, **--name** *\<NAME\>*  
Name of the network security group

**-d**, **--description** *\<DESCRIPTION\>*  
Description of the network security group

**-l**, **--labels** *\<LABELS\>*  
JSON map of simple key:value pairs to be applied as labels to the
network security group - will COMPLETELY overwrite any existing labels

**-s**, **--stateful-egress** *\<STATEFUL_EGRESS\>*  
Optional, whether egress rules are stateful\

\
*Possible values:*

- true

- false

**-r**, **--rules** *\<RULES\>*  
Optional, JSON array containing a defined set of network security group
rules - will COMPLETELY overwrite any existing rules

**-v**, **--version** *\<VERSION\>*  
Optional, version to use for comparison when performing the update,
which will be rejected if the actual version of the record does not
match the value of this parameter

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
nico-admin-cli network-security-group update --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --name web-tier
nico-admin-cli network-security-group update --id 12345678-1234-5678-90ab-cdef01234567 --tenant-organization-id fds34511233a --rules '[...]'
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
