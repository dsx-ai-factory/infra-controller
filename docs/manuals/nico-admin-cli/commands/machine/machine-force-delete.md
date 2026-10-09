# `nico-admin-cli machine force-delete`

*[Hardware commands](../../hardware.md) › [machine](./machine.md) › **force-delete***

## NAME

nico-admin-cli-machine-force-delete - Force delete a machine

## SYNOPSIS

```text
nico-admin-cli machine force-delete <--machine>
[-d|--delete-interfaces]
[-b|--delete-bmc-interfaces]
[-c|--delete-bmc-credentials]
[--delete-bmc-suppressions]
[--delete-retained-boot-interfaces]
[--release-preserved-addresses]
[--allow-delete-with-instance] [--wait-for-instance-dpu]
[--allow-delete-with-orphaned-dpf-crds] [--extended]
[--sort-by] [-h|--help]
```

## DESCRIPTION

Force delete a machine

## OPTIONS

`--machine <MACHINE>`

UUID, IPv4, MAC or hostname of the host or DPU machine to delete

`-d, --delete-interfaces`

Delete interfaces.

`-b, --delete-bmc-interfaces`

Delete BMC interfaces.

`-c, --delete-bmc-credentials`

Delete BMC credentials. Only applicable if site explorer has configured
credentials for the BMCs associated with this managed host.

`--delete-bmc-suppressions`

Delete Site Explorer and DHCP BMC suppressions for the host/DPU BMC MACs
and underlay (OOB) MACs so rediscovery is not skipped.

`--delete-retained-boot-interfaces`

Delete retained boot-interface pairs for the host/DPU BMC and interface
MACs. Without this, deleted interfaces keep their boot targets for
re-ingestion.

`--release-preserved-addresses`

Release preserved address reservations for deleted interfaces instead of
parking them. Without this, an address marked for preservation is parked
so the same MAC can reclaim it on re-ingestion.

`--allow-delete-with-instance`

Acknowledge deleting a machine with an allocated instance. For deletion
by host ID, remove its instance type association first; this flag does
not bypass that check.

`--wait-for-instance-dpu`

Wait for all attached DPUs to acknowledge the Admin network
configuration before deleting a host that has an Instance when force
deletion starts. Disabled by default; a fresh deletion without this flag
does not wait for DPU acknowledgements. A fresh deletion without an
Instance does not wait.

Once recorded, the wait survives retries; omitting this flag cannot
cancel it. Only servers supporting this option enforce a recorded wait.
An older server can complete deletion without acknowledgement, even if a
newer server already recorded the wait.

An unavailable DPU can prevent completion indefinitely. The CLI waits 5
seconds between polling calls, with a nominal 20-minute retry limit. It
checks the limit after RPC calls, so slow calls can extend the total
time. A timeout does not cancel deletion. This flag does not replace
--allow-delete-with-instance.

`--allow-delete-with-orphaned-dpf-crds`

Delete machine even if DPF CRDs exist and DPF is disabled at the site
level. This flag acknowledges that orphaned DPF resources may remain

`--extended`

Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

`--sort-by <SORT_BY> [default: primary-id]`

Sort output by specified field

*Possible values:*

> - primary-id: Sort by the primary ID
>
> - state: Sort by state

`-h, --help`

Print help (see a summary with -h)

## Examples

```sh
nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567
nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces
nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces --delete-bmc-interfaces --delete-bmc-suppressions --delete-retained-boot-interfaces
nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 --delete-interfaces --release-preserved-addresses
```

---

**Related:** [Hardware commands](../../hardware.md) · [CLI reference index](../../README.md)
