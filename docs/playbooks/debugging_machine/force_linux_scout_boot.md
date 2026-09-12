# Force a Managed Host to Boot into Linux Scout

Use this playbook when you need the Linux Scout environment for managed-host
diagnostics and the normal NICo boot workflow does not select it.

## Prerequisites

- Access to the managed host's serial or remote console.
- The static PXE URL that the managed host can reach.
- The managed host CPU architecture: x86-64 or Arm.

## Open the iPXE Debug Shell

1. Boot the managed host from its BlueField DPU network interface.
1. When NICo briefly displays `Press p key to override boot procedure`, press
   <kbd>p</kbd> several times.
1. Select **iPXE Debug Shell** from the menu:

   ```text
                                   NICo

   Boot according to NICo workflow
   Print information about this machine
   Boot to local drive
   Retry boot
   iPXE Debug Shell
   Reboot System
   ```

## Configure the Network

At the `nico>` prompt, request an address through DHCP:

```text
nico> dhcp
```

A successful request configures the DPU network interface and returns to the
prompt:

```text
Type "exit" to return to menu
nico> dhcp
Configuring (net0 xx:xx:xx:xx:xx:xx)....... ok
nico>
```

## Boot Linux Scout

Replace `<static-pxe-url>` with the static PXE endpoint that is reachable from
the managed host. The standard site-local endpoint is described in
[IP and Network Configuration](../../provisioning/ip-and-network-configuration.md).

For an x86-64 managed host, run:

```text
nico> chain <static-pxe-url>/public/blobs/internal/x86_64/scout.efi console=tty0 console=ttyS1,115200 pci=realloc=off iommu=off cli_cmd=auto-detect
```

For an Arm managed host, including an NVIDIA GB200 or GB300 system, run:

```text
nico> chain <static-pxe-url>/public/blobs/internal/aarch64/scout.efi console=tty0 console=ttyAMA0,115200 pci=realloc=off iommu=off cli_cmd=auto-detect
```

The managed host boots into Linux Scout. If it does not, verify that DHCP
completed, the static PXE endpoint is reachable, and the selected artifact
matches the CPU architecture.
