# Machine Stuck in DPUInitializing/Init During Ingestion

Use this playbook when a predicted host remains in `DPUInitializing/Init`
during ingestion and its NVIDIA BlueField-2 DPU exposes InfiniBand interfaces
instead of the Ethernet interfaces that NICo expects.

## Symptoms

Inspect the machine event history:

```bash
nico-admin-cli -f json machine show <host-machine-id> \
  | jq -r '.events[] | "\(.time) \(.event)"' \
  | tail -20
```

The most recent event remains similar to this output:

```json
{
  "state": "dpuinit",
  "dpu_states": {
    "states": {
      "<dpu-machine-id>": {
        "dpustate": "init"
      }
    }
  }
}
```

Set `BMC_IP` to the DPU BMC address and set `BMC_USER` and `BMC_PASS` to
credentials for that BMC. Query its Redfish network-port collection:

```bash
curl --silent --show-error --insecure \
  --user "${BMC_USER}:${BMC_PASS}" \
  --header "Content-Type: application/json" \
  "https://${BMC_IP}/redfish/v1/Chassis/Card1/NetworkAdapters/NvidiaNetworkAdapter/Ports" \
  | jq -r '.Members[] | .["@odata.id"]'
```

On an affected DPU, the member paths end in `ib0` and `ib1` instead of `eth0`
and `eth1`.

Log in to the DPU Arm OS and inspect its interfaces:

```bash
ip -brief link show
sudo mst status -v
```

The affected configuration has these characteristics:

- `ib0` and `ib1` exist instead of `p0` and `p1`.
- `mst status -v` reports `net-ib0` and `net-ib1`.
- The UEFI configuration reports the following values:

  ```text
  Device Name          Mellanox Network Adapter
  Chip Type            BlueField-2
  Network Link Type    <InfiniBand>
  ```

## Root Cause

Both DPU network ports are configured with the InfiniBand link type. NICo
expects these ports to use Ethernet and cannot advance DPU initialization while
they expose InfiniBand interfaces.

## Change the Network Link Type

<Warning>
A full host power cycle is disruptive. Confirm that the host has no assigned
tenant workload before continuing.
</Warning>

1. Open the DPU console through its BMC.
1. Reboot the DPU Arm OS and press Esc twice while the firmware starts to
   enter UEFI setup.
1. Enter the UEFI password when prompted.
1. Go to **Device Manager > Network Device List**.
1. Select the first network device by its **MAC address**, open
   **Mellanox Network Adapter**, and set **Network Link Type** to **Ethernet**.
   Press Esc and save the change.
1. Repeat the previous step for the second network device.
1. Exit UEFI setup. Use the BMC to power off the host, wait 30 seconds, and
   power it on.

<Note>
A reset does not replace the final power cycle. After changing the link type,
the network devices can disappear from UEFI setup until the host completes a
full power cycle.
</Note>

## Verify Recovery

1. Wait for the DPU to finish booting.
1. Repeat the Redfish query. Confirm that the member paths end in `eth0` and
   `eth1`, not `ib0` and `ib1`.
1. Run `ip -brief link show` and `sudo mst status -v` on the DPU. Confirm that
   `p0` and `p1` are present and that `net-ib0` and `net-ib1` are absent.
1. Inspect the machine event history again:

   ```bash
   nico-admin-cli -f json machine show <host-machine-id> \
     | jq -r '.events[] | "\(.time) \(.event)"'
   ```

   Confirm that the DPU advances beyond `DPUInitializing/Init`.

## Related Playbooks

- [DPU Provisioning Failures](dpu_provisioning_failures.md)
- [Host Ingestion Failures](host_ingestion_failures.md)
- [Network Connectivity Issues](network_connectivity.md)
