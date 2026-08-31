# `nico-admin-cli managed-host set-primary-dpu`

*[Hardware commands](../../hardware.md) › [managed-host](./managed-host.md) › **set-primary-dpu***

## NAME

nico-admin-cli-managed-host-set-primary-dpu - Deprecated: use
set-primary-interface with a machine-interface ID, not a DPU machine ID

## SYNOPSIS

**nico-admin-cli managed-host set-primary-dpu**
\[**--force-reconcile**\] \[**--reboot**\] \[**--extended**\]
\[**--sort-by**\] \[**-h**\|**--help**\] \<*HOST_MACHINE_ID*\>
\<*DPU_MACHINE_ID*\>

## DESCRIPTION

Deprecated compatibility form for managed hosts. This command accepts a
DPU_MACHINE_ID, chooses the host-facing interface row owned by that DPU,
and atomically updates the primary interface, Admin network identity,
and persisted desired boot target. The selected interface must be on the
Admin segment when the host has a DPU-backed Admin interface. The
machine-controller converges the BMC to the desired target when the host
is eligible. Use set-primary-interface with an INTERFACE_ID
(machine-interface ID):
[primary-interface command documentation](https://github.com/NVIDIA/infra-controller/blob/main/docs/manuals/nico-admin-cli/commands/managed-host/managed-host-set-primary-interface.md)

## OPTIONS

**--force-reconcile**  
Request a fresh machine-controller reconciliation even when this DPU is
already selected. Sends only force_reconcile=true; servers without
force_reconcile support ignore it, while supporting servers leave any
required restart to machine-controller

**--reboot**  
Deprecated compatibility option for servers without force_reconcile
support. Sends reboot=true and force_reconcile=true; supporting servers
treat it as reconciliation, while older servers force-restart the host
after changing the target

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
ID of the host machine

\<*DPU_MACHINE_ID*\>  
ID of the DPU machine to make primary

## Examples

```sh
nico-admin-cli managed-host set-primary-dpu 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli managed-host set-primary-dpu 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789 --force-reconcile
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
