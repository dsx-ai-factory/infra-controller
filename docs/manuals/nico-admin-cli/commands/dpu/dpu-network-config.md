# `nico-admin-cli dpu network config`

*[Hardware commands](../../hardware.md) › [dpu](./dpu.md) › [network](./dpu-network.md) › **config***

## NAME

nico-admin-cli-dpu-network-config - Machine network configuration, used
by VPC.

## SYNOPSIS

**nico-admin-cli dpu network config** \<**--machine-id**\>
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

Machine network configuration, used by VPC.

## OPTIONS

**--machine-id** *\<MACHINE_ID\>*  
DPU machine id

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
nico-admin-cli dpu network config --machine-id 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
