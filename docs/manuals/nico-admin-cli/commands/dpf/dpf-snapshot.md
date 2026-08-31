# `nico-admin-cli dpf snapshot`

*[Hardware commands](../../hardware.md) › [dpf](./dpf.md) › **snapshot***

## NAME

nico-admin-cli-dpf-snapshot - Snapshot DPF CRs (DPUNode, DPUDevices,
DPUs) for a host

## SYNOPSIS

**nico-admin-cli dpf snapshot** \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*HOST_MACHINE_ID*\>

## DESCRIPTION

Snapshot DPF CRs (DPUNode, DPUDevices, DPUs) for a host

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

\<*HOST_MACHINE_ID*\>  
Host machine id to snapshot. Must be a host (not a DPU) machine id

## Examples

```sh
nico-admin-cli dpf snapshot 12345678-1234-5678-90ab-cdef01234567
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
