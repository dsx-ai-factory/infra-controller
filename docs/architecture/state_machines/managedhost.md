# Managed Host State Diagrams

This document contains the complete Finite State Machine (FSM) that illustrates the lifecycle of NICo managed hosts from discovery through ingestion through instance assignment and management.

## High-Level Overview

The main flow shows the primary states and transitions between them:

```mermaid
stateDiagram-v2
    direction TB

    state "DpuDiscoveringState" as DpuDiscoveringState
    state "DPUInit" as DPUInit
    state "HostInit" as HostInit
    state "HostInit/Discovered" as HostInit_HI_Discovered
    state "BomValidating" as BomValidating
    state "Validation" as Validation
    state "Measuring" as Measuring
    state "Ready" as Ready
    state "Assigned" as Assigned
    state "HostReprovision" as HostReprovision
    state "DPUReprovision" as DPUReprovision
    state "PostAssignedMeasuring" as PostAssignedMeasuring
    state "WaitingForCleanup" as WaitingForCleanup
    state "AnyNotAssignedState" as AnyNotAssignedState
    state "Failed" as Failed
    state "ForceDeletion" as ForceDeletion

    [*] --> DpuDiscoveringState : Site Explorer creates managed host

    DpuDiscoveringState --> DPUInit : All DPU Security Boot Configured / DPU Reboot
    DpuDiscoveringState --> HostInit : No DPU

    DPUInit --> HostInit : DPU Ready

    HostInit --> BomValidating : Host Configured
    HostInit --> HostInit_HI_Discovered : See HostInit diagram
    HostInit_HI_Discovered --> Ready

    BomValidating --> Validation : BOM Valid AND Validation Enabled
    BomValidating --> HostInit_HI_Discovered : BOM Valid AND Validation Disabled

    Validation --> HostInit_HI_Discovered : Validation Complete
    Measuring --> Ready : Measurements Valid

    Ready --> HostInit : BIOS Password Setup Needed
    Ready --> Assigned : Instance Assigned
    Ready --> HostReprovision : Host Reprovision Request
    Ready --> DPUReprovision : DPU Reprovision Request
    Ready --> Measuring : Redo Measurements Request
    Ready --> BomValidating : BOM Validation Request
    Ready --> Validation : Machine Validation Request

    Assigned --> PostAssignedMeasuring : Measurements required
    Assigned --> WaitingForCleanup : Cleanup
    Assigned --> HostReprovision : Host reprovision needed
    Assigned --> DPUReprovision

    HostReprovision --> Ready : Reprovision Complete
    HostReprovision --> Assigned : Reprovision Complete

    DPUReprovision --> Assigned : if came from Assigned
    DPUReprovision --> HostInit_HI_Discovered : if came from Ready
    DPUReprovision --> HostReprovision : Host reprovision requested

    WaitingForCleanup --> BomValidating

    PostAssignedMeasuring --> WaitingForCleanup : Measurements complete

    AnyNotAssignedState --> Failed : Any failure condition
    Failed --> AnyNotAssignedState : Recovery
    Failed --> ForceDeletion : Admin forced deletion
    Failed --> Failed: Unrecoverable condition

    ForceDeletion --> [*] : Force deletion complete
```

### `Failed` recovery semantics

`FailureCause::BiosSetupFailed` is set when BIOS setup retries are exhausted during ingestion or instance deprovisioning. The Failed handler polls `is_bios_setup` and auto-recovers into `SetBootOrder` once the BMC reports success.

## DPU Discovery State Details (DpuDiscoveringState)

Shows the complete DPU discovery and configuration process:

```mermaid
stateDiagram-v2
    state "Initializing" as DD_Initializing
    state if_state_dpu_nodpu <<choice>>
    state "Configuring" as DD_Configuring
    state "EnableRshim" as DD_EnableRshim
    state if_bfb_supported <<choice>>
    state "HostInit/WaitingForPlatformConfiguration" as HostInit_HI_WaitingForPlatformConfiguration
    state "DPUInit/InstallingBFB" as DPUInit_DI_IDO_InstallingBFB

    state "EnableSecureBoot" as DD_EnableSecureBoot {
        state "CheckSecureBootStatus" as DD_SSB_E_CheckSecureBootStatus
        state "CheckSecureBootStatusWait" as DD_SSB_E_CheckSecureBootStatusWait
        state "SetSecureBoot" as DD_SSB_E_SetSecureBoot
        state "RebootDPU" as DD_SSB_E_RebootDPU
        state "RebootDPUWait" as DD_SSB_E_RebootDPUWait
    }

    state "DisableSecureBoot" as DD_DisableSecureBoot {
        state "CheckSecureBootStatus" as DD_SSB_D_CheckSecureBootStatus
        state "CheckSecureBootStatusWait" as DD_SSB_D_CheckSecureBootStatusWait
        state "DisableSecureBoot" as DD_SSB_D_DisableSecureBoot
        state "RebootDPU" as DD_SSB_D_RebootDPU
        state "RebootDPUWait" as DD_SSB_D_RebootDPUWait
    }

    state "SetUefiHttpBoot" as DD_SetUefiHttpBoot
    state "RebootAllDPUS" as DD_RebootAllDPUS
    state Failed
    state "DPUInit/Init" as DPUInit_DI_Init

    [*] --> DD_Initializing : Site Explorer creates managed host

    DD_Initializing --> if_state_dpu_nodpu
    if_state_dpu_nodpu --> DD_Configuring : Assoc DPU
    if_state_dpu_nodpu --> HostInit_HI_WaitingForPlatformConfiguration : No DPU

    DD_Configuring --> DD_EnableRshim
    DD_EnableRshim --> DD_RebootAllDPUS : DPF selected during site exploration
    DD_EnableRshim --> if_bfb_supported : DPF not selected during site exploration

    if_bfb_supported --> DD_SSB_E_CheckSecureBootStatus : BFB install supported
    if_bfb_supported --> DD_SSB_D_CheckSecureBootStatus : BFB install not supported

    DD_SSB_E_CheckSecureBootStatus --> DD_SSB_E_SetSecureBoot : Security boot is disabled
    DD_SSB_E_CheckSecureBootStatus --> DD_SSB_E_CheckSecureBootStatusWait : 2nd reboot
    DD_SSB_E_CheckSecureBootStatusWait --> DD_SSB_E_CheckSecureBootStatus : Wait for DPU (5min timeout)
    DD_SSB_E_CheckSecureBootStatus --> DPUInit_DI_IDO_InstallingBFB : Security boot is enabled
    DD_SSB_E_CheckSecureBootStatus --> DD_SSB_E_RebootDPU : Get status error
    DD_SSB_E_CheckSecureBootStatus --> DD_SSB_E_CheckSecureBootStatus : Missing data count++ retry if count < 10
    DD_SSB_E_CheckSecureBootStatus --> Failed : count >= 10 too many retries
    DD_SSB_E_SetSecureBoot --> DD_SSB_E_RebootDPU
    DD_SSB_E_RebootDPU --> DD_SSB_E_RebootDPU : Reboot again
    DD_SSB_E_RebootDPU --> DD_SSB_E_CheckSecureBootStatus : Two reboots
    DD_SSB_E_RebootDPU --> DD_SSB_E_RebootDPUWait
    DD_SSB_E_RebootDPUWait --> DD_SSB_E_RebootDPU : Wait for DPU (5min timeout)

    DD_SSB_D_CheckSecureBootStatus --> DD_SSB_D_DisableSecureBoot : Security boot is enabled
    DD_SSB_D_CheckSecureBootStatus --> DD_SSB_D_CheckSecureBootStatusWait : 2nd reboot
    DD_SSB_D_CheckSecureBootStatusWait --> DD_SSB_D_CheckSecureBootStatus : Wait for DPU (5min timeout)
    DD_SSB_D_CheckSecureBootStatus --> DD_SetUefiHttpBoot : Security boot is disabled
    DD_SSB_D_CheckSecureBootStatus --> DD_SSB_D_RebootDPU : Get status error
    DD_SSB_D_CheckSecureBootStatus --> Failed : count >= 10 too many retries
    DD_SSB_D_CheckSecureBootStatus --> DD_SSB_D_CheckSecureBootStatus : Missing data count++ retry if count < 10
    DD_SSB_D_DisableSecureBoot --> DD_SSB_D_RebootDPU
    DD_SSB_D_RebootDPU --> DD_SSB_D_RebootDPU : Reboot again
    DD_SSB_D_RebootDPU --> DD_SSB_D_CheckSecureBootStatus : Two reboots
    DD_SSB_D_RebootDPU --> DD_SSB_D_RebootDPUWait
    DD_SSB_D_RebootDPUWait --> DD_SSB_D_RebootDPU : Wait for DPU (5min timeout)

    DD_SetUefiHttpBoot --> DD_RebootAllDPUS
    DD_RebootAllDPUS --> DPUInit_DI_Init
```

