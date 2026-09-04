/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::collections::HashMap;
use std::sync::LazyLock;

use super::ExternalUserInfo;
use crate::auth::Principal;

static INTERNAL_RBAC_RULES: LazyLock<InternalRBACRules> = LazyLock::new(InternalRBACRules::new);

#[derive(Debug)]
pub(super) struct InternalRBACRules {
    perms: std::collections::HashMap<String, RuleInfo>,
}

#[derive(Debug)]
enum RulePrincipal {
    ForgeAdminCLI,
    Machineatron,
    SiteAgent,
    Agent, // Agent on the DPU, NOT site agent
    Scout,
    Dns,
    Dhcp,
    Ssh,
    SshRs,
    Health,
    Pxe,
    BmcProxy,
    Flow,
    MaintenanceJobs,
    DsxExchangeConsumer,
    Anonymous, // Permitted for everything
}
use self::RulePrincipal::{
    Agent, Anonymous, BmcProxy, Dhcp, Dns, DsxExchangeConsumer, Flow, ForgeAdminCLI, Health,
    Machineatron, MaintenanceJobs, Pxe, Scout, SiteAgent, Ssh, SshRs,
};

impl InternalRBACRules {
    pub(super) fn new() -> Self {
        let mut x = Self {
            perms: HashMap::default(),
        };

        // Add additional permissions to the list below.
        x.perm("Version", vec![Anonymous]);
        x.perm("CreateDomain", vec![SiteAgent]);
        x.perm("CreateDomainLegacy", vec![]);
        x.perm("UpdateDomainLegacy", vec![]);
        x.perm("DeleteDomainLegacy", vec![]);
        x.perm("FindDomainLegacy", vec![ForgeAdminCLI]);
        x.perm("UpdateDomain", vec![SiteAgent]);
        x.perm("DeleteDomain", vec![SiteAgent]);
        x.perm("FindDomain", vec![ForgeAdminCLI]);
        x.perm("CreateVpc", vec![SiteAgent, Machineatron]);
        x.perm("UpdateVpc", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateVpcVirtualization", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteVpc", vec![Machineatron, SiteAgent]);
        x.perm("FindVpcIds", vec![SiteAgent, ForgeAdminCLI, Machineatron]);
        x.perm("FindVpcsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateSitePrefix", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateSitePrefix", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteSitePrefix", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindSitePrefixIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindSitePrefixesByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindSitePrefixStateHistories",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("CreateVpcPrefix", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("SearchVpcPrefixes", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetVpcPrefixes", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateVpcPrefix", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteVpcPrefix", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindVpcPrefixStateHistories",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("GetAllDpaInterfaceIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindDpaInterfacesByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateDpaInterface", vec![]);
        x.perm("EnsureDpaInterface", vec![]);
        x.perm("DeleteDpaInterface", vec![]);
        x.perm("SetDpaNetworkObservationStatus", vec![]);
        x.perm(
            "FindNetworkSegmentIds",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "FindNetworkSegmentsByIds",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "FindNetworkSegmentStateHistories",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "CreateNetworkSegment",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm("AttachNetworkSegmentToVpc", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "DeleteNetworkSegment",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm("NetworkSegmentsForVpc", vec![]);
        x.perm("FindIBPartitionIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindIBPartitionsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateIBPartition", vec![SiteAgent]);
        x.perm("UpdateIBPartition", vec![SiteAgent]);
        x.perm("DeleteIBPartition", vec![SiteAgent]);
        x.perm("IBPartitionsForTenant", vec![]);
        x.perm("FindIBFabricIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "AllocateInstance",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "AllocateInstances",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm("ReleaseInstance", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateInstanceOperatingSystem", vec![SiteAgent]);
        x.perm("UpdateInstanceConfig", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindInstanceIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindInstancesByIds",
            vec![ForgeAdminCLI, SiteAgent, Ssh, SshRs],
        );
        x.perm(
            "FindInstanceByMachineID",
            vec![ForgeAdminCLI, Agent, SiteAgent],
        );
        x.perm("RecordObservedInstanceNetworkStatus", vec![]);
        x.perm(
            "GetManagedHostNetworkConfig",
            vec![ForgeAdminCLI, Agent, Machineatron, SiteAgent],
        );
        x.perm("RecordDpuNetworkStatus", vec![Agent, Machineatron]);
        x.perm(
            "ListMachineHealthReports",
            vec![ForgeAdminCLI, Health, Ssh, SshRs],
        );
        x.perm(
            "InsertMachineHealthReport",
            vec![ForgeAdminCLI, Health, SiteAgent, Ssh, SshRs, Flow],
        );
        x.perm(
            "RemoveMachineHealthReport",
            vec![ForgeAdminCLI, Health, SiteAgent, Ssh, SshRs, Flow],
        );
        x.perm(
            "ListRackHealthReports",
            vec![ForgeAdminCLI, Health, DsxExchangeConsumer],
        );
        x.perm(
            "InsertRackHealthReport",
            vec![ForgeAdminCLI, Health, DsxExchangeConsumer],
        );
        x.perm(
            "RemoveRackHealthReport",
            vec![ForgeAdminCLI, Health, DsxExchangeConsumer],
        );
        x.perm("ListSwitchHealthReports", vec![ForgeAdminCLI, Health]);
        x.perm("InsertSwitchHealthReport", vec![ForgeAdminCLI, Health]);
        x.perm("RemoveSwitchHealthReport", vec![ForgeAdminCLI, Health]);
        x.perm("ListPowerShelfHealthReports", vec![ForgeAdminCLI, Health]);
        x.perm("InsertPowerShelfHealthReport", vec![ForgeAdminCLI, Health]);
        x.perm("RemovePowerShelfHealthReport", vec![ForgeAdminCLI, Health]);
        x.perm("ListNVLinkDomainHealthReports", vec![ForgeAdminCLI, Health]);
        x.perm(
            "InsertNVLinkDomainHealthReport",
            vec![ForgeAdminCLI, Health],
        );
        x.perm(
            "RemoveNVLinkDomainHealthReport",
            vec![ForgeAdminCLI, Health],
        );
        // Deprecated aliases for the machine health report RPCs. Mirror the
        // permissions of their canonical equivalents above. Drop once we're
        // confident no clients are still calling the old names.
        x.perm(
            "ListHealthReportOverrides",
            vec![ForgeAdminCLI, Health, Ssh, SshRs],
        );
        x.perm(
            "InsertHealthReportOverride",
            vec![ForgeAdminCLI, Health, SiteAgent, Ssh, SshRs, Flow],
        );
        x.perm(
            "RemoveHealthReportOverride",
            vec![ForgeAdminCLI, Health, SiteAgent, Ssh, SshRs, Flow],
        );
        x.perm("DpuAgentUpgradeCheck", vec![Scout]);
        x.perm("DpuAgentUpgradePolicyAction", vec![ForgeAdminCLI]);
        x.perm("LookupRecord", vec![Dns]);
        x.perm("LookupRecordLegacy", vec![Dns]);
        x.perm("GetAllDomainMetadata", vec![Dns]);
        x.perm("GetAllDomains", vec![Dns]);
        x.perm("InvokeInstancePower", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ForgeAgentControl", vec![Machineatron, Scout]);
        x.perm("DiscoverMachine", vec![Anonymous]);
        x.perm("RenewMachineCertificate", vec![Agent]);
        x.perm("DiscoveryCompleted", vec![Machineatron, Scout]);
        x.perm("CleanupMachineCompleted", vec![Machineatron, Scout]);
        x.perm("ReportForgeScoutError", vec![Anonymous]);
        x.perm("ReportScoutFirmwareUpgradeStatus", vec![Scout]);
        x.perm("DiscoverDhcp", vec![Dhcp, Machineatron]);
        x.perm("ExpireDhcpLease", vec![Dhcp, Machineatron]);
        x.perm("AssignStaticAddress", vec![ForgeAdminCLI]);
        x.perm("RemoveStaticAddress", vec![ForgeAdminCLI]);
        x.perm("FindInterfaceAddresses", vec![ForgeAdminCLI]);
        x.perm("FindInterfaces", vec![ForgeAdminCLI, Agent, Flow]);
        x.perm("DeleteInterface", vec![ForgeAdminCLI]);
        x.perm("FindIpAddress", vec![ForgeAdminCLI]);
        x.perm(
            "FindMachineIds",
            vec![
                ForgeAdminCLI,
                Machineatron,
                Health,
                SiteAgent,
                Ssh,
                SshRs,
                Flow,
            ],
        );
        x.perm(
            "FindMachinesByIds",
            vec![
                ForgeAdminCLI,
                Machineatron,
                Health,
                SiteAgent,
                Ssh,
                SshRs,
                Flow,
            ],
        );
        x.perm("FindConnectedDevicesByDpuMachineIds", vec![ForgeAdminCLI]);
        x.perm("FindMachineIdsByBmcIps", vec![ForgeAdminCLI, Flow]);
        x.perm("FindMachineHealthHistories", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindMachineStateHistories", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("IdentifyUuid", vec![ForgeAdminCLI]);
        x.perm("IdentifyMac", vec![ForgeAdminCLI]);
        x.perm("IdentifySerial", vec![ForgeAdminCLI, Machineatron, Flow]);
        x.perm("GetBMCMetaData", vec![Health, Ssh, SshRs]);
        x.perm("UpdateBMCMetaData", vec![Machineatron]);
        x.perm("UpdateMachineCredentials", vec![]);
        x.perm("GetPxeInstructions", vec![Pxe, Machineatron]);
        x.perm("GetCloudInitInstructions", vec![Pxe]);
        x.perm("Echo", vec![Dhcp]);
        x.perm("CreateTenant", vec![SiteAgent]);
        x.perm("FindTenant", vec![SiteAgent, ForgeAdminCLI]);
        x.perm("UpdateTenant", vec![SiteAgent, ForgeAdminCLI]);
        x.perm("CreateTenantKeyset", vec![SiteAgent]);
        x.perm("FindTenantKeysetIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindTenantKeysetsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateTenantKeyset", vec![SiteAgent]);
        x.perm("DeleteTenantKeyset", vec![SiteAgent]);
        x.perm("ValidateTenantPublicKey", vec![SiteAgent, Ssh, SshRs]);
        x.perm("GetBmcCredentials", vec![Health, BmcProxy]);
        x.perm("GetSwitchNvosCredentials", vec![Health]);
        x.perm("GetAllManagedHostNetworkStatus", vec![ForgeAdminCLI]);
        x.perm(
            "GetSiteExplorationReport",
            vec![ForgeAdminCLI, Machineatron],
        );
        x.perm("GetSiteExplorerLastRun", vec![ForgeAdminCLI, Machineatron]);
        x.perm("ClearSiteExplorationError", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("IsBmcInManagedHost", vec![ForgeAdminCLI]);
        x.perm("Explore", vec![ForgeAdminCLI, Flow]);
        x.perm("ReExploreEndpoint", vec![ForgeAdminCLI, Flow, SiteAgent]);
        x.perm("RefreshEndpointReport", vec![ForgeAdminCLI, Flow]);
        x.perm("DeleteExploredEndpoint", vec![ForgeAdminCLI]);
        x.perm("PauseExploredEndpointRemediation", vec![ForgeAdminCLI]);
        x.perm(
            "FindExploredEndpointIds",
            vec![ForgeAdminCLI, Flow, SiteAgent],
        );
        x.perm(
            "FindExploredEndpointsByIds",
            vec![ForgeAdminCLI, Flow, SiteAgent],
        );
        x.perm("FindExploredManagedHostIds", vec![ForgeAdminCLI, Flow]);
        x.perm("FindExploredManagedHostsByIds", vec![ForgeAdminCLI, Flow]);
        x.perm("FindExploredMlxDeviceHostIds", vec![ForgeAdminCLI]);
        x.perm("FindExploredMlxDevicesByIds", vec![ForgeAdminCLI]);
        x.perm("AdminForceDeleteMachine", vec![ForgeAdminCLI, Machineatron]);
        x.perm(
            "DecommissionManagedHost",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm("AdminForceDeleteRack", vec![ForgeAdminCLI, Machineatron]);
        x.perm("AdminForceDeleteSwitch", vec![ForgeAdminCLI, Machineatron]);
        x.perm(
            "DecommissionSwitch",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm(
            "AdminForceDeletePowerShelf",
            vec![ForgeAdminCLI, Machineatron],
        );
        x.perm("AdminListResourcePools", vec![ForgeAdminCLI]);
        x.perm("AdminGrowResourcePool", vec![ForgeAdminCLI]);
        x.perm("SetMaintenance", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm("SetDynamicConfig", vec![ForgeAdminCLI, Machineatron]);
        x.perm("TriggerDpuReprovisioning", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("TriggerHostReprovisioning", vec![ForgeAdminCLI, Flow]);
        x.perm("ListDpuWaitingForReprovisioning", vec![ForgeAdminCLI]);
        x.perm("MarkManualFirmwareUpgradeComplete", vec![ForgeAdminCLI]);
        x.perm(
            "ListHostsWaitingForReprovisioning",
            vec![ForgeAdminCLI, Flow],
        );
        x.perm("GetDpuInfoList", vec![Agent]);
        x.perm("GetMachineBootOverride", vec![ForgeAdminCLI]);
        x.perm("SetMachineBootOverride", vec![ForgeAdminCLI]);
        x.perm("ClearMachineBootOverride", vec![ForgeAdminCLI]);
        x.perm("GetMachineBootInterfaces", vec![ForgeAdminCLI]);
        x.perm("GetNetworkTopology", vec![ForgeAdminCLI]);
        x.perm("FindNetworkDevicesByDeviceIds", vec![ForgeAdminCLI]);
        x.perm("CreateCredential", vec![ForgeAdminCLI]);
        x.perm("DeleteCredential", vec![ForgeAdminCLI]);
        x.perm("RotateCredential", vec![ForgeAdminCLI]);
        x.perm("GetCredentialRotationStatus", vec![ForgeAdminCLI]);
        x.perm("TriggerBmcCredentialRotation", vec![ForgeAdminCLI]);
        x.perm("TriggerUefiCredentialRotation", vec![ForgeAdminCLI]);
        x.perm("TriggerNicLockdownCredentialRotation", vec![ForgeAdminCLI]);
        x.perm("GetRouteServers", vec![ForgeAdminCLI]);
        x.perm("AddRouteServers", vec![ForgeAdminCLI]);
        x.perm("RemoveRouteServers", vec![ForgeAdminCLI]);
        x.perm("ReplaceRouteServers", vec![]);
        x.perm("UpdateAgentReportedInventory", vec![Agent]);
        x.perm("UpdateInstancePhoneHomeLastContact", vec![Agent]);
        x.perm("SetHostUefiPassword", vec![ForgeAdminCLI]);
        x.perm("ClearHostUefiPassword", vec![ForgeAdminCLI]);
        x.perm("SetDpuUefiPassword", vec![ForgeAdminCLI]);
        x.perm(
            "AddExpectedMachine",
            vec![ForgeAdminCLI, SiteAgent, Flow, Machineatron],
        );
        x.perm("DeleteExpectedMachine", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateExpectedMachine", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateExpectedMachines", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateExpectedMachines", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetExpectedMachine", vec![ForgeAdminCLI, Flow]);
        x.perm(
            "GetAllExpectedMachines",
            vec![ForgeAdminCLI, SiteAgent, Flow],
        );
        x.perm("ReplaceAllExpectedMachines", vec![ForgeAdminCLI]);
        x.perm("DeleteAllExpectedMachines", vec![ForgeAdminCLI]);
        x.perm(
            "GetAllExpectedMachinesLinked",
            vec![ForgeAdminCLI, SiteAgent, Flow],
        );
        x.perm(
            "GetAllUnexpectedMachines",
            vec![ForgeAdminCLI, SiteAgent, Flow],
        );
        x.perm("AttestQuote", vec![Anonymous]);
        x.perm("SignMachineIdentity", vec![Agent]);
        x.perm(
            "GetTenantIdentityConfiguration",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "SetTenantIdentityConfiguration",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "DeleteTenantIdentityConfiguration",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("GetTokenDelegation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("SetTokenDelegation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteTokenDelegation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ReencryptTenantIdentitySecrets", vec![ForgeAdminCLI]);
        x.perm("GetJWKS", vec![Anonymous, Agent, ForgeAdminCLI, SiteAgent]);
        x.perm(
            "GetOpenIDConfiguration",
            vec![Anonymous, Agent, ForgeAdminCLI, SiteAgent],
        );
        x.perm("CreateMeasurementBundle", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteMeasurementBundle", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("RenameMeasurementBundle", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateMeasurementBundle", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ShowMeasurementBundle", vec![ForgeAdminCLI]);
        x.perm("ShowMeasurementBundles", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementBundles", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementBundleMachines", vec![ForgeAdminCLI]);
        x.perm("FindClosestBundleMatch", vec![ForgeAdminCLI]);
        x.perm("DeleteMeasurementJournal", vec![ForgeAdminCLI]);
        x.perm("ShowMeasurementJournal", vec![ForgeAdminCLI]);
        x.perm("ShowMeasurementJournals", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementJournal", vec![ForgeAdminCLI]);
        x.perm("AttestCandidateMachine", vec![ForgeAdminCLI]);
        x.perm("ShowCandidateMachine", vec![ForgeAdminCLI]);
        x.perm("ShowCandidateMachines", vec![ForgeAdminCLI]);
        x.perm("ListCandidateMachines", vec![ForgeAdminCLI]);
        x.perm(
            "CreateMeasurementSystemProfile",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "DeleteMeasurementSystemProfile",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "RenameMeasurementSystemProfile",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("ShowMeasurementSystemProfile", vec![ForgeAdminCLI]);
        x.perm("ShowMeasurementSystemProfiles", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementSystemProfiles", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementSystemProfileBundles", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementSystemProfileMachines", vec![ForgeAdminCLI]);
        x.perm("CreateMeasurementReport", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteMeasurementReport", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("PromoteMeasurementReport", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("RevokeMeasurementReport", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ShowMeasurementReportForId", vec![ForgeAdminCLI]);
        x.perm("ShowMeasurementReportsForMachine", vec![ForgeAdminCLI]);
        x.perm("ShowMeasurementReports", vec![ForgeAdminCLI]);
        x.perm("ListMeasurementReport", vec![ForgeAdminCLI]);
        x.perm("MatchMeasurementReport", vec![ForgeAdminCLI]);
        x.perm("ImportSiteMeasurements", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ExportSiteMeasurements", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "AddMeasurementTrustedMachine",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "RemoveMeasurementTrustedMachine",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "AddMeasurementTrustedProfile",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "RemoveMeasurementTrustedProfile",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "ListMeasurementTrustedMachines",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "ListMeasurementTrustedProfiles",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("ListAttestationSummary", vec![SiteAgent]);
        x.perm("ImportStorageCluster", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteStorageCluster", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ListStorageCluster", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetStorageCluster", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateStorageCluster", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateStoragePool", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteStoragePool", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ListStoragePool", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetStoragePool", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateStoragePool", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateStorageVolume", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteStorageVolume", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ListStorageVolume", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetStorageVolume", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateStorageVolume", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateOsImage", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteOsImage", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ListOsImage", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetOsImage", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateOsImage", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateOperatingSystem", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetOperatingSystem", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateOperatingSystem", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteOperatingSystem", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindOperatingSystemIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindOperatingSystemsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "GetOperatingSystemCachableIpxeTemplateArtifacts",
            vec![ForgeAdminCLI],
        );
        x.perm(
            "UpdateOperatingSystemCachableIpxeTemplateArtifacts",
            vec![ForgeAdminCLI],
        );
        x.perm("ReWrapSecrets", vec![ForgeAdminCLI]);
        x.perm("GetIpxeTemplate", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ListIpxeTemplates", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindRackStateHistories", vec![ForgeAdminCLI, Machineatron]);
        x.perm("FindRackHealthHistories", vec![ForgeAdminCLI, Machineatron]);
        x.perm("RebootCompleted", vec![Machineatron, Scout]);
        x.perm("PersistValidationResult", vec![Scout, SiteAgent]);
        x.perm(
            "GetMachineValidationResults",
            vec![ForgeAdminCLI, Scout, SiteAgent],
        );
        x.perm("MachineValidationCompleted", vec![Machineatron, Scout]);
        x.perm("MachineSetAutoUpdate", vec![ForgeAdminCLI, Flow]);
        x.perm(
            "GetMachineValidationExternalConfig",
            vec![ForgeAdminCLI, Scout],
        );
        x.perm("GetContainerRegistryCredential", vec![ForgeAdminCLI, Scout]);
        x.perm("SetContainerRegistryCredential", vec![ForgeAdminCLI]);
        x.perm(
            "AddUpdateMachineValidationExternalConfig",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("GetMachineValidationRuns", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindMachineValidationRunItemIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "FindMachineValidationRunItemsByIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "GetMachineValidationAttempt",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("HeartbeatMachineValidationRun", vec![Scout, SiteAgent]);
        x.perm("AdminBmcReset", vec![ForgeAdminCLI]);
        x.perm("AdminPowerControl", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm("AdminGpuReset", vec![ForgeAdminCLI, Flow]);
        x.perm("DisableSecureBoot", vec![ForgeAdminCLI]);
        x.perm("MachineSetup", vec![ForgeAdminCLI]);
        x.perm("SetDpuFirstBootOrder", vec![ForgeAdminCLI]);
        x.perm("OnDemandMachineValidation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("OnDemandRackMaintenance", vec![ForgeAdminCLI]);
        x.perm("TerminateRackMaintenance", vec![ForgeAdminCLI]);
        x.perm("TpmAddCaCert", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("TpmShowCaCerts", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("TpmShowUnmatchedEkCerts", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("TpmDeleteCaCert", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("RedfishListActions", vec![ForgeAdminCLI]);
        x.perm("RedfishCreateAction", vec![ForgeAdminCLI]);
        x.perm("RedfishApproveAction", vec![ForgeAdminCLI]);
        x.perm("RedfishApplyAction", vec![ForgeAdminCLI]);
        x.perm("RedfishCancelAction", vec![ForgeAdminCLI]);
        x.perm("FindTenantOrganizationIds", vec![SiteAgent, ForgeAdminCLI]);
        x.perm(
            "FindTenantsByOrganizationIds",
            vec![SiteAgent, ForgeAdminCLI],
        );
        x.perm("FindMacAddressByBmcIp", vec![SiteAgent, BmcProxy]);
        x.perm("FindBmcIps", vec![ForgeAdminCLI, BmcProxy]);
        x.perm("BmcCredentialStatus", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "GetMachineValidationExternalConfigs",
            vec![ForgeAdminCLI, Scout, SiteAgent],
        );
        x.perm(
            "RemoveMachineValidationExternalConfig",
            vec![ForgeAdminCLI, Scout, SiteAgent],
        );
        x.perm(
            "GetMachineValidationTests",
            vec![ForgeAdminCLI, SiteAgent, Agent, Scout],
        );
        x.perm("AddMachineValidationTest", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "UpdateMachineValidationTest",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "MachineValidationTestVerfied",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "MachineValidationTestNextVersion",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "MachineValidationTestEnableDisableTest",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "MachineValidationTestApproveFullHost",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("UpdateMachineValidationRun", vec![Scout, SiteAgent]);
        x.perm("FindInstanceTypeIds", vec![SiteAgent, ForgeAdminCLI]);
        x.perm("FindInstanceTypesByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateInstanceType", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateInstanceType", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteInstanceType", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "AssociateMachinesWithInstanceType",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "RemoveMachineInstanceTypeAssociation",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("RedfishBrowse", vec![ForgeAdminCLI]);
        x.perm("UfmBrowse", vec![ForgeAdminCLI]);
        x.perm("NmxcBrowse", vec![ForgeAdminCLI]);
        x.perm("UpdateMachineMetadata", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateRackMetadata", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateSwitchMetadata", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdatePowerShelfMetadata", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateNetworkSecurityGroup", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindNetworkSecurityGroupIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "FindNetworkSecurityGroupsByIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("UpdateNetworkSecurityGroup", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteNetworkSecurityGroup", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "GetNetworkSecurityGroupPropagationStatus",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "GetNetworkSecurityGroupAttachments",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "GetDesiredFirmwareVersions",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm("UpsertHostFirmwareConfig", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteHostFirmwareConfig", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CreateSku", vec![ForgeAdminCLI]);
        x.perm("GenerateSkuFromMachine", vec![ForgeAdminCLI]);
        x.perm("AssignSkuToMachine", vec![ForgeAdminCLI]);
        x.perm("VerifySkuForMachine", vec![ForgeAdminCLI]);
        x.perm("RemoveSkuAssociation", vec![ForgeAdminCLI]);
        x.perm("GetAllSkuIds", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm(
            "FindSkusByIds",
            vec![ForgeAdminCLI, Health, SiteAgent, Flow],
        );
        x.perm("DeleteSku", vec![ForgeAdminCLI]);
        x.perm("UpdateSkuMetadata", vec![ForgeAdminCLI]);
        x.perm("UpdateMachineHardwareInfo", vec![ForgeAdminCLI]);
        x.perm("ReplaceSku", vec![ForgeAdminCLI]);
        x.perm(
            "GetManagedHostQuarantineState",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "SetManagedHostQuarantineState",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "ClearManagedHostQuarantineState",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("CreateVpcPeering", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindVpcPeeringIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindVpcPeeringsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteVpcPeering", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ResetHostReprovisioning", vec![ForgeAdminCLI, Flow]);
        x.perm("CopyBfbToDpuRshim", vec![ForgeAdminCLI]);
        x.perm("GetPowerOptions", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm("UpdatePowerOption", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm("CreateBmcUser", vec![ForgeAdminCLI]);
        x.perm("DeleteBmcUser", vec![ForgeAdminCLI]);
        x.perm("SetBmcRootPassword", vec![ForgeAdminCLI]);
        x.perm("ProbeBmcVendor", vec![ForgeAdminCLI]);
        x.perm("SetFirmwareUpdateTimeWindow", vec![ForgeAdminCLI, Flow]);
        x.perm("ListHostFirmware", vec![ForgeAdminCLI, Flow]);
        x.perm("EnableInfiniteBoot", vec![ForgeAdminCLI]);
        x.perm("IsInfiniteBootEnabled", vec![ForgeAdminCLI]);
        x.perm("Lockdown", vec![ForgeAdminCLI]);
        x.perm("LockdownStatus", vec![ForgeAdminCLI]);
        x.perm(
            "PublishMlxDeviceReport",
            vec![Agent, Scout, Machineatron, ForgeAdminCLI],
        );
        x.perm(
            "PublishMlxObservationReport",
            vec![Agent, Scout, Machineatron, ForgeAdminCLI],
        );
        x.perm("TrimTable", vec![ForgeAdminCLI, MaintenanceJobs]);
        x.perm("ListNvlinkNmxcEndpoints", vec![ForgeAdminCLI]);
        x.perm("CreateNvlinkNmxcEndpoint", vec![ForgeAdminCLI]);
        x.perm("UpdateNvlinkNmxcEndpoint", vec![ForgeAdminCLI]);
        x.perm("DeleteNvlinkNmxcEndpoint", vec![ForgeAdminCLI]);
        x.perm("CreateRemediation", vec![ForgeAdminCLI]);
        x.perm("ApproveRemediation", vec![ForgeAdminCLI]);
        x.perm("RevokeRemediation", vec![ForgeAdminCLI]);
        x.perm("EnableRemediation", vec![ForgeAdminCLI]);
        x.perm("DisableRemediation", vec![ForgeAdminCLI]);
        x.perm("FindRemediationIds", vec![ForgeAdminCLI]);
        x.perm("FindRemediationsByIds", vec![ForgeAdminCLI]);
        x.perm("FindAppliedRemediations", vec![ForgeAdminCLI]);
        x.perm("FindAppliedRemediationIds", vec![ForgeAdminCLI]);
        x.perm("GetNextRemediationForMachine", vec![Agent]);
        x.perm("RemediationApplied", vec![Agent]);
        x.perm("DetermineMachineIngestionState", vec![ForgeAdminCLI, Flow]);
        x.perm("AllowIngestionAndPowerOn", vec![ForgeAdminCLI, Flow]);
        x.perm("SetPrimaryDpu", vec![ForgeAdminCLI]);
        x.perm("SetPrimaryInterface", vec![ForgeAdminCLI]);
        x.perm("CreateDpuExtensionService", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("UpdateDpuExtensionService", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteDpuExtensionService", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindDpuExtensionServiceIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindDpuExtensionServicesByIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "GetDpuExtensionServiceVersionsInfo",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "FindInstancesByDpuExtensionService",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("TriggerMachineAttestation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("CancelMachineAttestation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ListAttestationMachines", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("GetAttestationMachine", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindPowerShelves", vec![ForgeAdminCLI, Machineatron, Flow]);
        x.perm("FindPowerShelfIds", vec![ForgeAdminCLI, Machineatron, Flow]);
        x.perm(
            "FindPowerShelvesByIds",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm("CreatePowerShelf", vec![ForgeAdminCLI, Machineatron]);
        x.perm(
            "DecommissionPowerShelf",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm("DeletePowerShelf", vec![ForgeAdminCLI, Machineatron]);
        x.perm(
            "AddExpectedPowerShelf",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "DeleteExpectedPowerShelf",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "UpdateExpectedPowerShelf",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "GetExpectedPowerShelf",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm(
            "GetAllExpectedPowerShelves",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "ReplaceAllExpectedPowerShelves",
            vec![ForgeAdminCLI, Machineatron],
        );
        x.perm(
            "DeleteAllExpectedPowerShelves",
            vec![ForgeAdminCLI, Machineatron],
        );
        x.perm(
            "GetAllExpectedPowerShelvesLinked",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "FindPowerShelfStateHistories",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm(
            "FindPowerShelfHealthHistories",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm(
            "SetPowerShelfMaintenance",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm(
            "FindSwitches",
            vec![ForgeAdminCLI, Machineatron, Flow, Health],
        );
        x.perm(
            "FindSwitchIds",
            vec![ForgeAdminCLI, Machineatron, Flow, Health],
        );
        x.perm(
            "FindSwitchesByIds",
            vec![ForgeAdminCLI, Machineatron, Flow, Health],
        );
        x.perm("CreateSwitch", vec![ForgeAdminCLI, Machineatron]);
        x.perm("DeleteSwitch", vec![ForgeAdminCLI, Machineatron]);
        x.perm(
            "AddExpectedSwitch",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "DeleteExpectedSwitch",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "UpdateExpectedSwitch",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm("GetExpectedSwitch", vec![ForgeAdminCLI, Machineatron, Flow]);
        x.perm(
            "GetAllExpectedSwitches",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "ReplaceAllExpectedSwitches",
            vec![ForgeAdminCLI, Machineatron],
        );
        x.perm(
            "DeleteAllExpectedSwitches",
            vec![ForgeAdminCLI, Machineatron],
        );
        x.perm(
            "GetAllExpectedSwitchesLinked",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "AddExpectedRack",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "DeleteExpectedRack",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "UpdateExpectedRack",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "GetExpectedRack",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "GetAllExpectedRacks",
            vec![ForgeAdminCLI, Machineatron, SiteAgent, Flow],
        );
        x.perm(
            "ReplaceAllExpectedRacks",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "DeleteAllExpectedRacks",
            vec![ForgeAdminCLI, Machineatron, SiteAgent],
        );
        x.perm(
            "FindSwitchStateHistories",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm(
            "FindSwitchHealthHistories",
            vec![ForgeAdminCLI, Machineatron, Flow],
        );
        x.perm("FindRackIds", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm("FindRacksByIds", vec![ForgeAdminCLI, SiteAgent, Flow]);
        x.perm("GetRack", vec![ForgeAdminCLI, Flow]);
        x.perm("DeleteRack", vec![ForgeAdminCLI, Flow]);
        x.perm("GetRackProfile", vec![ForgeAdminCLI]);
        x.perm("ListRackProfiles", vec![ForgeAdminCLI]);
        x.perm("RackManagerCall", vec![ForgeAdminCLI]);
        x.perm("ScoutStream", vec![Scout]);
        x.perm("ScoutStreamShowConnections", vec![ForgeAdminCLI]);
        x.perm("ScoutStreamDisconnect", vec![ForgeAdminCLI]);
        x.perm("ScoutStreamPing", vec![ForgeAdminCLI]);
        x.perm("MlxAdminProfileSync", vec![ForgeAdminCLI]);
        x.perm("MlxAdminProfileShow", vec![ForgeAdminCLI]);
        x.perm("MlxAdminProfileCompare", vec![ForgeAdminCLI]);
        x.perm("MlxAdminProfileList", vec![ForgeAdminCLI]);
        x.perm("MlxAdminLockdownLock", vec![ForgeAdminCLI]);
        x.perm("MlxAdminLockdownUnlock", vec![ForgeAdminCLI]);
        x.perm("MlxAdminLockdownStatus", vec![ForgeAdminCLI]);
        x.perm("MlxAdminShowDevice", vec![ForgeAdminCLI]);
        x.perm("MlxAdminShowMachine", vec![ForgeAdminCLI]);
        x.perm("MlxAdminRegistryList", vec![ForgeAdminCLI]);
        x.perm("MlxAdminRegistryShow", vec![ForgeAdminCLI]);
        x.perm("MlxAdminConfigQuery", vec![ForgeAdminCLI]);
        x.perm("MlxAdminConfigSet", vec![ForgeAdminCLI]);
        x.perm("MlxAdminConfigSync", vec![ForgeAdminCLI]);
        x.perm("MlxAdminConfigCompare", vec![ForgeAdminCLI]);
        x.perm("CreateSpxPartition", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteSpxPartition", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindSpxPartitionIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindSpxPartitionsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindNVLinkPartitionIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindNVLinkPartitionsByIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("NVLinkPartitionsForTenant", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindNVLinkLogicalPartitionIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "FindNVLinkLogicalPartitionsByIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "CreateNVLinkLogicalPartition",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "UpdateNVLinkLogicalPartition",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "DeleteNVLinkLogicalPartition",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "NVLinkLogicalPartitionsForTenant",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm(
            "GetMachinePositionInfo",
            vec![ForgeAdminCLI, SiteAgent, Flow],
        );
        x.perm("ModifyDPFState", vec![ForgeAdminCLI]);
        x.perm("GetDPFState", vec![ForgeAdminCLI]);
        x.perm("UpdateMachineNvLinkInfo", vec![ForgeAdminCLI]);
        x.perm("CreateComputeAllocation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("FindComputeAllocationIds", vec![ForgeAdminCLI, SiteAgent]);
        x.perm(
            "FindComputeAllocationsByIds",
            vec![ForgeAdminCLI, SiteAgent],
        );
        x.perm("UpdateComputeAllocation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("DeleteComputeAllocation", vec![ForgeAdminCLI, SiteAgent]);
        x.perm("ComponentPowerControl", vec![ForgeAdminCLI, Flow]);
        x.perm("GetComponentInventory", vec![ForgeAdminCLI, Flow]);
        x.perm("UpdateComponentFirmware", vec![ForgeAdminCLI, Flow]);
        x.perm(
            "ComponentConfigureSwitchCertificate",
            vec![ForgeAdminCLI, Flow],
        );
        x.perm("GetComponentFirmwareStatus", vec![ForgeAdminCLI, Flow]);
        x.perm("ListComponentFirmwareVersions", vec![ForgeAdminCLI, Flow]);
        x.perm("GetDPFHostSnapshot", vec![ForgeAdminCLI]);
        x.perm("GetDPFServiceVersions", vec![ForgeAdminCLI]);
        x.perm("FindPendingDPUServiceSyncIds", vec![ForgeAdminCLI]);
        x.perm("FindPendingDPUServiceSyncsByIds", vec![ForgeAdminCLI]);
        x.perm("ListDPUServiceSyncHistory", vec![ForgeAdminCLI]);
        // Operator-only: releasing a hold restarts DPU services, and for an
        // assigned host it disrupts a tenant. No service identity should be able
        // to ask for that on its own.
        x.perm("ReleaseDPUServiceSyncHold", vec![ForgeAdminCLI]);
        x
    }
    fn perm(&mut self, msg: &str, principals: Vec<RulePrincipal>) {
        self.perms
            .insert(msg.to_string(), RuleInfo::new(principals));
    }

    pub(super) fn allowed_from_static(
        msg: &str,
        user_principals: &[crate::auth::Principal],
    ) -> bool {
        INTERNAL_RBAC_RULES.allowed(msg, user_principals)
    }

    pub(super) fn allowed(&self, msg: &str, user_principals: &[crate::auth::Principal]) -> bool {
        if let Some(perm_info) = self.perms.get(msg) {
            if user_principals.is_empty() {
                // No proper cert presented, but we allow any rule that lists Anonymous.
                return perm_info.principals.contains(&Principal::Anonymous);
            }
            user_principals.iter().any(|user_principal| {
                perm_info
                    .principals
                    .iter()
                    .any(|perm_principal| user_principal.is_proper_subset_of(perm_principal))
            })
        } else {
            false
        }
    }
}

impl Default for InternalRBACRules {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct RuleInfo {
    principals: Vec<crate::auth::Principal>,
}

impl RuleInfo {
    fn new(principals: Vec<RulePrincipal>) -> Self {
        // Helper: emit both the nico-* and carbide-* SPIFFE service identifiers
        // for a renamed service. The matcher in `allowed()` walks this Vec with
        // `.any(...)`, so any cert presenting either string is accepted. Drop
        // the carbide-* alias once every deployed site has rotated to a cert
        // with the nico-* identifier.
        let svc_compat = |nico_name: &str, carbide_name: &str| {
            vec![
                Principal::SpiffeServiceIdentifier(nico_name.to_string()),
                Principal::SpiffeServiceIdentifier(carbide_name.to_string()),
            ]
        };
        Self {
            principals: principals
                .iter()
                .flat_map(|x| match *x {
                    RulePrincipal::ForgeAdminCLI => {
                        // The group is NOT compared: `is_proper_subset_of`
                        // matches any ExternalUser against any ExternalUser,
                        // so this value is documentation-only. What actually
                        // gates admin-CLI access is the client cert mapping to
                        // an ExternalUser at all (issuer CN listed in
                        // `auth.additional_issuer_cns`). Keep the value in
                        // sync with the helm-prereqs `nicoCliClientRole` OU so
                        // audit logs read sensibly; its previous placeholder
                        // ("Invalid") leaked into chart defaults as if it were
                        // load-bearing (issue #3662).
                        vec![Principal::ExternalUser(ExternalUserInfo::new(
                            None,
                            "nico-cli-client".to_string(),
                            None,
                        ))]
                    }
                    RulePrincipal::Machineatron => vec![Principal::SpiffeServiceIdentifier(
                        "machine-a-tron".to_string(),
                    )],
                    RulePrincipal::SiteAgent => vec![Principal::SpiffeServiceIdentifier(
                        "elektra-site-agent".to_string(),
                    )],
                    RulePrincipal::Agent => {
                        vec![Principal::SpiffeMachineIdentifier("".to_string())]
                    }
                    RulePrincipal::Scout => {
                        vec![Principal::SpiffeMachineIdentifier("".to_string())]
                    }
                    RulePrincipal::Dns => svc_compat("nico-dns", "carbide-dns"),
                    RulePrincipal::Dhcp => svc_compat("nico-dhcp", "carbide-dhcp"),
                    RulePrincipal::Ssh => svc_compat("nico-ssh-console", "carbide-ssh-console"),
                    RulePrincipal::SshRs => {
                        svc_compat("nico-ssh-console-rs", "carbide-ssh-console-rs")
                    }
                    RulePrincipal::Pxe => svc_compat("nico-pxe", "carbide-pxe"),
                    RulePrincipal::BmcProxy => svc_compat("nico-bmc-proxy", "carbide-bmc-proxy"),
                    RulePrincipal::Health => {
                        svc_compat("nico-hardware-health", "carbide-hardware-health")
                    }
                    RulePrincipal::Flow => svc_compat("nico-flow", "carbide-flow"),
                    RulePrincipal::MaintenanceJobs => {
                        svc_compat("nico-maintenance-jobs", "carbide-maintenance-jobs")
                    }
                    RulePrincipal::DsxExchangeConsumer => svc_compat(
                        "nico-dsx-exchange-consumer",
                        "carbide-dsx-exchange-consumer",
                    ),
                    RulePrincipal::Anonymous => vec![Principal::Anonymous],
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod rbac_rule_tests {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    use super::*;
    use crate::auth::Principal;

    fn ensure_identical_permissions(princ_a: &Principal, princ_b: &Principal) {
        for (rule_name, rule) in &INTERNAL_RBAC_RULES.perms {
            if rule.principals.contains(princ_a) {
                assert!(
                    rule.principals.contains(princ_b),
                    "{} RBAC rule allows {} but not {}",
                    rule_name,
                    princ_a.as_identifier(),
                    princ_b.as_identifier(),
                );
            } else {
                assert!(
                    !rule.principals.contains(princ_b),
                    "{} RBAC rule rejects {} but allows {}",
                    rule_name,
                    princ_a.as_identifier(),
                    princ_b.as_identifier(),
                );
            }

            if rule.principals.contains(princ_b) {
                assert!(
                    rule.principals.contains(princ_a),
                    "{} RBAC rule allows {} but not {}",
                    rule_name,
                    princ_b.as_identifier(),
                    princ_a.as_identifier(),
                );
            } else {
                assert!(
                    !rule.principals.contains(princ_a),
                    "{} RBAC rule rejects {} but allows {}",
                    rule_name,
                    princ_b.as_identifier(),
                    princ_a.as_identifier(),
                );
            }
        }
    }

    #[test]
    fn admin_cli_can_create_network_segments() {
        assert!(InternalRBACRules::allowed_from_static(
            "CreateNetworkSegment",
            &[Principal::ExternalUser(ExternalUserInfo::new(
                None,
                "nico-admin-cli".to_string(),
                None,
            ))],
        ));
    }

    #[test]
    fn site_agent_can_manage_domains() {
        let site_agent = Principal::SpiffeServiceIdentifier("elektra-site-agent".to_string());
        let unrelated_service = Principal::SpiffeServiceIdentifier("nico-dns".to_string());

        for method in ["CreateDomain", "UpdateDomain", "DeleteDomain"] {
            assert!(
                InternalRBACRules::allowed_from_static(method, std::slice::from_ref(&site_agent)),
                "{method} should allow the site agent"
            );
            assert!(
                !InternalRBACRules::allowed_from_static(
                    method,
                    std::slice::from_ref(&unrelated_service)
                ),
                "{method} should reject unrelated services"
            );
        }
    }

    #[test]
    fn supported_callers_can_attach_network_segments_to_vpcs() {
        let method = "AttachNetworkSegmentToVpc";
        assert!(InternalRBACRules::allowed_from_static(
            method,
            &[Principal::ExternalUser(ExternalUserInfo::new(
                None,
                "nico-admin-cli".to_string(),
                None,
            ))],
        ));
        assert!(InternalRBACRules::allowed_from_static(
            method,
            &[Principal::SpiffeServiceIdentifier(
                "elektra-site-agent".to_string()
            )],
        ));
        for unrelated in [
            Principal::SpiffeServiceIdentifier("nico-dns".to_string()),
            Principal::SpiffeServiceIdentifier("machine-a-tron".to_string()),
        ] {
            assert!(
                !InternalRBACRules::allowed_from_static(method, std::slice::from_ref(&unrelated)),
                "{method} should reject {}",
                unrelated.as_identifier(),
            );
        }
    }

    #[test]
    fn anonymous_rules_allow_certless_callers() {
        // Certless callers must be allowed when a rule lists Anonymous among other principals.
        for method in ["GetJWKS", "GetOpenIDConfiguration"] {
            assert!(
                InternalRBACRules::allowed_from_static(method, &[]),
                "{method}"
            );
        }
    }

    #[test]
    fn rbac_rule_tests() -> Result<(), eyre::Report> {
        assert!(InternalRBACRules::allowed_from_static(
            "Version",
            &[Principal::TrustedCertificate]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "GetStoragePool",
            &[Principal::ExternalUser(ExternalUserInfo::new(
                None,
                "any".to_string(),
                None
            ))]
        ));
        assert!(!InternalRBACRules::allowed_from_static(
            "GetStoragePool",
            &[Principal::SpiffeMachineIdentifier("foo".to_string())]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "ReportForgeScoutError",
            &[Principal::SpiffeMachineIdentifier("foo".to_string())]
        ));
        // A machine reporting a pre-registration failure presents no certificate,
        // so it arrives with no principals at all. That case is the reason this
        // RPC is public.
        assert!(InternalRBACRules::allowed_from_static(
            "ReportForgeScoutError",
            &[]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "GetCloudInitInstructions",
            &[Principal::SpiffeServiceIdentifier("nico-pxe".to_string())]
        ));
        assert!(!InternalRBACRules::allowed_from_static(
            "GetCloudInitInstructions",
            &[Principal::SpiffeServiceIdentifier("nico-dns".to_string())]
        ));
        assert!(!InternalRBACRules::allowed_from_static(
            "GetCloudInitInstructions",
            &[Principal::ExternalUser(ExternalUserInfo::new(
                None,
                "any".to_string(),
                None
            ))]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "CreateVpc",
            &[Principal::SpiffeServiceIdentifier(
                "machine-a-tron".to_string()
            )]
        ));
        assert!(!InternalRBACRules::allowed_from_static(
            "CreateVpc",
            &[Principal::SpiffeServiceIdentifier("nico-dns".to_string())]
        ));

        assert!(InternalRBACRules::allowed_from_static(
            "CreateTenantKeyset",
            &[Principal::SpiffeServiceIdentifier(
                "elektra-site-agent".to_string()
            )]
        ));

        // REST admin operations proxy to Core as the site agent (issue #4597).
        for method in ["AdminPowerControl", "TriggerDpuReprovisioning"] {
            assert!(
                InternalRBACRules::allowed_from_static(
                    method,
                    &[Principal::SpiffeServiceIdentifier(
                        "elektra-site-agent".to_string()
                    )]
                ),
                "{method} should allow the site agent"
            );
            assert!(
                !InternalRBACRules::allowed_from_static(
                    method,
                    &[Principal::SpiffeServiceIdentifier("nico-dns".to_string())]
                ),
                "{method} should reject unrelated services"
            );
        }

        assert!(InternalRBACRules::allowed_from_static(
            "OnDemandMachineValidation",
            &[Principal::SpiffeServiceIdentifier(
                "elektra-site-agent".to_string()
            )]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "FindNetworkSegmentsByIds",
            &[
                Principal::SpiffeServiceIdentifier("machine-a-tron".to_string()),
                Principal::TrustedCertificate
            ]
        ));

        assert!(InternalRBACRules::allowed_from_static(
            "DiscoverMachine",
            &[]
        ));

        assert!(InternalRBACRules::allowed_from_static(
            "TrimTable",
            &[Principal::SpiffeServiceIdentifier(
                "nico-maintenance-jobs".to_string()
            )]
        ));

        for method in [
            "GetAllExpectedSwitches",
            "GetAllExpectedSwitchesLinked",
            "GetAllExpectedPowerShelves",
            "GetAllExpectedPowerShelvesLinked",
        ] {
            assert!(
                InternalRBACRules::allowed_from_static(
                    method,
                    &[Principal::SpiffeServiceIdentifier(
                        "elektra-site-agent".to_string()
                    )]
                ),
                "{method} should allow SiteAgent"
            );
        }

        for method in [
            "ClearSiteExplorationError",
            "ReExploreEndpoint",
            "FindExploredEndpointIds",
            "FindExploredEndpointsByIds",
        ] {
            assert!(
                InternalRBACRules::allowed_from_static(
                    method,
                    &[Principal::SpiffeServiceIdentifier(
                        "elektra-site-agent".to_string()
                    )]
                ),
                "{method} should allow SiteAgent"
            );
        }

        assert!(InternalRBACRules::allowed_from_static(
            "SetMaintenance",
            &[Principal::SpiffeServiceIdentifier("nico-flow".to_string())]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "InsertMachineHealthReport",
            &[Principal::SpiffeServiceIdentifier("nico-flow".to_string())]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "RemoveMachineHealthReport",
            &[Principal::SpiffeServiceIdentifier("nico-flow".to_string())]
        ));
        assert!(InternalRBACRules::allowed_from_static(
            "MachineSetAutoUpdate",
            &[Principal::SpiffeServiceIdentifier("nico-flow".to_string())]
        ));
        for method in ["FindMacAddressByBmcIp", "GetBmcCredentials"] {
            assert!(
                InternalRBACRules::allowed_from_static(
                    method,
                    &[Principal::SpiffeServiceIdentifier(
                        "nico-bmc-proxy".to_string()
                    )]
                ),
                "{method} should allow bmc-proxy"
            );
        }

        // Ensure Ssh and SshRs both have identical permissions. (ssh-console-rs is a rust rewrite
        // of ssh-console, and to keep things straightforward, it has its own set of DNS names,
        // SPIFFE identifiers, etc. We don't want to play any tricks by reusing principals here, so
        // we gotta list both, until we've fully migrated to ssh-console-rs.)
        ensure_identical_permissions(
            &Principal::SpiffeServiceIdentifier("nico-ssh-console".to_string()),
            &Principal::SpiffeServiceIdentifier("nico-ssh-console-rs".to_string()),
        );

        // Backward-compat: every renamed service's carbide-* SPIFFE identifier
        // must have *identical* permissions to its nico-* counterpart across
        // every rule. RuleInfo::new emits both names side-by-side; this guards
        // against accidental skew while we keep accepting carbide-*. Drop this
        // block (and the svc_compat() carbide-* entries) once every deployed
        // site has rotated to a nico-* cert.
        for (nico, carbide) in [
            ("nico-dns", "carbide-dns"),
            ("nico-dhcp", "carbide-dhcp"),
            ("nico-ssh-console", "carbide-ssh-console"),
            ("nico-ssh-console-rs", "carbide-ssh-console-rs"),
            ("nico-pxe", "carbide-pxe"),
            ("nico-bmc-proxy", "carbide-bmc-proxy"),
            ("nico-hardware-health", "carbide-hardware-health"),
            ("nico-flow", "carbide-flow"),
            ("nico-maintenance-jobs", "carbide-maintenance-jobs"),
            (
                "nico-dsx-exchange-consumer",
                "carbide-dsx-exchange-consumer",
            ),
        ] {
            ensure_identical_permissions(
                &Principal::SpiffeServiceIdentifier(nico.to_string()),
                &Principal::SpiffeServiceIdentifier(carbide.to_string()),
            );
        }

        Ok(())
    }

    #[test]
    fn all_requests_listed() -> Result<(), eyre::Report> {
        let mut messages = vec![];
        let proto = File::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../rpc/proto/forge.proto"),
        )?;
        let reader = BufReader::new(proto);
        for line in reader.lines() {
            let line = line?;
            let line = line.trim();
            if line.starts_with("rpc") {
                let mut name = line.strip_prefix("rpc").unwrap_or("why").trim().to_string();
                let offset = name.find("(").unwrap_or(name.len());
                name.replace_range(offset.., "");
                messages.push(name.trim().to_string());
            }
        }
        if messages.is_empty() {
            panic!("Parsing failed, no messages found")
        }
        let rules = InternalRBACRules::new();
        let mut missing = vec![];
        for msg in messages {
            if !rules.perms.contains_key(&msg) {
                missing.push(msg);
            }
        }
        if !missing.is_empty() {
            panic!("GRPC messages missing RBAC permissions: {missing:?}");
        }
        Ok(())
    }
}
