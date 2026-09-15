# `nico-admin-cli network-security-group detach`

_[Network commands](../../network.md) › [network-security-group](./network-security-group.md) › **detach**_

## NAME

nico-admin-cli-network-security-group-detach - Remove a network security
group from a VPC or instance

## SYNOPSIS

**nico-admin-cli network-security-group detach**
\[**-v**\|**--vpc-id**\] \[**-i**\|**--instance-id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Remove a network security group from a VPC or instance

## OPTIONS

**-v**, **--vpc-id** *\<VPC_ID\>*  
Optional, VPC ID that should have the network security group removed

**-i**, **--instance-id** *\<INSTANCE_ID\>*  
Optional, Instance ID that should have the network security group
removed

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
nico-admin-cli network-security-group detach --vpc-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli network-security-group detach --instance-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../README.md)