## DPU Initialization State Details (DpuInitState)

Shows DPU initialization including BFB installation:

```mermaid
stateDiagram-v2
    state "DpuDiscoveringState/CheckSecureBootStatus" as DpuDiscoveringState_DD_SSB_E_CheckSecureBootStatus
    state "DpuDiscoveringState/RebootAllDPUS" as DpuDiscoveringState_DD_RebootAllDPUS

    state "Init" as DI_Init
    state "InstallDpuOs" as DI_InstallDpuOs {
        state "InstallingBFB" as DI_IDO_InstallingBFB
        state "WaitForInstallComplete" as DI_IDO_WaitForInstallComplete
        state "Completed" as DI_IDO_Completed
        state "InstallationError" as DI_IDO_InstallationError
    }
    note right of DI_IDO_InstallationError : Terminal state - waits indefinitely

    state "WaitingForPlatformPowercycle" as DI_WaitingForPlatformPowercycle {
        state "Off" as DI_W4PP_OFF
        state "On" as DI_W4PP_ON
    }

    state "WaitingForPlatformConfiguration" as DI_WaitingForPlatformConfiguration
    state "WaitingForNetworkConfig" as DI_WaitingForNetworkConfig
    state "DpfStates/Provisioning" as DI_DpfStates_Provisioning
    state "HostInit/EnableIpmiOverLan" as HostInit_HI_EnableIpmiOverLan

    DpuDiscoveringState_DD_SSB_E_CheckSecureBootStatus --> DI_IDO_InstallingBFB : Security boot is enabled
    DpuDiscoveringState_DD_RebootAllDPUS --> DI_DpfStates_Provisioning : DPF selected during site exploration
    DpuDiscoveringState_DD_RebootAllDPUS --> DI_Init : DPF not selected during site exploration

    DI_IDO_InstallingBFB --> DI_IDO_WaitForInstallComplete
    DI_IDO_WaitForInstallComplete --> DI_IDO_WaitForInstallComplete : Task Running/New/Starting (wait more)
    DI_IDO_WaitForInstallComplete --> DI_IDO_Completed : Task completed
    DI_IDO_WaitForInstallComplete --> DI_IDO_InstallationError : Task exception
    DI_IDO_Completed --> DI_Init

    DI_Init --> DI_W4PP_OFF : Restart all DPUs
    DI_W4PP_OFF --> DI_W4PP_OFF : DPUs not synchronized (wait more)
    DI_W4PP_OFF --> DI_W4PP_ON : All DPUs synchronized, power OFF host
    DI_W4PP_ON --> DI_WaitingForPlatformConfiguration : Power ON host
    DI_WaitingForPlatformConfiguration --> DI_WaitingForNetworkConfig : Call machine-setup/uefi-setup and restart DPU Restart DPU to apply BIOS settings
    DI_WaitingForNetworkConfig --> DI_WaitingForNetworkConfig : DPU network not ready (wait more, potentially reboot DPU)
    DI_WaitingForNetworkConfig --> HostInit_HI_EnableIpmiOverLan
```

## Host Initialization State Details (HostInitState)

Shows host initialization including boot order and UEFI setup:

