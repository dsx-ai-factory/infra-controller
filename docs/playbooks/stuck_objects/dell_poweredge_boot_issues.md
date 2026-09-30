# Restore a Missing DPU Boot Device on Dell PowerEdge

Use this playbook when a Dell PowerEdge server with a DPU no longer lists its
UEFI HTTP device. This issue has been observed on PowerEdge R750, R760, and
XE9680 systems.

## Symptoms and Impact

- The server repeatedly selects boot devices other than the DPU.
- A terminating instance boots the tenant operating system from local storage
  instead of booting through the DPU.
- Re-ingestion remains in `HostInitializing/SetBootOrder`.
- Redfish does not return an `HTTP Device` boot option for the DPU.

Without the DPU boot option, NICo cannot restore the expected boot order.
Provisioning, termination, wiping, and re-ingestion can remain blocked.

## Confirm That the Boot Device Is Missing

1. Set the BMC connection variables. Obtain credentials from your approved
   credential store.

   ```bash
   export BMC_IP=<bmc-ip-address>
   export BMC_USER=<bmc-username>
   export BMC_PASS=<bmc-password>
   ```

1. List the boot options:

   ```bash
   for device in $(curl --silent --insecure \
     --user "${BMC_USER}:${BMC_PASS}" \
     "https://${BMC_IP}/redfish/v1/Systems/System.Embedded.1" \
     | jq -r '.Boot.BootOrder[]'); do
     curl --silent --insecure \
       --user "${BMC_USER}:${BMC_PASS}" \
       "https://${BMC_IP}/redfish/v1/Systems/System.Embedded.1/BootOptions/${device}" \
       | jq -r '"\(.DisplayName)\t\(.BootOptionEnabled)"'
   done
   ```

1. Check for an enabled `HTTP Device` entry. A healthy system includes output
   similar to:

   ```text
   BOSS in SL 16: ubuntu                               false
   HTTP Device 1: NIC in Slot 40 Port 1 Partition 1   true
   ```

If no HTTP device appears, continue with this procedure.

## Cause

A PCIe training failure can prevent UEFI from detecting the DPU during boot.
UEFI then removes the corresponding device from `BootOptions`. iDRAC System
Lockdown prevents NICo from restoring the boot configuration until lockdown is
disabled and the server is rebooted.

## Disable System Lockdown

1. Check the current state:

   ```bash
   nico-admin-cli bmc-machine lockdown-status --machine <machine-id>
   ```

1. Disable lockdown and reboot the host so the change takes effect:

   ```bash
   nico-admin-cli bmc-machine lockdown \
     --machine <machine-id> --disable --reboot
   ```

You can also disable lockdown from the iDRAC dashboard by selecting
**More Actions > Turn off the System Lockdown Mode**, then power cycling the
host.

## Restore the DPU Boot Device

Try NICo machine setup first:

1. Get the primary DPU interface MAC address:

   ```bash
   MAC_ADDRESS=$(nico-admin-cli -f json machine show <machine-id> \
     | jq -r '.interfaces[] | select(.primary_interface == true) | .mac_address')
   ```

1. Run machine setup against the BMC:

   ```bash
   nico-admin-cli redfish \
     --address "${BMC_IP}" \
     --username "${BMC_USER}" \
     --password "${BMC_PASS}" \
     machine-setup --boot-interface-mac "${MAC_ADDRESS}"
   ```

1. Force-restart the host to apply pending UEFI changes:

   ```bash
   nico-admin-cli redfish \
     --address "${BMC_IP}" \
     --username "${BMC_USER}" \
     --password "${BMC_PASS}" \
     force-restart
   ```

If machine setup cannot restore the device, configure it from the UEFI UI:

1. Open the iDRAC remote console and set the next boot device to **BIOS Setup**.
1. Cold power cycle the system.
1. At the UEFI password prompt, paste the password using the console's virtual
   clipboard.
1. Go to **System BIOS > Network Settings**, disable the PXE devices, and enable
   **HTTP Device1** under UEFI.
1. In **HTTP Device1 Settings**, select the DPU interface. Confirm the slot
   against the server inventory and cabling. Common mappings are:

   | Model | DPU Interface Example |
   |---|---|
   | PowerEdge R760 | NIC PCI Slot 2 |
   | PowerEdge R750 | NIC in Slot 5 Port 1 |
   | PowerEdge XE9680 | NIC in Slot 40 Port 1 Partition 1 |

1. Save the settings and exit.
1. If the host still selects local storage first, return to
   **System BIOS > Boot Settings > UEFI Boot Settings** and move
   **HTTP Device 1** to the first position.

## Re-enable System Lockdown

<Warning>

Re-enable lockdown after restoring the boot configuration. Leaving lockdown
disabled weakens the intended BMC security posture.

</Warning>

```bash
nico-admin-cli bmc-machine lockdown \
  --machine <machine-id> --enable
```

If the BMC requires a reboot to apply the change, add `--reboot`. From the
iDRAC UI, select **More Actions > Turn on the System Lockdown Mode**.

## Verify Recovery

1. List the Redfish boot options again and confirm that an enabled
   `HTTP Device` entry is present.
1. Confirm that the host boots through the DPU rather than from the tenant OS
   on local storage.
1. If the host was stuck in `HostInitializing/SetBootOrder`, confirm that it
   advances to the next state.
1. Confirm that System Lockdown is enabled:

   ```bash
   nico-admin-cli bmc-machine lockdown-status --machine <machine-id>
   ```

For vendor background, refer to the
[Dell iDRAC System Lockdown overview](https://www.dell.com/support/kbdoc/en-uk/000135182/idrac9-how-to-enable-and-disable-lockdown-mode).
