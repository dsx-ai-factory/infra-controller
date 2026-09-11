# `nico-admin-cli boot-interface set`

*[Hardware commands](../../hardware.md) › [boot-interface](./boot-interface.md) › **set***

## NAME

nico-admin-cli-boot-interface-set - Set the boot interface for a managed
host (promotes it to the primary interface)

## SYNOPSIS

**nico-admin-cli boot-interface set** \[**--force-reconcile**\]
\[**--reboot**\] \[**--extended**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*MACHINE*\> \<*INTERFACE*\>

## DESCRIPTION

Make an interface both the primary interface and persisted desired boot
target for a managed host. This is the same operation as \`managed-host
set-primary-interface\`: the primary row and desired target commit
together, then machine-controller reconciles the BMC when the host is
eligible. The interface can be named by machine-interface UUID or by MAC
address; a MAC must match exactly one managed interface row on the
machine. If the host has a DPU-backed Admin interface, the selected
interface must also be on the Admin segment.

## OPTIONS

**--force-reconcile**  
Request a fresh machine-controller reconciliation even when this
interface is already selected. Sends only force_reconcile=true; servers
without force_reconcile support ignore it, while supporting servers
leave any required restart to machine-controller

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

\<*MACHINE*\>  
The managed host for which to set the boot interface

\<*INTERFACE*\>  
The interface to boot from -- a machine-interface UUID or a MAC address

## Examples

```sh
nico-admin-cli boot-interface set 12345678-1234-5678-90ab-cdef01234567 00:11:22:33:44:55
nico-admin-cli boot-interface set 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789
nico-admin-cli boot-interface set 12345678-1234-5678-90ab-cdef01234567 00:11:22:33:44:55 --force-reconcile
```

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