```mermaid
stateDiagram-v2
    state "DpuDiscoveringState/Initializing" as DpuDiscoveringState_DD_Initializing
    state "DpuInitState/WaitingForNetworkConfig" as DpuInitState_DI_WaitingForNetworkConfig
    state "DPUReprovision/RebootHost" as DPUReprovision_DR_RebootHost
    state "Validation/MachineValidating" as Validation_V_MachineValidating

    state "SetBootOrder" as HI_SetBootOrder {
        state hi_sbo_if_zero_dpu <<choice>>
        state "SetBootOrder" as HI_SBO_SetBootOrder
        state "WaitForSetBootOrderJobScheduled" as HI_SBO_WaitForSetBootOrderJobScheduled
        state "RebootHost" as HI_SBO_RebootHost
        state "WaitForSetBootOrderJobCompletion" as HI_SBO_WaitForSetBootOrderJobCompletion
    }

    state hi_attestation_enabled <<choice>>

    state HI_Measuring {
        state "WaitingForMeasurements" as HI_M_WaitingForMeasurements
        state "PendingBundle" as HI_M_PendingBundle
    }

    state "UefiSetup" as HI_UefiSetup {
        state "UnlockHost" as HI_USS_UnlockHost
        state "SetUefiPassword" as HI_USS_SetUefiPassword
        state "WaitForPasswordJobScheduled" as HI_USS_WaitForPasswordJobScheduled
        state "PowercycleHost" as HI_USS_PowercycleHost
        state "WaitForPasswordJobCompletion" as HI_USS_WaitForPasswordJobCompletion
        state "LockdownHost" as HI_USS_LockdownHost
    }

    state "WaitingForLockdown" as HI_WaitingForLockdown {
        state "TimeWaitForDPUDown" as HI_WFL_TimeWaitForDPUDown
        state "WaitForDPUUp" as HI_WFL_WaitForDPUUp
    }

    state "EnableIpmiOverLan" as HI_EnableIpmiOverLan
    state "WaitingForPlatformConfiguration" as HI_WaitingForPlatformConfiguration
    state "WaitingForBiosJob" as HI_WaitingForBiosJob {
        state "HandleBiosJobFailure" as HI_WBJ_HandleBiosJobFailure
    }
    state "PollingBiosSetup" as HI_PollingBiosSetup
    state "WaitingForDiscovery" as HI_WaitingForDiscovery
    state "Discovered" as HI_Discovered
    state "BomValidating/MatchingSku" as BomValidating_BV_MatchingSku
    state Ready

    note left of DpuDiscoveringState_DD_Initializing : In reallity any DpuDiscoveringState can be entry point but effectively only DD_Initializing

    DpuDiscoveringState_DD_Initializing --> HI_WaitingForPlatformConfiguration : No DPU
    DpuInitState_DI_WaitingForNetworkConfig --> HI_EnableIpmiOverLan
    Failed --> HI_WFL_TimeWaitForDPUDown
    Failed --> HI_M_WaitingForMeasurements
    Failed --> HI_SBO_SetBootOrder : BiosSetupFailed AND is_bios_setup ok

    HI_EnableIpmiOverLan --> HI_WaitingForPlatformConfiguration : Enable IPMI over LAN access
    HI_WaitingForPlatformConfiguration --> HI_PollingBiosSetup : Call machine setup/Restart Host
    HI_WaitingForPlatformConfiguration --> HI_WaitingForBiosJob : Dell BIOS job scheduled
    HI_WaitingForBiosJob --> HI_PollingBiosSetup : BIOS job completed
    HI_WaitingForBiosJob --> HI_WBJ_HandleBiosJobFailure : BIOS job failed
    HI_WaitingForBiosJob --> Failed : Retry budget exhausted (BiosSetupFailed)
    HI_WBJ_HandleBiosJobFailure --> HI_WaitingForPlatformConfiguration : Power off + BMC reset + power on
    HI_PollingBiosSetup --> HI_PollingBiosSetup : Wait for BIOS setup
    HI_PollingBiosSetup --> HI_WBJ_HandleBiosJobFailure : Stuck > 15 min, retry budget left
    HI_PollingBiosSetup --> Failed : Stuck > 15 min, retry budget exhausted (BiosSetupFailed)
    HI_PollingBiosSetup --> HI_SBO_SetBootOrder : BIOS is setup

    HI_SBO_SetBootOrder --> hi_sbo_if_zero_dpu
    hi_sbo_if_zero_dpu --> HI_SBO_WaitForSetBootOrderJobCompletion : No DPU
    hi_sbo_if_zero_dpu --> HI_SBO_WaitForSetBootOrderJobScheduled : DPU
    HI_SBO_WaitForSetBootOrderJobScheduled --> HI_SBO_RebootHost
    HI_SBO_RebootHost --> HI_SBO_WaitForSetBootOrderJobCompletion
    HI_SBO_SetBootOrder --> HI_SBO_WaitForSetBootOrderJobCompletion
    HI_SBO_WaitForSetBootOrderJobCompletion --> hi_attestation_enabled

    hi_attestation_enabled --> HI_M_WaitingForMeasurements : if attestation is enabled
    hi_attestation_enabled --> HI_WaitingForDiscovery : if attestation is disabled

    HI_M_WaitingForMeasurements --> HI_M_WaitingForMeasurements : Waiting for machine to send measurement report
    HI_M_WaitingForMeasurements --> HI_M_PendingBundle : Wait for golden values
    HI_M_WaitingForMeasurements --> HI_WaitingForDiscovery : Measurements validated
    HI_M_PendingBundle --> HI_M_PendingBundle : Waiting for matching measurement bundle
    HI_M_PendingBundle --> HI_M_WaitingForMeasurements : Measurements wiped, restart
    HI_M_PendingBundle --> HI_WaitingForDiscovery : Measurements validated

    HI_WaitingForDiscovery --> HI_WaitingForDiscovery : Wait for scout to update
    HI_WaitingForDiscovery --> HI_USS_SetUefiPassword : Discovery is Successful

    HI_USS_UnlockHost --> HI_USS_SetUefiPassword
    HI_USS_SetUefiPassword --> HI_USS_WaitForPasswordJobScheduled : job created
    HI_USS_SetUefiPassword --> HI_USS_LockdownHost : Fail AND !Dell AND !Lenovo AND !Nvidia
    HI_USS_WaitForPasswordJobScheduled --> HI_USS_PowercycleHost
    HI_USS_PowercycleHost --> HI_USS_WaitForPasswordJobCompletion
    HI_USS_WaitForPasswordJobCompletion --> HI_USS_LockdownHost

    HI_USS_LockdownHost --> HI_WFL_TimeWaitForDPUDown

    HI_WFL_TimeWaitForDPUDown --> HI_WFL_WaitForDPUUp
    HI_WFL_WaitForDPUUp --> BomValidating_BV_MatchingSku

    BomValidating --> HI_Discovered
    Validation_V_MachineValidating --> HI_Discovered
    DPUReprovision_DR_RebootHost --> HI_Discovered
    HI_Discovered --> HI_Discovered : need reboot
    HI_Discovered --> Ready : rebooted
    Ready --> HI_USS_UnlockHost : Need setup BIOS password
```

