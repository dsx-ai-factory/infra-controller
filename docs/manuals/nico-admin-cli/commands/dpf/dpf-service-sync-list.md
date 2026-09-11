# `nico-admin-cli dpf service-sync list`

*[Hardware commands](../../hardware.md) › [dpf](./dpf.md) › [service-sync](./dpf-service-sync.md) › **list***

## NAME

nico-admin-cli-dpf-service-sync-list - List machines DPF is waiting on
before a DPUService rollout

## SYNOPSIS

**nico-admin-cli dpf service-sync list** \[**--machine-id**\]
\[**--extended**\] \[**--sort-by**\] \[**-h**\|**--help**\]

## DESCRIPTION

List machines DPF is waiting on before a DPUService rollout

## OPTIONS

**--machine-id** *\<MACHINE_ID\>*  
Show recorded history for this host instead of the outstanding worklist

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
nico-admin-cli dpf service-sync list
nico-admin-cli dpf service-sync list --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
