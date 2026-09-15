# Adding New Machines to an Existing Site

This guide is intended to cover some of the basic things you should check to get a machine into a basic state where it can be discovered by NICo auto-ingestion.

Some of the configuration items that should be considered which could potentially cause issues:

1. Host BMC Password Requirements
2. Updating the Host BMC and UEFI Firmware (Not covered in this document at this time)
3. DPU BMC Password Requirements
4. Updating DPU BMC Firmware
5. DPU ARM OS Check Secure Boot status

## Host BMC Password Requirements

<Note>
New servers should be using the default username for the server type. For example, `USERID` for Lenovo, `admin` for NVIDIA/Vikings, and `root` for Dell.
</Note>

You should check both the expected machines DB and the credential store (the site vault pod data store by default, or the Postgres `secrets` journal when configured) for any existing data. If entries exist in both expected machines and the credential store, you should consider the password in the credential store as the password that should be used. With the Postgres store, list the BMC entries in the `nico_system_nico` database with `SELECT path, seq, kek_id, created_at FROM secrets WHERE path LIKE 'machines/bmc/%' ORDER BY seq DESC;`; the values are encrypted, so the query shows which BMC paths have an entry, not the passwords.

### Check Host BMC exists in Expected Machines DB

If there is an existing data in expected machines for the machine, you can either update the password
in expected machines or change the password on the Host BMC to match.

1. Use `nico-admin-cli` to check if there is an existing entry for the host BMC:

    ```bash
    nico-admin-cli expected-machine show |grep <Host BMC IP Address|Host BMC MAC Address>
    ```

2. If an entry exists for the machine, display the details using `nico-admin-cli`:

    ```bash
    nico-admin-cli expected-machine show <Host BMC MAC address>
    ```

3. To update an existing expected machines data:

    ```bash
    nico-admin-cli expected-machine add --bmc-mac-address <BMC MAC Address> --bmc-username <BMC Username> --bmc-password <BMC Password --chassis-serial-number <Chassis Serial Number>
    ```

   > **Note**: If you only need to update the BMC password, you just need to supply the BMC MAC Address and BMC Password

4. To add a new machine to the expected machines DB:

    ```bash
    nico-admin-cli expected-machine update --bmc-mac-address <BMC_MAC_ADDRESS> <--bmc-username <BMC_USERNAME> --bmc-password <BMC_PASSWORD> --chassis-serial-number <CHASSIS_SERIAL_NUMBER>
    ```

### Checking site vault data

To check if the Host BMC has currently any passwords in vault on a site (default Vault backend):

1. Connect to the Kubernetes environment for the site you are working on
2. Retrieve the decoded vault secret for the site:

    ```bash
    kubectl get secret -n nico-system nico-vault-token -oyaml | yq '.data.token' | base64 -d ; echo
    ```

3. Connect to the vault pod for the site and paste in the decoded vault secret at the Token prompt:

    ```bash
    kubectl --namespace vault exec -it vault-0 -- /bin/sh
    vault login --tls-skip-verify
    Token (will be hidden):
    ```

4. List the secrets in vault:

    ```bash
    vault secrets list --tls-skip-verify
    ```

5. Look for the site BMC:

    ```bash
    vault kv list --tls-skip-verify secrets/machines/bmc/ |grep <Host BMC MAC Address>
    ```

6. Get the current credentials set for the host bmc if they exist:

    ```bash
    vault kv get --tls-skip-verify secrets/machines/bmc/<BMC MAC Address>/root
    ```

   Ensure these credentials match the credentials currently set on the host BMC. It is easier to just update the Host BMC to match vault rather than attempting to update the secret in vault.

## DPU BMC Password Requirements

For a new/undiscovered DPU BMC, ensure that it is set to the default BMC username/password

### Resetting DPU BMC password to default - From DPU BMC

To reset to factory defaults from the DPU BMC:

1. Log into the DPU BMC.
2. Run the following command to reset to factory defaults:

    ```bash
    ipmitool raw 0x32 0x66
    ```

3. Reboot the DPU BMC:

    ```bash
    reboot
    ```

### Resetting DPU BMC password to default - From DPU ARM OS

If you don't know the BMC password, but have access to the DPU ARM OS, you can reset to defaults as follows:

1. Log into the DPU ARM OS
2. Switch to root:

    ```bash
    sudo -i
    ```

3. Restore DPU BMC defaults:

    ```bash
    ipmitool raw 0x32 0x66
    ```

4. Restart DPU BMC:

    ```bash
    ipmitool mc reset cold
    ```

## Updating DPU firmware

### Determine the DPU model

