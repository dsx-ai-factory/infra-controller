# `nico-admin-cli vpc set-virtualizer`

*[Network commands](../../network.md) › [vpc](./vpc.md) › **set-virtualizer***

## NAME

nico-admin-cli-vpc-set-virtualizer

## SYNOPSIS

**nico-admin-cli vpc set-virtualizer** \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*ID*\> \<*VIRTUALIZER*\>

## DESCRIPTION

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

\<*ID*\>  
The VPC ID for the VPC to update

\<*VIRTUALIZER*\>  
The virtualizer to use for this VPC\

\
*Possible values:*

- ethernet-virtualizer

- fnn

- flat: \`Flat\` is for VPCs whose tenant instances live directly on the
  underlay (zero-DPU hosts, or hosts with their DPU in NIC mode) and
  whose interfaces are bound to \`HostInband\` network segments rather
  than a NICo-managed overlay. Flat VPCs are still real tenant VPCs with
  a VNI and NSGs, but NICo doesnt drive their data plane -- routing and
  ACL enforcement between Flat VPCs and other VPCs is the network
  operators responsibility

## Examples

```sh
nico-admin-cli vpc set-virtualizer 12345678-1234-5678-90ab-cdef01234567 fnn
```

---

**See also:** [Network commands](../../network.md) · [CLI reference index](../../index.md)
