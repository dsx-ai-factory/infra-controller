# `nico-admin-cli vpc create`

*[Network commands](../../network.md) › [vpc](./vpc.md) › **create***

## NAME

nico-admin-cli-vpc-create - Create VPC

## SYNOPSIS

**nico-admin-cli vpc create** \<**--name**\> \[**--description**\]
\[**--id**\] \<**--org-id**\> \[**--virtualization-type**\]
\[**--slaac-enabled**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Create VPC

## OPTIONS

**--name** *\<NAME\>*  
Name to give the new VPC

**--description** *\<DESCRIPTION\>*  
Description for the new VPC

**--id** *\<VpcId\>*  
Optional VPC ID to use instead of allowing the API server to generate
one

**--org-id** *\<ORG_ID\>*  
Tenant organization ID (Plain text string, used by cloud API)

**--virtualization-type** *\<VIRTUALIZATION_TYPE\>* \[default: ethernet-virtualizer\]  
Network virtualization type\

\
*Possible values:*

- ethernet-virtualizer

- ethernet-virtualizer-with-nvue: Deprecated. NVUE is implied; use
  ethernet-virtualizer

- fnn-classic: Deprecated. Use fnn

- fnn-l3: Deprecated. Use fnn

- fnn

- flat: Flat networking for instances attached directly to the underlay.
  NICo does not manage routing or ACL enforcement between Flat VPCs and
  other VPCs; that is the responsibility of the network operator

**--slaac-enabled** *\<SLAAC_ENABLED\>*  
Whether Core should allocate an IPv6 /64 for each IPv6-enabled instance
interface. Supported only for FNN VPCs; NICo does not configure router
advertisements. Enabling requires the connected Core to advertise VPC
SLAAC support and fails otherwise. Omit or set false to disable. This
setting cannot be changed after creation\

\
*Possible values:*

- true

- false

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
nico-admin-cli --cloud-unsafe-op=my_username vpc create --name tenant-vpc-1 --org-id tenant-org-1
nico-admin-cli --cloud-unsafe-op=my_username vpc create --name tenant-vpc-1 --org-id tenant-org-1 --id ad1f9fd5-8438-4407-b259-72fdb7896d42 --virtualization-type flat
nico-admin-cli --cloud-unsafe-op=admin vpc create --name tenant-vpc-1 --org-id fds34511233a --virtualization-type fnn --slaac-enabled true
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