Log on to the DPU ARM OS and attempt to run the following command:

```bash
sudo mlxfwmanager --query -d /dev/mst/*_pciconf0
```

For Bluefield 2 DPUs you should expect the output similar to the following:

```text
Querying Mellanox devices firmware ...

Device #1:
----------

  Device Type:      BlueField2
  Part Number:      MBF2H536C-CECO_Ax_Bx
  Description:      BlueField-2 P-Series DPU 100GbE Dual-Port QSFP56; integrated BMC; PCIe Gen4 x16; Secure Boot Enabled; Crypto Enabled; 32GB on-board DDR; 1GbE OOB management; FHHL
  PSID:             MT_0000000768
  PCI Device Name:  /dev/mst/mt41686_pciconf0
  Base GUID:        a088c20300ea8240
  Base MAC:         a088c2ea8240
  Versions:         Current        Available
     FW             24.40.1000     N/A
     FW (Running)   24.35.2000     N/A
     PXE            3.6.0805       N/A
     UEFI           14.28.0016     N/A
     UEFI Virtio blk   22.4.0010      N/A
     UEFI Virtio net   21.4.0010      N/A

```

For Bluefield 3 DPUs you should expect the output similar to the following:

```text
Querying Mellanox devices firmware ...

Device #1:
----------

Device Type:      BlueField3
  Part Number:      900-9D3B6-00CV-A_Ax
  Description:      NVIDIA BlueField-3 B3220 P-Series FHHL DPU; 200GbE (default mode) / NDR200 IB; Dual-port QSFP112; PCIe Gen5.0 x16 with x16 PCIe extension option; 16 Arm cores; 32GB on-board DDR; integrated BMC; Crypto Enabled
  PSID:             MT_0000000884
  PCI Device Name:  /dev/mst/mt41692_pciconf0
  Base MAC:         a088c232137a
  Versions:         Current        Available
     FW             32.41.1000     N/A
     PXE            3.7.0400       N/A
     UEFI           14.34.0012     N/A
     UEFI Virtio blk   22.4.0013      N/A
     UEFI Virtio net   21.4.0013      N/A

  Status:           No matching image found
```

### Checking Bluefield Firmware Versions

To check the current Bluefield firmware versions installed on a DPU:

