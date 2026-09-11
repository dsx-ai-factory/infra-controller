# Lenovo SR675 V3 Stuck Polling BIOS Setup

Use this playbook when a Lenovo ThinkSystem SR675 V3 OVX remains in
`Assigned/HostPlatformConfiguration/PollingBiosSetup` and an instance cannot
finish termination.

## Symptoms

- An instance remains in `Terminating`.
- Managed-host history advances through `ConfigureBios`, then stops at
  `PollingBiosSetup`.
- Manual BIOS changes fail or appear to succeed without changing the effective
  configuration.

Inspect the affected instance and host:

```bash
nico-admin-cli instance show
nico-admin-cli -f json machine show <machine-id> \
  | jq -r '.events[] | "\(.time) \(.event)"' \
  | tail -10
```

## Cause

NICo is waiting for the BMC to apply the requested BIOS settings and boot
order. On an affected server, the BMC can silently retain inconsistent UEFI
state, so NICo never observes the desired settings.

Clearing CMOS resets that UEFI state. Running machine setup afterward reapplies
the settings and boot interface that NICo requires.

<Warning>

Clearing CMOS resets firmware configuration. Record any site-specific BIOS
settings and confirm the maintenance window before continuing.

</Warning>

## Clear CMOS

1. Set the BMC connection variables. Obtain credentials from your approved
   credential store.

   ```bash
   export BMC_IP=<bmc-ip-address>
   export BMC_USER=<bmc-username>
   export BMC_PASS=<bmc-password>
   ```

1. Force the server off:

   ```bash
   curl --fail --silent --show-error --insecure \
     --user "${BMC_USER}:${BMC_PASS}" \
     --header 'Content-Type: application/json' \
     --data '{"ResetType":"ForceOff"}' \
     --request POST \
     "https://${BMC_IP}/redfish/v1/Systems/1/Actions/ComputerSystem.Reset"
   ```

1. Confirm that the server is off:

   ```bash
   curl --fail --silent --show-error --insecure \
     --user "${BMC_USER}:${BMC_PASS}" \
     "https://${BMC_IP}/redfish/v1/Systems/1" \
     | jq -r '.PowerState'
   ```

   Continue only when the command returns `Off`.

1. Invoke the Lenovo CMOS-clear action. Replace `<uefi-password>` with the
   existing UEFI administrator password:

   ```bash
   curl --fail --silent --show-error --insecure \
     --user "${BMC_USER}:${BMC_PASS}" \
     --header 'Content-Type: application/json' \
     --data '{"UefiAdminPassword":"<uefi-password>"}' \
     --request POST \
     "https://${BMC_IP}/redfish/v1/Systems/1/Actions/Oem/LenovoComputerSystem.RemoteClearCMOS"
   ```

1. Power the server on:

   ```bash
   curl --fail --silent --show-error --insecure \
     --user "${BMC_USER}:${BMC_PASS}" \
     --header 'Content-Type: application/json' \
     --data '{"ResetType":"On"}' \
     --request POST \
     "https://${BMC_IP}/redfish/v1/Systems/1/Actions/ComputerSystem.Reset"
   ```

## Reapply Machine Setup

1. Get the primary boot-interface MAC address:

   ```bash
   MAC_ADDRESS=$(nico-admin-cli -f json machine show <machine-id> \
     | jq -r '.interfaces[] | select(.primary_interface == true) | .mac_address')
   ```

1. Run machine setup after the server begins booting:

   ```bash
   nico-admin-cli redfish \
     --address "${BMC_IP}" \
     --username "${BMC_USER}" \
     --password "${BMC_PASS}" \
     machine-setup --boot-interface-mac "${MAC_ADDRESS}"
   ```

1. Check which setup operations remain:

   ```bash
   nico-admin-cli redfish \
     --address "${BMC_IP}" \
     --username "${BMC_USER}" \
     --password "${BMC_PASS}" \
     machine-setup-status --boot-interface-mac "${MAC_ADDRESS}"
   ```

   If machine setup ran too early in the boot process, wait for the BMC to
   become ready and rerun it.

## Verify Recovery

1. Confirm that machine history advances past `PollingBiosSetup`:

   ```bash
   nico-admin-cli -f json machine show <machine-id> \
     | jq -r '.events[] | "\(.time) \(.event)"' \
     | tail -10
   ```

1. Confirm that the host leaves
   `Assigned/HostPlatformConfiguration/PollingBiosSetup`.
1. Confirm that the instance completes termination.

For general state inspection, refer to
[State Machine Debugging](state_machine_debugging.md).
