# `nico-admin-cli instance allocate`

*[Tenant commands](../../tenant.md) › [instance](./instance.md) › **allocate***

## NAME

nico-admin-cli-instance-allocate - Allocate instance

## SYNOPSIS

**nico-admin-cli instance allocate** \[**-n**\|**--number**\]
\[**-s**\|**--subnet**\] \[**-t**\|**--tenant-org**\]
\<**-p**\|**--prefix-name**\> \[**--label-key**\] \[**--label-value**\]
\[**--network-security-group-id**\] \[**--instance-type-id**\]
\[**--os**\] \[**--spxconfig**\] \[**--vf-subnet**\]
\[**-v**\|**--vpc-prefix-id**\] \[**--flat-vpc-id**\] \[**--extended**\]
\[**--vf-vpc-prefix-id**\] \[**--ip-address**\] \[**--vf-ip-address**\]
\[**--ipv6-vpc-prefix-id**\] \[**--ipv6-vf-prefix-id**\]
\[**--ipv6-ip-address**\] \[**--ipv6-vf-ip-address**\]
\[**--machine-id**\] \[**--transactional**\] \[**--sort-by**\]
\[**-h**\|**--help**\]

## DESCRIPTION

Allocate instance

## OPTIONS

**-n**, **--number** *\<NUMBER\>*  
**-s**, **--subnet** *\<SUBNET\>*  
The subnet to assign to a PF

**-t**, **--tenant-org** *\<TENANT_ORG\>*  
**-p**, **--prefix-name** *\<PREFIX_NAME\>*  
**--label-key** *\<LABEL_KEY\>*  
The key of label instance to query

**--label-value** *\<LABEL_VALUE\>*  
The value of label instance to query

**--network-security-group-id** *\<NETWORK_SECURITY_GROUP_ID\>*  
The ID of a network security group to apply to the new instance upon
creation

**--instance-type-id** *\<INSTANCE_TYPE_ID\>*  
The expected instance type id for the instance, which will be compared
to type ID set for the machine of the request

**--os** *\<OS_JSON\>*  
OS definition in JSON format

**--spxconfig** *\<SPX_JSON\>*  
SPX configuration in JSON format

**--vf-subnet** *\<VF_SUBNET\>*  
The subnet to assign to a VF

**-v**, **--vpc-prefix-id** *\<VPC_PREFIX_ID\>*  
The VPC prefix to assign to a PF

**--flat-vpc-id** *\<FLAT_VPC_ID\>*  
Create an instance in the given "flat" VPC, for machines without DPUs

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--vf-vpc-prefix-id** *\<VF_VPC_PREFIX_ID\>*  
The VPC prefix to assign to a VF

**--ip-address** *\<IP_ADDRESS\>*  
Explicit IPv4 address to request for each PF interface

**--vf-ip-address** *\<VF_IP_ADDRESS\>*  
Explicit IPv4 address to request for each VF interface

**--ipv6-vpc-prefix-id** *\<IPV6_VPC_PREFIX_ID\>*  
IPv6 VPC prefix to pair with each PF vpc-prefix-id for dual-stack

**--ipv6-vf-prefix-id** *\<IPV6_VF_PREFIX_ID\>*  
IPv6 VPC prefix to pair with each VF vf-vpc-prefix-id for dual-stack

**--ipv6-ip-address** *\<IPV6_IP_ADDRESS\>*  
Explicit IPv6 address to request for each PF interface (dual-stack)

**--ipv6-vf-ip-address** *\<IPV6_VF_IP_ADDRESS\>*  
Explicit IPv6 address to request for each VF interface (dual-stack)

**--machine-id** *\<MACHINE_ID\>*  
The machine ids for the machines to use (instead of searching)

**--transactional**  
Use batch API for all-or-nothing allocation (requires --number \> 1)

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
nico-admin-cli instance allocate --prefix-name eth0 --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance allocate --prefix-name eth0 --subnet 192.0.2.0/24
nico-admin-cli instance allocate --number 4 --prefix-name eth0 --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance allocate --number 4 --prefix-name eth0 --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567 --transactional
nico-admin-cli instance allocate --prefix-name eth0 --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567 --machine-id 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli instance allocate --prefix-name eth0 --vpc-prefix-id 12345678-1234-5678-90ab-cdef01234567 --instance-type-id abcdef01-2345-6789-abcd-ef0123456789 --network-security-group-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Tenant commands](../../tenant.md) · [CLI reference index](../../index.md)