## BOM Validation State Details (BomValidating)

Shows the BOM (Bill of Materials) validation process:

```mermaid
stateDiagram-v2
    state "HostInitState/WaitForDPUUp" as HostInitState_HI_WFL_WaitForDPUUp
    state Ready
    state "WaitingForCleanup/HostCleanup" as WaitingForCleanup_C_HostCleanup
    state "WaitingForCleanup/LockHost" as WaitingForCleanup_C_SBV_LockHost

    state bv_requested <<choice>>
    state "MatchingSku" as BV_MatchingSku
    state "UpdatingInventory" as BV_UpdatingInventory
    state "VerifyingSku" as BV_VerifyingSku
    state "SkuVerificationFailed" as BV_SkuVerificationFailed
    state "WaitingForSkuAssignment" as BV_WaitingForSkuAssignment
    state "SkuMissing" as BV_SkuMissing
    state bv_validation_enabled <<choice>>

    state "Validation/RebootHost" as Validation_V_RebootHost
    state "HostInit/Discovered" as HostInit_HI_Discovered

    HostInitState_HI_WFL_WaitForDPUUp --> BV_MatchingSku
    WaitingForCleanup_C_HostCleanup --> BV_UpdatingInventory
    WaitingForCleanup_C_SBV_LockHost --> BV_UpdatingInventory
    Ready --> bv_requested

    bv_requested --> BV_UpdatingInventory : Possible SKU match
    bv_requested --> BV_WaitingForSkuAssignment : SKU unassigned
    bv_requested --> BV_SkuMissing : SKU not found

    BV_MatchingSku --> BV_VerifyingSku : SKU present
    BV_MatchingSku --> BV_WaitingForSkuAssignment : SKU not present AND !matched
    BV_UpdatingInventory --> BV_UpdatingInventory : Wait discovery
    BV_UpdatingInventory --> BV_MatchingSku : SKU not present
    BV_UpdatingInventory --> BV_VerifyingSku : SKU present
    BV_VerifyingSku --> BV_MatchingSku : SKU present
    BV_VerifyingSku --> BV_SkuMissing : SKU not found
    BV_VerifyingSku --> BV_SkuVerificationFailed : SKU diff is not empty
    BV_SkuVerificationFailed --> BV_WaitingForSkuAssignment : SKU not present
    BV_SkuVerificationFailed --> BV_UpdatingInventory : Update timeout
    BV_SkuVerificationFailed --> BV_SkuVerificationFailed : Wait update timeout
    BV_WaitingForSkuAssignment --> BV_UpdatingInventory : SKU present OR matched
    BV_WaitingForSkuAssignment --> BV_WaitingForSkuAssignment : wait assignment
    BV_SkuMissing --> BV_UpdatingInventory : SKU present and found
    BV_SkuMissing --> BV_SkuMissing : SKU present and not found
    BV_SkuMissing --> BV_WaitingForSkuAssignment : SKU not present

    BV_WaitingForSkuAssignment --> bv_validation_enabled : BOM validation disabled
    BV_MatchingSku --> bv_validation_enabled : BOM validation disabled OR (SKU Present and matched)
    BV_VerifyingSku --> bv_validation_enabled : SKU diff is empty

    bv_validation_enabled --> Validation_V_RebootHost : validation enabled
    bv_validation_enabled --> HostInit_HI_Discovered : validation disabled
```

## Machine Validation State Details (ValidationState)

Shows the machine validation process:

```mermaid
stateDiagram-v2
    direction LR

    state "BomValidating/bv_validation_enabled" as BomValidating_bv_validation_enabled
    state Ready

    state "RebootHost" as V_RebootHost
    state "MachineValidating" as V_MachineValidating

    state "HostInit/Discovered" as HostInit_HI_Discovered
    state Failed

    Ready --> V_RebootHost : validation is requested
    BomValidating_bv_validation_enabled --> V_RebootHost : validation is enabled
    V_RebootHost --> V_MachineValidating
    V_MachineValidating --> V_MachineValidating : validation in progress
    V_MachineValidating --> HostInit_HI_Discovered : validation disabled OR validated successfully
    V_MachineValidating --> Failed : validation failed
```

## Ready State Details (Ready)

Shows what can happen in Ready state:

```mermaid
stateDiagram-v2
    direction LR

    state Measuring
    state HostReprovision
    state "HostInit/Discovered" as HostInit_HI_Discovered
    state Ready
    state "Validation/RebootHost" as Validation_V_RebootHost
    state "BomValidating/bv_requested" as BomValidating_bv_requested
    state "Assigned/WaitingForNetworkSegmentToBeReady" as Assigned_A_WaitingForNetworkSegmentToBeReady
    state "HostInit/UefiSetup/UnlockHost" as HostInit_HI_UefiSetup_HI_USS_UnlockHost
    state "Measuring/WaitingForMeasurements" as Measuring_M_WaitingForMeasurements
    state "DPUReprovision/dr_bfb_check_support" as DPUReprovision_dr_bfb_check_support
    state "HostReprovision/CheckingFirmware" as HostReprovision_HR_CheckingFirmware

    HostInit_HI_Discovered --> Ready
    Measuring --> Ready : Measuring completed
    HostReprovision --> Ready : Host reprovision completed

    Ready --> Validation_V_RebootHost : Machine validation requested
    Ready --> BomValidating_bv_requested : BOM validation requested
    Ready --> DPUReprovision_dr_bfb_check_support : DPU reprovision requested
    Ready --> HostReprovision_HR_CheckingFirmware : Host reprovision requested
    Ready --> Assigned_A_WaitingForNetworkSegmentToBeReady : Instance assigned
    Ready --> Measuring_M_WaitingForMeasurements : Redo measuring requested
    Ready --> HostInit_HI_UefiSetup_HI_USS_UnlockHost : Need setup BIOS password
```

## Instance Assignment State Details (InstanceState)

Shows the complete instance assignment and management flow:

