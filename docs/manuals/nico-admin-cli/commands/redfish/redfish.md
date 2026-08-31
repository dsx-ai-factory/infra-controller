# `nico-admin-cli redfish`

*[Hardware commands](../../hardware.md) › **redfish***

## NAME

nico-admin-cli-redfish - Redfish BMC actions

## SYNOPSIS

**nico-admin-cli redfish** \[**--extended**\] \<**--address**\>
\[**--username**\] \[**--password**\] \[**--sort-by**\]
\[**-h**\|**--help**\] \<*subcommands*\>

## DESCRIPTION

Redfish BMC actions

## OPTIONS

**--extended**  
Extended result output.

This is used by measured boot, where basic output contains just what you
probably care about, and "extended" output also dumps out all the
internal UUIDs that are used to associate instances.

**--address** *\<ADDRESS\>*  
IP:port of machine BMC. Port is optional and defaults to 443

**--username** *\<USERNAME\>*  
Username for machine BMC

**--password** *\<PASSWORD\>*  
Password for machine BMC

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
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword get-power-state
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword on
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword force-off
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword boot-pxe
nico-admin-cli redfish --address 192.0.2.10 --username admin --password mypassword update-firmware-multipart --filename ./host-fw.bin
```

## Subcommands

| Subcommand | Description |
|---|---|
| [`bios-attrs`](./redfish-bios-attrs.md) | List BIOS attributes |
| [`boot-hdd`](./redfish-boot-hdd.md) | Set hard drive first in boot order |
| [`boot-pxe`](./redfish-boot-pxe.md) | Set PXE first in boot order |
| [`boot-uefi-http`](./redfish-boot-uefi-http.md) | Set Boot order to UEFI Http First |
| [`boot-once-hdd`](./redfish-boot-once-hdd.md) | On next boot only, boot from hard drive |
| [`boot-once-pxe`](./redfish-boot-once-pxe.md) | On next boot only, boot from PXE |
| [`boot-once-uefi-http`](./redfish-boot-once-uefi-http.md) | Boot rom UEFI HTTP Once |
| [`clear-pending`](./redfish-clear-pending.md) | Delete all pending jobs |
| [`create-bmc-user`](./redfish-create-bmc-user.md) | Create new BMC user |
| [`delete-bmc-user`](./redfish-delete-bmc-user.md) | Create new BMC user |
| [`machine-setup`](./redfish-machine-setup.md) | Setup host for use |
| [`machine-setup-status`](./redfish-machine-setup-status.md) | Is everything MachineSetup does already done? What's missing? |
| [`set-forge-password-policy`](./redfish-set-forge-password-policy.md) | Set our password policy |
| [`get-boot-option`](./redfish-get-boot-option.md) | List one or all BIOS boot options |
| [`get-power-state`](./redfish-get-power-state.md) | Is this thing on? |
| [`lockdown-disable`](./redfish-lockdown-disable.md) | Disable BMC/BIOS lockdown |
| [`lockdown-enable`](./redfish-lockdown-enable.md) | Enable BMC/BIOS lockdown |
| [`lockdown-status`](./redfish-lockdown-status.md) | Display status of BMC/BIOS lockdown |
| [`force-off`](./redfish-force-off.md) | Force turn machine off |
| [`force-restart`](./redfish-force-restart.md) | Force restart. This is equivalent to pressing the reset button on the front panel. |
| [`graceful-restart`](./redfish-graceful-restart.md) | Graceful restart. Asks the OS to restart via ACPI |
| [`graceful-shutdown`](./redfish-graceful-shutdown.md) | Graceful host shutdown |
| [`ac-power-cycle`](./redfish-ac-power-cycle.md) | AC power cycle |
| [`on`](./redfish-on.md) | Power on a machine |
| [`pcie-devices`](./redfish-pcie-devices.md) | List PCIe devices |
| [`local-storage`](./redfish-local-storage.md) | List Direct Attached drives |
| [`pending`](./redfish-pending.md) | List pending operations |
| [`power-metrics`](./redfish-power-metrics.md) | Display power metrics (voltages, power supplies, etc) |
| [`serial-enable`](./redfish-serial-enable.md) | Enable serial console |
| [`serial-status`](./redfish-serial-status.md) | Serial console status |
| [`thermal-metrics`](./redfish-thermal-metrics.md) | Display thermal metrics (fans and temperatures) |
| [`tpm-reset`](./redfish-tpm-reset.md) | Clear Trusted Platform Module (TPM) |
| [`bmc-reset-to-defaults`](./redfish-bmc-reset-to-defaults.md) | Reset BMC to factory defaults |
| [`bmc-reset`](./redfish-bmc-reset.md) | Reboot the BMC itself |
| [`get-secure-boot`](./redfish-get-secure-boot.md) | Get Secure boot status |
| [`disable-secure-boot`](./redfish-disable-secure-boot.md) | Disable Secure Boot |
| [`get-chassis-all`](./redfish-get-chassis-all.md) | List Chassis |
| [`get-chassis`](./redfish-get-chassis.md) | List Chassis Subsystem |
| [`get-bmc-ethernet-interfaces`](./redfish-get-bmc-ethernet-interfaces.md) | Show BMC's Ethernet interface information |
| [`get-system-ethernet-interfaces`](./redfish-get-system-ethernet-interfaces.md) | Show System Ethernet interface information |
| [`get-bmc-accounts`](./redfish-get-bmc-accounts.md) | List of existing BMC accounts |
| [`change-bmc-username`](./redfish-change-bmc-username.md) | Rename an account |
| [`change-bmc-password`](./redfish-change-bmc-password.md) | Change password for a BMC user |
| [`change-uefi-password`](./redfish-change-uefi-password.md) | Change UEFI password |
| [`dpu`](./redfish-dpu.md) | DPU specific operations |
| [`get-manager`](./redfish-get-manager.md) | Get information about the managers |
| [`update-firmware-multipart`](./redfish-update-firmware-multipart.md) | Update host firmware |
| [`get-task`](./redfish-get-task.md) | Get detailed info on a Redfish task |
| [`get-tasks`](./redfish-get-tasks.md) | Get a list of Redfish tasks |
| [`clear-uefi-password`](./redfish-clear-uefi-password.md) | Clear UEFI password |
| [`is-ipmi-over-lan-enabled`](./redfish-is-ipmi-over-lan-enabled.md) | Is IPMI enabled over LAN |
| [`enable-ipmi-over-lan`](./redfish-enable-ipmi-over-lan.md) | Enable IPMI over LAN |
| [`disable-ipmi-over-lan`](./redfish-disable-ipmi-over-lan.md) | Disable IPMI over LAN |
| [`get-base-mac-address`](./redfish-get-base-mac-address.md) | Get Base Mac Address (DPU only) |
| [`clear-nvram`](./redfish-clear-nvram.md) | Clear Nvram (Viking only) |
| [`set-bios`](./redfish-set-bios.md) | Set BIOS options |
| [`reset-bios`](./redfish-reset-bios.md) | Reset BIOS settings to factory defaults. Returns once the BMC accepts the reset request. A system restart is required for the settings to take effect |
| [`get-nic-mode`](./redfish-get-nic-mode.md) | Get DPU mode |
| [`is-infinite-boot-enabled`](./redfish-is-infinite-boot-enabled.md) | Is infinite boot enable |
| [`enable-infinite-boot`](./redfish-enable-infinite-boot.md) | Enable infinite boot |
| [`set-nic-mode`](./redfish-set-nic-mode.md) | Set NIC mode (host networking via the NIC) |
| [`set-dpu-mode`](./redfish-set-dpu-mode.md) | Set DPU mode (host networking via the DPU) |
| [`chassis-reset-card1-powercycle`](./redfish-chassis-reset-card1-powercycle.md) | Power cycle a machine |
| [`set-boot-order-dpu-first`](./redfish-set-boot-order-dpu-first.md) | Set the DPU as the first boot target Set the boot order so the DPU boots first |
| [`get-host-rshim`](./redfish-get-host-rshim.md) | Get status of the rshim process on DPU |
| [`enable-host-rshim`](./redfish-enable-host-rshim.md) | Enable rshim on DPU |
| [`disable-host-rshim`](./redfish-disable-host-rshim.md) | Disable rshim on dpu |
| [`get-boss-controller`](./redfish-get-boss-controller.md) | Get the Boss Controller |
| [`decommission-controller`](./redfish-decommission-controller.md) | Decommission a storage controller |
| [`create-volume`](./redfish-create-volume.md) | Create a storage volume |
| [`is-boot-order-setup`](./redfish-is-boot-order-setup.md) | Check if boot order is set correctly Check whether the DPU-first boot order is already configured |

---

**See also:** [Hardware commands](../../hardware.md) · [CLI reference index](../../index.md)