1. Log into the staging server for the site
2. Set up IP, password and token environment variables:

    ```bash
    export DPUBMCIP=<DPU BMC IP>
    export BMCPASS=<BMC Password>
    export BMCTOKEN=`curl -k -H "Content-Type: application/json" -X POST https://$DPUBMCIP/login -d "{\"username\": \"root\", \"password\": \"$BMCPASS\"}" | grep token | awk '{print $2;}' | tr -d '"'`
    ```

3. Check the current DPU BMC Firmware Versions:

   Bluefield 2 DPUs:

    ```bash
    curl -k -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/UpdateService/FirmwareInventory

    # Use the Firmware ID from the first command to complete the firmware ID needed for the following command:
    curl -k -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/UpdateService/FirmwareInventory/<firmware_id>_BMC_Firmware | jq -r ' .Version'
    ```

   Bluefield 3 DPUs:

    ```bash
    curl -ks -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware | jq -r ' .Version'
    ```

### Updating the Bluefield Firmware Versions

***Note:*** If discovery is failing due to the firmware revision being too low, confirm with the NICo dev team what version you should update to before proceeding

DPU Firmware versions can be downloaded from the following locations:

- BF2: [BF2 BMC Firmware release](https://confluence.nvidia.com/display/SW/BF2+BMC+Firmware+release)
- BF3: [BF3 BMC Firmware release](https://confluence.nvidia.com/display/SW/BF3+BMC+Firmware+release)

For the examples below, we are installing FW version 24.01-5, but confirm this with the development team for your specific install before proceeding

1. Download the relevant packages for your DPU type:

   Bluefield 2:

    ```bash
    wget https://urm.nvidia.com/artifactory/sw-bmc-generic-local/BF2/BF2BMC-24.01-5/OPN/bf2-bmc-ota-24.01-5-opn.tar
    ```

   Bluefield 3:

    ```bash
    wget https://urm.nvidia.com/artifactory/sw-bmc-generic-local/BF3/BF3BMC-24.01-5/OPN/bf3-bmc-24.01-5_opn.fwpkg
    ```

2. Copy the firmware package to the staging server for the site
3. Set up IP, password and token environment variables:

    ```bash
    export DPUBMCIP=<DPU BMC IP>
    export BMCPASS=<BMC Password>
    export BMCTOKEN=`curl -k -H "Content-Type: application/json" -X POST https://$DPUBMCIP/login -d "{\"username\": \"root\", \"password\": \"$BMCPASS\"}" | grep token | awk '{print $2;}' | tr -d '"'`
    ```

4. Initiate the DPU BMC FW Upgrade:

   Bluefield 2:

    ```bash
    curl -k -H "X-Auth-Token: $BMCTOKEN" -H "Content-Type: application/octet-stream" -X POST -T bf2-bmc-ota-24.01-5-opn.tar https://$DPUBMCIP/redfish/v1/UpdateService/update
    ```

   Bluefield 3:

    ```bash
    curl -k -H "X-Auth-Token: $BMCTOKEN" -H "Content-Type: application/octet-stream" -X POST -T bf3-bmc-24.01-5_opn.fwpkg https://$DPUBMCIP/redfish/v1/UpdateService/update
    ```

5. Monitor the firmware update progress:

    ```bash
    # List the running tasks:
    curl -ks -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/TaskService/Tasks
    {
      "@odata.id": "/redfish/v1/TaskService/Tasks",
      "@odata.type": "#TaskCollection.TaskCollection",
      "Members": [
        {
          "@odata.id": "/redfish/v1/TaskService/Tasks/0"
        }
      ],
      "Members@odata.count": 1,
      "Name": "Task Collection"
    }

    # Display the current progress
    curl -ks -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/TaskService/Tasks/0 | jq -r ' .PercentComplete'
    30
    ```

6. Once the progress has reached 100% complete, initiate a reboot of the BMC:

    ```bash
    curl -k -H "X-Auth-Token: $BMCTOKEN" -H "Content-Type: application/json" -X POST -d '{"ResetType": "GracefulRestart"}' https://$DPUBMCIP/redfish/v1/Managers/Bluefield_BMC/Actions/Manager.Reset
    ```

7. Once the DPU BMC has rebooted, retrieve a new BMC Token and check the installed firmware version:

   Bluefield 2:

    ```bash
    export BMCTOKEN=`curl -k -H "Content-Type: application/json" -X POST https://$DPUBMCIP/login -d "{\"username\": \"root\", \"password\": \"$BMCPASS\"}" | grep token | awk '{print $2;}' | tr -d '"'`

    curl -k -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/UpdateService/FirmwareInventory
    # Use the Firmware ID from the first command to complete the firmware ID needed for the following command:
    curl -k -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/UpdateService/FirmwareInventory/<firmware_id>_BMC_Firmware | jq -r ' .Version'

    ```

   Bluefield 3:

    ```bash
    export BMCTOKEN=`curl -k -H "Content-Type: application/json" -X POST https://$DPUBMCIP/login -d "{\"username\": \"root\", \"password\": \"$BMCPASS\"}" | grep token | awk '{print $2;}' | tr -d '"'`

    curl -ks -H "X-Auth-Token: $BMCTOKEN" -X GET https://$DPUBMCIP/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware | jq -r ' .Version'
    ```

## DPU ARM OS: Checking Secure Boot Status

To successfully boot from the NICo BFB image, the DPU ARM OS needs to have Secure Boot disabled and configured for HTTP PXE boot.

### Check current secure boot settings

1. Log in to the staging server for the site
2. Set up the DPU IP, password environment variables:

    ```bash
    export DPUBMCIP='BMC IP'
    export BMCPASS='BMC password'
    ```

3. Check the current Secure Boot settings:

    ```bash
    curl -k -u root:"$BMCPASS" -X  GET https://$DPUBMCIP/redfish/v1/Systems/Bluefield/SecureBoot
    ```

   ***Note:*** If you do not see the `SecureBootCurrentBoot` option listed, you should install DOCA version 2.5.0

   If you see the following output, secure boot is enabled and it needs to be disabled:

    ```json
    {
      "@odata.id": "/redfish/v1/Systems/Bluefield/SecureBoot",
      "@odata.type": "#SecureBoot.v1_1_0.SecureBoot",
      "Description": "The UEFI Secure Boot associated with this system.",
      "Id": "SecureBoot",
      "Name": "UEFI Secure Boot",
      "SecureBootCurrentBoot": "Enabled",
      "SecureBootDatabases": {
        "@odata.id": "/redfish/v1/Systems/Bluefield/SecureBoot/SecureBootDatabases"
      },
      "SecureBootEnable": true,
      "SecureBootMode": "UserMode"
    }
    ```

   If you see `"SecureBootCurrentBoot": "Disabled",` no action is required. You should attempt to boot the DPU ARM OS over the network:

    ```json
    {
      "@odata.id": "/redfish/v1/Systems/Bluefield/SecureBoot",
      "@odata.type": "#SecureBoot.v1_1_0.SecureBoot",
      "Description": "The UEFI Secure Boot associated with this system.",
      "Id": "SecureBoot",
      "Name": "UEFI Secure Boot",
      "SecureBootCurrentBoot": "Disabled",
      "SecureBootDatabases": {
        "@odata.id": "/redfish/v1/Systems/Bluefield/SecureBoot/SecureBootDatabases"
      },
      "SecureBootEnable": true,
      "SecureBootMode": "UserMode"
    }
    ```

### Disable Secure Boot

To disable Secure Boot if it is enabled:

1. Run the command to disable Secure Boot:

    ```bash
    curl -k -u root:"$BMCPASS" -X  PATCH -H 'Content-Type: application/json' https://$DPUBMCIP/redfish/v1/Systems/Bluefield/SecureBoot -d '{"SecureBootEnable":false}'
    ```

2. Restart the DPU ARM OS:

    ```bash
    curl -k -u root:"$BMCPASS" -X POST -H 'Content-Type: application/json' https://$DPUBMCIP/redfish/v1/Systems/Bluefield/Actions/ComputerSystem.Reset -d '{"ResetType" : "GracefulRestart"}'
    ```

3. Wait for the DPU ARM OS to boot and check if Secure Boot is enabled now:

    ```bash
    curl -k -u root:"$BMCPASS" -X  GET https://$DPUBMCIP/redfish/v1/Systems/Bluefield/SecureBoot
    ```

   ***Note:*** You may need to run this step several times to disable secure boot. It may take up to 3 cycles of this for the setting to stick

If the "SecureBootCurrentBoot" setting is not shown, attempt to install DOCA 2.5.0:

1. Download the BFB image on the staging server:

    ```bash
    mkdir DOCA
    cd DOCA
    wget https://image.azure.nvmetal.net/mirror/nico/DOCA_2.5.0_BSP_4.5.0_Ubuntu_22.04-1.23-10.prod.bfb --no-check-certificate
    ```

2. Install the BFB image to the DPU ARM OS via the DPU BMC from the server with the BFB image:

    ```bash
    export DPUBMCIP='BMC IP'
    export BMCPASS='BMC password'
    sshpass -p $BMCPASS scp -o StrictHostKeyChecking=no DOCA_2.5.0_BSP_4.5.0_Ubuntu_22.04-1.23-10.prod.bfb root@$DPUBMCIP:/dev/rshim0/boot
    ```

3. Log on to the DPU BMC and reboot the DPU ARM OS:

    ```bash
    echo SW_RESET 1 > /dev/rshim0/misc
    ```

4. After the DPU ARM OS boots, log into the DPU ARM OS using the default password
5. Switch to root and set the default username password back to the default
6. Ensure that the DOCA firmware is up to date:

    ```bash
    sudo -i
    bfvcheck
    /opt/mellanox/mlnx-fw-updater/mlnx_fw_updater.pl
    ```

7. Check that the DPU ARM OS is configured for HTTPs boot. Log into the DPU ARM OS and switch to root,
8. List the current boot order:

    ```bash
    efibootmgr
    ```

9. If the boot order is set to something similar the following, no action is needed and you should reboot the DPU ARM OS:

    ```text
    BootCurrent: 0040
    Timeout: 3 seconds
    BootOrder: 0000,0040,0001,0002,0003
    Boot0000* NET-OOB-IPV4-HTTP
    Boot0001* NET-OOB.4040-IPV4
    Boot0002* UiApp
    Boot0003* EFI Internal Shell
    Boot0040* ubuntu0
    ```

10. To set the correct boot order, create the /etc/bf.cfg file with the following contents:

    ```bash
    echo "BOOT0=NET-OOB-IPV4-HTTP
    BOOT1=DISK
    BOOT2=NET-OOB.4040-IPV4" >> /etc/bf.cfg
    ```

11. Run the bfcfg command to update the boot order:

    ```bash
    bfcfg
    ```

12. Verify that the boot order is now set to NET-OOB-IPV4-HTTP as default:

    ```bash
    efibootmgr
    ```

13. Reboot the DPU ARM OS from the RSHIM console and monitor the reboot/provisioning process

    ***Note:*** If you see an error similar to the following during PXE boot, verify that Secure Boot is disabled correctly:

    ```text
    EFI stub: Booting Linux Kernel...
    EFI stub: ERROR: FIRMWARE BUG: kernel image not aligned on 64k boundary
    EFI stub: UEFI Secure Boot is enabled.
    EFI stub: Using DTB from configuration table
    ```