```mermaid
stateDiagram-v2
    state Ready
    state "DPUReprovision/DR_RebootHost" as DPUReprovision_DR_RebootHost
    state "HostReprovision/HR_CheckingFirmware" as HostReprovision_HR_CheckingFirmware

    state "WaitingForNetworkSegmentToBeReady" as A_WaitingForNetworkSegmentToBeReady
    state "WaitingForNetworkConfig" as A_WaitingForNetworkConfig
    state "WaitingForStorageConfig (legacy recovery)" as A_WaitingForStorageConfig
    state "WaitingForExtensionServicesConfig (legacy recovery)" as A_WaitingForExtensionServicesConfig
    state "WaitingForRebootToReady" as A_WaitingForRebootToReady
    state "Assigned/Ready" as A_Ready
    state "WaitingForDpusToUp" as A_WaitingForDpusToUp
    state "BootingWithDiscoveryImage" as A_BootingWithDiscoveryImage
    state "SwitchToAdminNetwork" as A_SwitchToAdminNetwork
    state "WaitingForNetworkReconfig" as A_WaitingForNetworkReconfig
    state a_release_instance <<choice>>
    state "Assigned/Failed" as A_Failed
    state "NetworkConfigUpdate" as A_NetworkConfigUpdate {
        state "WaitingForNetworkSegmentToBeReady" as A_NCU_WaitingForNetworkSegmentToBeReady
        state "WaitingForConfigSynced" as A_NCU_WaitingForConfigSynced
        state "ReleaseOldResources" as A_NCU_ReleaseOldResources
    }
    state "HostPlatformConfiguration" as A_HostPlatformConfiguration {
        state "FactoryResetBmc" as A_HPC_FactoryResetBmc
        state "PowerCycle" as A_HPC_PowerCycle
        state "UnlockHost" as A_HPC_UnlockHost {
            state "DisableLockdown" as A_HPC_UH_DisableLockdown
            state "RebootHost" as A_HPC_UH_RebootHost
            state "WaitForUefiBoot" as A_HPC_UH_WaitForUefiBoot
        }
        state "CheckHostConfig" as A_HPC_CheckHostConfig
        state "ConfigureBios" as A_HPC_ConfigureBios
        state "WaitingForBiosJob" as A_HPC_WaitingForBiosJob {
            state "HandleBiosJobFailure" as A_HPC_WBJ_HandleBiosJobFailure
        }
        state "PollingBiosSetup" as A_HPC_PollingBiosSetup
        state "SetBootOrder" as A_HPC_SetBootOrder {
            state "SetBootOrder" as A_HPC_SBO_SetBootOrder
            state "WaitForSetBootOrderJobScheduled" as A_HPC_SBO_WaitForSetBootOrderJobScheduled
            state "RebootHost" as A_HPC_SBO_RebootHost
            state "WaitForSetBootOrderJobCompletion" as A_HPC_SBO_WaitForSetBootOrderJobCompletion
        }
        state "LockHost" as A_HPC_LockHost
    }

    state "PostAssignedMeasuring/WaitingForMeasurements" as PostAssignedMeasuring_M_WaitingForMeasurements
    state "WaitingForCleanup/Init" as WaitingForCleanup_C_Init
    state "DPUReprovision/dr_bfb_check_support" as DPUReprovision_dr_bfb_check_support

    Ready --> A_WaitingForNetworkSegmentToBeReady : Instance assigned

    A_WaitingForNetworkSegmentToBeReady --> A_WaitingForNetworkSegmentToBeReady : segments not ready
    A_WaitingForNetworkSegmentToBeReady --> A_WaitingForNetworkConfig : No segment OR segments ready

    A_WaitingForNetworkConfig --> A_WaitingForNetworkConfig : An assigned provisioning gate is not ready
    A_WaitingForNetworkConfig --> A_WaitingForRebootToReady : Deletion requested OR all assigned provisioning gates clear

    A_WaitingForStorageConfig --> A_WaitingForExtensionServicesConfig : Recover a persisted legacy state
    A_WaitingForExtensionServicesConfig --> A_WaitingForExtensionServicesConfig : Extension services not ready
    A_WaitingForExtensionServicesConfig --> A_WaitingForRebootToReady : No managed DPU OR no configured services OR services ready
    A_WaitingForRebootToReady --> A_WaitingForRebootToReady : Aggregate PreventHostStateChanges OR managed DPU PXE signal
    A_WaitingForRebootToReady --> A_Ready : Readiness checks pass OR deletion or custom iPXE bypass; ForceRestart

    A_Ready --> A_NCU_WaitingForNetworkSegmentToBeReady : Update network request
    A_Ready --> A_HPC_FactoryResetBmc : Instance deleted
    A_Ready --> A_HPC_UH_DisableLockdown : Host or DPU reprovisioning, firmware update, or custom iPXE requested
    A_HPC_FactoryResetBmc --> A_HPC_PowerCycle : Factory reset completes or is disabled

    A_WaitingForDpusToUp --> A_WaitingForRebootToReady : DPUs ready or absent AND custom iPXE requested
    A_WaitingForDpusToUp --> A_BootingWithDiscoveryImage : DPUs ready or absent AND no custom iPXE request
    A_WaitingForDpusToUp --> A_WaitingForDpusToUp : Managed DPUs not ready

    A_BootingWithDiscoveryImage --> A_BootingWithDiscoveryImage : Retry reboot if needed
    A_BootingWithDiscoveryImage --> A_SwitchToAdminNetwork : If instance deleted
    A_BootingWithDiscoveryImage --> DPUReprovision_dr_bfb_check_support : DPU reprovision needed
    A_BootingWithDiscoveryImage --> HostReprovision_HR_CheckingFirmware : Host reprovision needed
    A_BootingWithDiscoveryImage --> A_BootingWithDiscoveryImage : Nothing is needed. Stuck.

    DPUReprovision_DR_RebootHost --> A_Ready : Reprovision completed
    state "HostReprovision/hr_completed" as HostReprovision_hr_completed
    HostReprovision_hr_completed --> A_Ready : Reprovision not needed

    A_SwitchToAdminNetwork --> A_WaitingForNetworkReconfig : Update network config

    A_WaitingForNetworkReconfig --> A_WaitingForNetworkReconfig : Network not synced
    A_WaitingForNetworkReconfig --> a_release_instance : Delete instance & clean network config
    a_release_instance --> PostAssignedMeasuring_M_WaitingForMeasurements : attenstation enabled
    a_release_instance --> WaitingForCleanup_C_Init : attenstation disabled

    A_NCU_WaitingForNetworkSegmentToBeReady --> A_NCU_WaitingForConfigSynced : No segments OR All ready
    A_NCU_WaitingForNetworkSegmentToBeReady --> A_NCU_WaitingForNetworkSegmentToBeReady : Not all segments ready

    A_NCU_WaitingForConfigSynced --> A_NCU_ReleaseOldResources : No associated DPU OR observations synced with no aggregate PreventHostStateChanges
    A_NCU_WaitingForConfigSynced --> A_NCU_WaitingForConfigSynced : Managed DPU observation not synced OR aggregate PreventHostStateChanges
    A_NCU_ReleaseOldResources --> A_Ready

    A_HPC_PowerCycle --> A_HPC_PowerCycle : Wait Power Off
    A_HPC_PowerCycle --> A_HPC_UH_DisableLockdown : Power On
    A_HPC_UH_DisableLockdown --> A_HPC_UH_RebootHost : BMC lockdown disabled
    A_HPC_UH_RebootHost --> A_HPC_UH_WaitForUefiBoot : ForceRestart issued
    A_HPC_UH_WaitForUefiBoot --> A_HPC_UH_WaitForUefiBoot : Waiting for UEFI boot (5 min)
    A_HPC_UH_WaitForUefiBoot --> A_HPC_CheckHostConfig : UEFI boot wait complete
    A_HPC_CheckHostConfig --> A_HPC_CheckHostConfig : Wait DPU Up
    A_HPC_CheckHostConfig --> A_HPC_ConfigureBios : Need config host boot order
    A_HPC_CheckHostConfig --> A_HPC_LockHost : No need config host boot order
    A_HPC_ConfigureBios --> A_HPC_PollingBiosSetup : Config BIOS
    A_HPC_ConfigureBios --> A_HPC_WaitingForBiosJob : Dell BIOS job scheduled
    A_HPC_WaitingForBiosJob --> A_HPC_PollingBiosSetup : BIOS job completed
    A_HPC_WaitingForBiosJob --> A_HPC_WBJ_HandleBiosJobFailure : BIOS job failed
    A_HPC_WaitingForBiosJob --> A_Failed : Retry budget exhausted (BiosSetupFailed)
    A_HPC_WBJ_HandleBiosJobFailure --> A_HPC_ConfigureBios : Power off + BMC reset + power on
    A_HPC_PollingBiosSetup --> A_HPC_PollingBiosSetup : Wait for BIOS setup
    A_HPC_PollingBiosSetup --> A_HPC_WBJ_HandleBiosJobFailure : Stuck > 15 min, retry budget left
    A_HPC_PollingBiosSetup --> A_Failed : Stuck > 15 min, retry budget exhausted (BiosSetupFailed)
    A_HPC_PollingBiosSetup --> A_HPC_SBO_SetBootOrder : BIOS is setup
    A_HPC_SBO_SetBootOrder --> A_HPC_SBO_WaitForSetBootOrderJobScheduled : Set boot order job scheduled
    A_HPC_SBO_WaitForSetBootOrderJobScheduled --> A_HPC_SBO_RebootHost : Job scheduled
    A_HPC_SBO_RebootHost --> A_HPC_SBO_WaitForSetBootOrderJobCompletion : Reboot
    A_HPC_SBO_WaitForSetBootOrderJobCompletion --> A_HPC_LockHost : Job completed
    A_HPC_LockHost --> A_WaitingForDpusToUp : BMC lockdown enabled

    state AnyState
    AnyState --> A_Failed : Any failure condition
    A_Failed --> A_Failed : Wait (stuck, manual action needed)
    A_Failed --> A_HPC_SBO_SetBootOrder : BiosSetupFailed AND is_bios_setup ok
```

### Network Readiness Gates

During initial provisioning, `WaitingForNetworkConfig` waits for all required
network configuration observations to match their desired versions. For a host
with associated DPUs, it also waits for aggregate health alerts that prevent
host state changes to clear. A host without associated DPUs skips both the DPU
observation and aggregate health checks in this state.

The normal agent report gives a p0 transport failure the combination of
`BgpPeeringTor` and `PreventAllocations`. The PXE gate matches that probe ID and
classification; it does not inspect the alert target. A host `Replace` report
takes precedence for this check. Without one, the primary DPU `Replace` report
takes precedence over that DPU's `Merge` reports. Refer to
[DPU ToR Uplink Health](../../dpu-management/dpu_configuration.md#dpu-tor-uplink-health)
for the uplink policy and classifications.

`WaitingForRebootToReady` repeats the aggregate health and primary p0 checks
immediately before the normal PXE `ForceRestart`. This closes the interval in
which health can change after the configuration check. Hosts without managed
DPUs skip this PXE signal check, including one supplied by a host `Replace`
report, but still receive the aggregate health check. In
`WaitingForNetworkConfig`, a host `Replace` report can supply the matching
signal before NICo looks for a primary DPU, including on a host without managed
DPUs.

Normal success proceeds directly from `WaitingForNetworkConfig` to
`WaitingForRebootToReady`. `WaitingForStorageConfig` and
`WaitingForExtensionServicesConfig` remain only so NICo can recover hosts that
persisted those states on an older release.

The following requests bypass these readiness checks:

- Instance deletion proceeds through its required restart without waiting for
  network health.
- An explicit custom iPXE request proceeds through its requested restart.

A live network update uses `NetworkConfigUpdate/WaitingForConfigSynced`. A host
with associated DPUs waits for current observations and aggregate health alerts
that prevent host state changes. A host without associated DPUs skips both
checks. This path does not apply the primary p0 PXE gate or restart the host.

## Host Reprovision State Details (HostReprovisionState)

Shows the host firmware reprovision process:

```mermaid
stateDiagram-v2
    state Ready
    state "Assigned/BootingWithDiscoveryImage" as Assigned_A_BootingWithDiscoveryImage

    state "CheckingFirmware" as HR_CheckingFirmware
    state "CheckingFirmwareRepeat" as HR_CheckingFirmwareRepeat
    state "InitialReset" as HR_InitialReset {
        state "Start" as HR_IR_Start
        state "BMCWasReset" as HR_IR_BMCWasReset
        state "WaitBoot" as HR_IR_WaitBoot
    }
    state "WaitingForScript" as HR_WaitingForScript
    state "WaitingForUpload" as HR_WaitingForUpload
    state "WaitingForFirmwareUpgrade" as HR_WaitingForFirmwareUpgrade
    state "ResetForNewFirmware" as HR_ResetForNewFirmware
    state "NewFirmwareReportedWait" as HR_NewFirmwareReportedWait
    state "FailedFirmwareUpgrade" as HR_FailedFirmwareUpgrade
    state hr_completed <<choice>>

    state "Assigned/Ready" as Assigned_A_Ready

    Ready --> HR_CheckingFirmware : Host reprovision request
    Assigned_A_BootingWithDiscoveryImage --> HR_CheckingFirmware : Host reprovision request

    HR_CheckingFirmware --> HR_IR_Start : Firmware needs pre-update reset
    HR_IR_Start --> HR_IR_BMCWasReset : Power Off
    HR_IR_BMCWasReset --> HR_IR_WaitBoot : Power On
    HR_IR_WaitBoot --> HR_IR_WaitBoot : Wait 20 min
    HR_IR_WaitBoot --> HR_CheckingFirmwareRepeat
    HR_CheckingFirmware --> HR_WaitingForScript : Upgrade using script
    HR_WaitingForScript --> HR_CheckingFirmwareRepeat : Script completed
    HR_WaitingForScript --> HR_FailedFirmwareUpgrade : Script failed
    HR_CheckingFirmware --> HR_CheckingFirmwareRepeat : No explored endpoint data
    HR_CheckingFirmware --> hr_completed : FW info not found
    HR_CheckingFirmwareRepeat --> hr_completed : FW info not found
    HR_CheckingFirmware --> HR_WaitingForUpload
    HR_WaitingForUpload --> HR_CheckingFirmwareRepeat : No upload status upload error
    HR_WaitingForUpload --> HR_WaitingForFirmwareUpgrade : Upload success
    HR_WaitingForFirmwareUpgrade --> HR_WaitingForUpload : Update AND has more
    HR_WaitingForFirmwareUpgrade --> HR_ResetForNewFirmware : Updated last
    HR_WaitingForFirmwareUpgrade --> HR_CheckingFirmwareRepeat : Task interrupted OR get error
    HR_WaitingForFirmwareUpgrade --> HR_FailedFirmwareUpgrade : Unknown task status
    HR_ResetForNewFirmware --> HR_NewFirmwareReportedWait
    HR_NewFirmwareReportedWait --> hr_completed
    HR_FailedFirmwareUpgrade --> HR_FailedFirmwareUpgrade : Wait retry timeout
    HR_FailedFirmwareUpgrade --> HR_CheckingFirmware : Retry after timeout

    hr_completed --> Assigned_A_Ready : if Assigned state
    hr_completed --> Ready : if source is Ready state
```

## DPU Reprovision State Details (DpuReprovisionState)

Shows the DPU firmware reprovision process:

```mermaid
stateDiagram-v2
    state Ready
    state "Assigned/BootingWithDiscoveryImage" as Assigned_A_BootingWithDiscoveryImage

    state dr_bfb_check_support <<choice>>
    state "BmcFirmwareUpgrade" as DR_BmcFirmwareUpgrade
    state "InstallDpuOs" as DR_InstallDpuOs {
        state "InstallingBFB" as DR_IDO_InstallingBFB
        state "WaitForInstallComplete" as DR_IDO_WaitForInstallComplete
        state "Completed" as DR_IDO_Completed
        state "InstallationError" as DR_IDO_InstallationError
    }
    state "FirmwareUpgrade" as DR_FirmwareUpgrade
    state "WaitingForNetworkInstall" as DR_WaitingForNetworkInstall
    state "PoweringOffHost" as DR_PoweringOffHost
    state "PowerDown" as DR_PowerDown
    state "BufferTime" as DR_BufferTime
    state "VerifyFirmareVersions" as DR_VerifyFirmareVersions
    state "WaitingForNetworkConfig" as DR_WaitingForNetworkConfig
    state "RebootHostBmc" as DR_RebootHostBmc
    state "RebootHost" as DR_RebootHost
    state "NotUnderReprovision" as DR_NotUnderReprovision

    state "Assigned/Ready" as Assigned_A_Ready
    state "HostInit/Discovered" as HostInit_HI_Discovered
    state "HostReprovision/CheckingFirmware" as HostReprovision_HR_CheckingFirmware

    Ready --> dr_bfb_check_support : DPU reprovision request
    Assigned_A_BootingWithDiscoveryImage --> dr_bfb_check_support : DPU reprovision request

    dr_bfb_check_support --> DR_IDO_InstallingBFB : BFB install supported
    dr_bfb_check_support --> DR_WaitingForNetworkInstall : BFB install not supported

    DR_IDO_InstallingBFB --> DR_IDO_WaitForInstallComplete : Start update
    DR_IDO_WaitForInstallComplete --> DR_IDO_Completed : Task completed
    DR_IDO_WaitForInstallComplete --> DR_IDO_InstallationError : Task exception OR unknown state
    DR_IDO_WaitForInstallComplete --> DR_IDO_WaitForInstallComplete : Wait task completion
    DR_IDO_Completed --> DR_WaitingForNetworkInstall

    DR_BmcFirmwareUpgrade --> DR_FirmwareUpgrade : deprecated
    DR_FirmwareUpgrade --> DR_WaitingForNetworkInstall : deprecated
    DR_BufferTime --> DR_VerifyFirmareVersions : deprecated
    DR_WaitingForNetworkInstall --> DR_PoweringOffHost
    DR_PoweringOffHost --> DR_PoweringOffHost : wait all DPU ready
    DR_PoweringOffHost --> DR_PowerDown : DPUs ready
    DR_PowerDown --> DR_PowerDown : Wait Power off
    DR_PowerDown --> DR_VerifyFirmareVersions : Power is off
    DR_VerifyFirmareVersions --> DR_WaitingForNetworkConfig
    DR_WaitingForNetworkConfig --> DR_WaitingForNetworkConfig : Any DPU is not up OR network not synced
    DR_WaitingForNetworkConfig --> DR_RebootHostBmc
    DR_RebootHostBmc --> DR_RebootHost
    DR_RebootHost --> HostInit_HI_Discovered : if entered from Ready
    DR_RebootHost --> HostReprovision_HR_CheckingFirmware : if entered from Assigned AND host reprovision requested
    DR_RebootHost --> Assigned_A_Ready : if entered from Assigned AND host reprovision not requested
```

## WaitingForCleanup State Details

```mermaid
stateDiagram-v2
    state "Assigned/A_WaitingForNetworkReconfig" as Assigned_A_WaitingForNetworkReconfig
    state Failed

    state "Init" as C_Init
    state "SecureEraseBoss" as C_SecureEraseBoss {
        state "UnlockHost" as C_SEB_UnlockHost
        state "SecureEraseBoss" as C_SEB_SecureEraseBoss
        state "WaitForJobCompletion" as C_SEB_WaitForJobCompletion
        state "HandleJobFailure" as C_SEB_HandleJobFailure
    }
    state C_HostCleanup
    state C_CreateBossVolume {
        state "CreateBossVolume" as C_SBV_CreateBossVolume
        state "WaitForJobScheduled" as C_SBV_WaitForJobScheduled
        state "RebootHost" as C_SBV_RebootHost
        state "WaitForJobCompletion" as C_SBV_WaitForJobCompletion
        state "LockHost" as C_SBV_LockHost
        state "HandleJobFailure" as C_SBV_HandleJobFailure
    }
    state "DisableBIOSBMCLockdown" as C_DisableBIOSBMCLockdown

    state "BomValidating/UpdatingInventory" as BomValidating_BV_UpdatingInventory

    Failed --> C_Init
    Assigned_A_WaitingForNetworkReconfig --> C_Init
    C_Init --> C_SEB_UnlockHost : If Dell AND has Boss controller
    C_Init --> C_HostCleanup : If not Dell OR no Boss controller

    C_SEB_UnlockHost --> C_SEB_SecureEraseBoss
    C_SEB_SecureEraseBoss --> C_SEB_WaitForJobCompletion
    C_SEB_WaitForJobCompletion --> C_HostCleanup : Job success
    C_SEB_WaitForJobCompletion --> C_SEB_HandleJobFailure : Job fail
    C_SEB_WaitForJobCompletion --> C_SEB_WaitForJobCompletion : Wait job to complete
    C_SEB_HandleJobFailure --> C_SEB_HandleJobFailure : Power off OR Power on if needed
    C_SEB_HandleJobFailure --> C_SEB_SecureEraseBoss : Retry
    C_SEB_HandleJobFailure --> C_SEB_HandleJobFailure : Power cycle (Off / On)
    C_HostCleanup --> C_HostCleanup : Wait for cleanup
    C_HostCleanup --> C_SBV_CreateBossVolume : if Boss controller present
    C_HostCleanup --> BomValidating_BV_UpdatingInventory : if Boss controller not present

    C_SBV_CreateBossVolume --> C_SBV_WaitForJobScheduled
    C_SBV_WaitForJobScheduled --> C_SBV_RebootHost
    C_SBV_RebootHost --> C_SBV_WaitForJobCompletion
    C_SBV_WaitForJobCompletion --> C_SBV_HandleJobFailure : Job fail
    C_SBV_WaitForJobCompletion --> C_SBV_LockHost : Job success
    C_SBV_WaitForJobCompletion --> C_SBV_WaitForJobCompletion : Wait job to complete
    C_SBV_HandleJobFailure --> C_SBV_HandleJobFailure : Power off OR Power on if needed
    C_SBV_HandleJobFailure --> C_SBV_CreateBossVolume : Retry
    C_SBV_HandleJobFailure --> C_SBV_HandleJobFailure : Power cycle (Off / On)
    C_SBV_LockHost --> BomValidating_BV_UpdatingInventory
```

## Measuring and PostAssignedMeasuring State Details

Shows the attestation measurement process.

```mermaid
stateDiagram-v2
    state "Assigned/WaitingForNetworkReconfig" as Assigned_A_WaitingForNetworkReconfig
    state Ready
    state "WaitingForMeasurements" as M_WaitingForMeasurements
    state "PendingBundle" as M_PendingBundle
    state m_return_success <<choice>>
    state Failed
    state "WaitingForCleanup/Init" as WaitingForCleanup_C_Init

    Ready --> M_WaitingForMeasurements : Redo measuring requested
    Assigned_A_WaitingForNetworkReconfig --> M_WaitingForMeasurements : On instance delete
    M_WaitingForMeasurements --> M_PendingBundle : Wait for golden values
    M_WaitingForMeasurements --> M_WaitingForMeasurements : Wait for measurements
    M_WaitingForMeasurements --> m_return_success : Measurements validated
    M_WaitingForMeasurements --> Failed : Measurements failed
    M_PendingBundle --> m_return_success : Measurements validated
    M_PendingBundle --> Failed : Measurements failed
    m_return_success --> Ready : if came from Ready state
    m_return_success --> WaitingForCleanup_C_Init : if came from Assigned state
```

## Failed State

```mermaid
stateDiagram-v2
    direction LR

    state AnyNotAssignedState
    state Failed
    state "HostInit/WaitingForLockdown/TimeWaitForDPUDown" as HostInit_HI_WaitingForLockdown_HI_WFL_TimeWaitForDPUDown
    state "WaitingForCleanup/Init" as WaitingForCleanup_C_Init
    state "HostInit/Measuring/WaitingForMeasurements" as HostInit_HI_Measuring_HI_M_WaitingForMeasurements
    state "Measuring/WaitingForMeasurements" as Measuring_M_WaitingForMeasurements
    state "PostAssignedMeasuring/WaitingForMeasurements" as PostAssignedMeasuring_M_WaitingForMeasurements
    state "Validation/RebootHost" as Validation_V_RebootHost
    state ForceDeletion

    AnyNotAssignedState --> Failed
    Failed --> HostInit_HI_WaitingForLockdown_HI_WFL_TimeWaitForDPUDown : Discovery Failure On discovery succeeded
    Failed --> Failed : Reboot with retry count
    Failed --> WaitingForCleanup_C_Init : NVMECleanFailed Cleaned up successfully after a failure
    Failed --> Measuring_M_WaitingForMeasurements : Measurements Fail Not in original failure cause anymore
    Failed --> HostInit_HI_Measuring_HI_M_WaitingForMeasurements : Measurements Fail Not in original failure cause anymore
    Failed --> PostAssignedMeasuring_M_WaitingForMeasurements : Measurements Fail Not in original failure cause anymore
    Failed --> Validation_V_RebootHost : MachineValidation Fail Machine validation requested
    Failed --> Failed : Non-recoverable cause
    Failed --> ForceDeletion : Admin force deletion
```
