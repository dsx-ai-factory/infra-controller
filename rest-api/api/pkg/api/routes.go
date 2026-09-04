// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package api

import (
	"net/http"

	tClient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	apiHandler "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
)

// NewAPIRoutes returns all API routes
func NewAPIRoutes(dbSession *cdb.Session, tc tClient.Client, tnc tClient.NamespaceClient, scp *sc.ClientPool, cfg *config.Config, dpsc dpsclient.PowerProvisioner) []Route {
	apiName := cfg.GetAPIName()

	apiPathPrefix := "/org/:orgName/" + apiName

	apiRoutes := []Route{
		// Metadata endpoint
		{
			Path:    apiPathPrefix + "/metadata",
			Method:  http.MethodGet,
			Handler: apiHandler.NewMetadataHandler(),
		},
		// BMC credential endpoint (Provider Admin). First operation migrated to
		// the generic NICo Core gRPC proxy; equivalent to the admin CLI
		// `credential add-bmc` command.
		{
			Path:    apiPathPrefix + "/credential/bmc",
			Method:  http.MethodPut,
			Handler: apiHandler.NewCreateOrUpdateBMCCredentialHandler(dbSession, scp, cfg),
		},
		// Site Explorer explored endpoints (Provider Admin). Lists Core explored
		// endpoints through FindExploredEndpointIds + FindExploredEndpointsByIds.
		{
			Path:    apiPathPrefix + "/site-explorer/endpoint",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllExploredEndpointHandler(dbSession, scp, cfg),
		},
		// Site Explorer endpoint actions (Provider Admin). Composes existing
		// single-endpoint Core methods through the generic gRPC proxy.
		{
			Path:    apiPathPrefix + "/site-explorer/endpoint/action",
			Method:  http.MethodPost,
			Handler: apiHandler.NewSiteExplorerEndpointActionHandler(dbSession, scp, cfg),
		},
		// Site-default UEFI credential endpoint (Provider Admin); equivalent to
		// the admin CLI `credential add-uefi` command.
		{
			Path:    apiPathPrefix + "/credential/uefi",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateUEFICredentialHandler(dbSession, scp),
		},
		// Measured-boot machine and profile trust approvals (Provider Admin).
		{
			Path:    apiPathPrefix + "/measured-boot/trusted-machine",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateMeasuredBootTrustedMachineHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/measured-boot/trusted-machine",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMeasuredBootTrustedMachineHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/measured-boot/trusted-machine/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteMeasuredBootTrustedMachineHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/measured-boot/trusted-profile",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateMeasuredBootTrustedProfileHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/measured-boot/trusted-profile",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMeasuredBootTrustedProfileHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/measured-boot/trusted-profile/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteMeasuredBootTrustedProfileHandler(dbSession, scp),
		},
		// Site-wide credential rotation (Provider Admin); equivalent to the admin
		// CLI `credential rotate` / `credential rotation-status` commands. POST
		// stages a rotation and returns the new target version; GET reports
		// convergence for the site or a single device by MAC.
		{
			Path:    apiPathPrefix + "/credential/rotation",
			Method:  http.MethodPost,
			Handler: apiHandler.NewRotateCredentialHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/credential/rotation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCredentialRotationStatusHandler(dbSession, scp),
		},
		// User endpoint
		{
			Path:    apiPathPrefix + "/user/current",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetUserHandler(dbSession),
		},
		// Service Account endpoint
		{
			Path:    apiPathPrefix + "/service-account/current",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCurrentServiceAccountHandler(dbSession),
		},
		// Infrastructure Provider endpoints
		{
			Path:    apiPathPrefix + "/infrastructure-provider",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateInfrastructureProviderHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/infrastructure-provider/current",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCurrentInfrastructureProviderHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/infrastructure-provider/current",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateCurrentInfrastructureProviderHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/infrastructure-provider/current/stats",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCurrentInfrastructureProviderStatsHandler(dbSession, tc, cfg),
		},
		// Tenant endpoints
		{
			Path:    apiPathPrefix + "/tenant",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateTenantHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/current",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCurrentTenantHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/current",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateCurrentTenantHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/current/stats",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCurrentTenantStatsHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/current/routing-profile",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetCurrentTenantRoutingProfileHandler(dbSession, scp),
		},
		// Tenant Instance Type Stats endpoint
		{
			Path:    apiPathPrefix + "/tenant/instance-type/stats",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTenantInstanceTypeStatsHandler(dbSession, cfg),
		},
		// TenantAccount endpoints
		{
			Path:    apiPathPrefix + "/tenant/account",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllTenantAccountHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/account/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTenantAccountHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/account",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateTenantAccountHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/account/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateTenantAccountHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/tenant/account/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteTenantAccountHandler(dbSession, tc, cfg),
		},
		// Site endpoints
		{
			Path:    apiPathPrefix + "/site",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSiteHandler(dbSession, tc, tnc, cfg),
		},
		{
			Path:    apiPathPrefix + "/site",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllSiteHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSiteHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateSiteHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteSiteHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:id/status-history",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSiteStatusDetailsHandler(dbSession),
		},
		// VPC endpoints
		{
			Path:    apiPathPrefix + "/vpc",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateVPCHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/vpc",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllVPCHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetVPCHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateVPCHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/vpc/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteVPCHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/vpc/:id/virtualization",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateVPCVirtualizationHandler(dbSession, tc, scp, cfg),
		},

		// VpcPrefix endpoints
		{
			Path:    apiPathPrefix + "/vpc-prefix",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateVpcPrefixHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-prefix",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllVpcPrefixHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-prefix/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetVpcPrefixHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-prefix/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateVpcPrefixHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-prefix/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteVpcPrefixHandler(dbSession, tc, scp, cfg),
		},

		// VPC Peering endpoints
		{
			Path:    apiPathPrefix + "/vpc-peering",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateVpcPeeringHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-peering",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllVpcPeeringHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-peering/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetVpcPeeringHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/vpc-peering/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteVpcPeeringHandler(dbSession, tc, scp, cfg),
		},

		// IPBlock endpoints
		{
			Path:    apiPathPrefix + "/ipblock",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateIPBlockHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/ipblock",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllIPBlockHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/ipblock/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetIPBlockHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/ipblock/:id/derived",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllDerivedIPBlockHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/ipblock/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateIPBlockHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/ipblock/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteIPBlockHandler(dbSession, tc, cfg),
		},
		// Instance endpoints
		{
			Path:    apiPathPrefix + "/instance",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateInstanceHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/instance/batch",
			Method:  http.MethodPost,
			Handler: apiHandler.NewBatchCreateInstanceHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/instance",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInstanceHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetInstanceHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateInstanceHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/instance/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteInstanceHandler(dbSession, tc, scp, cfg, dpsc),
		},
		{
			Path:    apiPathPrefix + "/instance/:id/status-history",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetInstanceStatusDetailsHandler(dbSession),
		},
		// Instance Type endpoints
		{
			Path:    apiPathPrefix + "/instance/type",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateInstanceTypeHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/type",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInstanceTypeHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/type/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetInstanceTypeHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/type/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateInstanceTypeHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/type/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteInstanceTypeHandler(dbSession, tc, scp, cfg),
		},
		// Interface endpoints
		{
			Path:    apiPathPrefix + "/instance/:instanceId/interface",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInterfaceHandler(dbSession, tc, cfg),
		},
		// Instance InfiniBandInterface endpoints
		{
			Path:    apiPathPrefix + "/instance/:instanceId/infiniband-interface",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInstanceInfiniBandInterfaceHandler(dbSession, tc, cfg),
		},
		// Instance NVLinkInterface endpoints
		{
			Path:    apiPathPrefix + "/instance/:instanceId/nvlink-interface",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInstanceNVLinkInterfaceHandler(dbSession, tc, cfg),
		},
		// InfiniBandInterface endpoints
		{
			Path:    apiPathPrefix + "/infiniband-interface",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInfiniBandInterfaceHandler(dbSession, tc, cfg, nil),
		},
		// NVLinkInterface endpoints
		{
			Path:    apiPathPrefix + "/nvlink-interface",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllNVLinkInterfaceHandler(dbSession, tc, cfg, nil),
		},
		// InfiniBandPartition endpoints
		{
			Path:    apiPathPrefix + "/infiniband-partition",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateInfiniBandPartitionHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/infiniband-partition",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllInfiniBandPartitionHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/infiniband-partition/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetInfiniBandPartitionHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/infiniband-partition/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateInfiniBandPartitionHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/infiniband-partition/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteInfiniBandPartitionHandler(dbSession, tc, scp, cfg),
		},
		// NVLinkLogicalPartition endpoints
		{
			Path:    apiPathPrefix + "/nvlink-logical-partition",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateNVLinkLogicalPartitionHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/nvlink-logical-partition",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllNVLinkLogicalPartitionHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/nvlink-logical-partition/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetNVLinkLogicalPartitionHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/nvlink-logical-partition/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateNVLinkLogicalPartitionHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/nvlink-logical-partition/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteNVLinkLogicalPartitionHandler(dbSession, tc, scp, cfg),
		},
		// ExpectedMachine endpoints
		{
			Path:    apiPathPrefix + "/expected-machine",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateExpectedMachineHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-machine",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllExpectedMachineHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-machine/batch",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateExpectedMachinesHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-machine/batch",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateExpectedMachinesHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-machine/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetExpectedMachineHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-machine/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateExpectedMachineHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-machine/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteExpectedMachineHandler(dbSession, scp, cfg),
		},
		// ExpectedPowerShelf endpoints
		{
			Path:    apiPathPrefix + "/expected-power-shelf",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateExpectedPowerShelfHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-power-shelf",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllExpectedPowerShelfHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-power-shelf/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetExpectedPowerShelfHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-power-shelf/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateExpectedPowerShelfHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-power-shelf/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteExpectedPowerShelfHandler(dbSession, scp, cfg),
		},
		// ExpectedRack endpoints
		{
			Path:    apiPathPrefix + "/expected-rack",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateExpectedRackHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-rack",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllExpectedRackHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-rack",
			Method:  http.MethodPut,
			Handler: apiHandler.NewReplaceAllExpectedRacksHandler(dbSession, scp, cfg),
		},
		{
			// "all" suffix disambiguates from the path-param Delete handler below.
			Path:    apiPathPrefix + "/expected-rack/all",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteAllExpectedRacksHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-rack/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetExpectedRackHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-rack/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateExpectedRackHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-rack/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteExpectedRackHandler(dbSession, scp, cfg),
		},
		// ExpectedSwitch endpoints
		{
			Path:    apiPathPrefix + "/expected-switch",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateExpectedSwitchHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-switch",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllExpectedSwitchHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-switch/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetExpectedSwitchHandler(dbSession, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-switch/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateExpectedSwitchHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/expected-switch/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteExpectedSwitchHandler(dbSession, scp, cfg),
		},
		// Machine endpoints
		{
			Path:    apiPathPrefix + "/dpu",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllDpuMachinesHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/dpu/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetDpuMachineHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/machine",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateMachineHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteMachineHandler(dbSession, scp),
		},
		{

			Path:    apiPathPrefix + "/machine/:id/status-history",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineStatusDetailsHandler(dbSession),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/dpu",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllDpuMachineHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/dpu/reprovision",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewReprovisionMachineDpuHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/bmc/reset",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewResetMachineBMCHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/health-report",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineHealthReportHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/health-report",
			Method:  http.MethodPut,
			Handler: apiHandler.NewCreateOrUpdateMachineHealthReportHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/health-report/:source",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteMachineHealthReportHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewMachinePowerControlHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/decommission",
			Method:  http.MethodPost,
			Handler: apiHandler.NewDecommissionMachineHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/validation/run",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateMachineValidationRunHandler(dbSession, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/validation/run",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineValidationRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/machine/:id/validation/result",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineValidationResultsHandler(dbSession, tc, scp, cfg),
		},

		// Machine GPU Stats endpoint
		{
			Path:    apiPathPrefix + "/machine/gpu/stats",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineGPUStatsHandler(dbSession, cfg),
		},
		// Machine Instance Type Stats Summary endpoint
		{
			Path:    apiPathPrefix + "/machine/instance-type/stats/summary",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineInstanceTypeSummaryHandler(dbSession, cfg),
		},
		// Machine Instance Type Stats endpoint
		{
			Path:    apiPathPrefix + "/machine/instance-type/stats",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineInstanceTypeStatsHandler(dbSession, cfg),
		},
		// Machine/Instance Type association endpoints
		{
			Path:    apiPathPrefix + "/instance/type/:instanceTypeId/machine",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateMachineInstanceTypeHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/type/:instanceTypeId/machine",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineInstanceTypeHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/instance/type/:instanceTypeId/machine/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteMachineInstanceTypeHandler(dbSession, tc, scp, cfg),
		},
		// Allocation endpoints
		{
			Path:    apiPathPrefix + "/allocation",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateAllocationHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/allocation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllAllocationHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/allocation/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllocationHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/allocation/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateAllocationHandler(dbSession, tc, cfg),
		},
		// AllocationConstraint update endpoint
		{
			Path:    apiPathPrefix + "/allocation/:allocationId/constraint/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateAllocationConstraintHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/allocation/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteAllocationHandler(dbSession, tc, cfg),
		},
		// Subnet endpoints
		{
			Path:    apiPathPrefix + "/subnet",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSubnetHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/subnet",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllSubnetHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/subnet/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSubnetHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/subnet/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateSubnetHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/subnet/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteSubnetHandler(dbSession, tc, scp, cfg),
		},
		// OperatingSystem endpoints
		{
			Path:    apiPathPrefix + "/operating-system",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateOperatingSystemHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/operating-system",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllOperatingSystemHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/operating-system/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetOperatingSystemHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/operating-system/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateOperatingSystemHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/operating-system/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteOperatingSystemHandler(dbSession, tc, scp, cfg),
		},
		// iPXE Template endpoints (read-only; templates are synced from nico-core)
		{
			Path:    apiPathPrefix + "/ipxe-template",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllIpxeTemplateHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/ipxe-template/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetIpxeTemplateHandler(dbSession, tc, cfg),
		},
		// NetworkSecurityGroup endpoints
		{
			Path:    apiPathPrefix + "/network-security-group",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateNetworkSecurityGroupHandler(dbSession, tc, scp, cfg),
		},

		{
			Path:    apiPathPrefix + "/network-security-group",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllNetworkSecurityGroupHandler(dbSession, tc, cfg),
		},

		{
			Path:    apiPathPrefix + "/network-security-group/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetNetworkSecurityGroupHandler(dbSession, tc, cfg),
		},

		{
			Path:    apiPathPrefix + "/network-security-group/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateNetworkSecurityGroupHandler(dbSession, tc, scp, cfg),
		},

		{
			Path:    apiPathPrefix + "/network-security-group/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteNetworkSecurityGroupHandler(dbSession, tc, scp, cfg),
		},

		// SSHKey endpoints
		{
			Path:    apiPathPrefix + "/sshkey",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSSHKeyHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkey",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllSSHKeyHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkey/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSSHKeyHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkey/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateSSHKeyHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkey/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteSSHKeyHandler(dbSession, tc, cfg),
		},
		// SSHKeyGroup endpoints
		{
			Path:    apiPathPrefix + "/sshkeygroup",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSSHKeyGroupHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkeygroup",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllSSHKeyGroupHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkeygroup/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSSHKeyGroupHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkeygroup/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateSSHKeyGroupHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sshkeygroup/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteSSHKeyGroupHandler(dbSession, tc, cfg),
		},
		// Machine Capability endpoints
		{
			Path:    apiPathPrefix + "/machine-capability",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineCapabilityHandler(dbSession),
		},
		// Audit Log endpoints
		{
			Path:    apiPathPrefix + "/audit",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllAuditEntryHandler(dbSession),
		},
		{
			Path:    apiPathPrefix + "/audit/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAuditEntryHandler(dbSession),
		},
		// Machine Validation endpoints
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/test",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateMachineValidationTestHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/test/:id/version/:version",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateMachineValidationTestHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/test",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineValidationTestHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/test/:id/version/:version",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineValidationTestHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/machine/:machineID/results",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineValidationResultsHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/machine/:machineID/runs",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineValidationRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/external-config",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllMachineValidationExternalConfigHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/external-config/:cfgName",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetMachineValidationExternalConfigHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/external-config",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateMachineValidationExternalConfigHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/external-config/:cfgName",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateMachineValidationExternalConfigHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/machine-validation/external-config/:cfgName",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteMachineValidationExternalConfigHandler(dbSession, tc, scp, cfg),
		},
		// DPU Extension Service endpoints
		{
			Path:    apiPathPrefix + "/dpu-extension-service",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateDpuExtensionServiceHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/dpu-extension-service",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllDpuExtensionServiceHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/dpu-extension-service/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetDpuExtensionServiceHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/dpu-extension-service/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateDpuExtensionServiceHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/dpu-extension-service/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteDpuExtensionServiceHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/dpu-extension-service/:id/version/:version",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetDpuExtensionServiceVersionHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/dpu-extension-service/:id/version/:version",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteDpuExtensionServiceVersionHandler(dbSession, tc, scp, cfg),
		},
		// SKU endpoints
		{
			Path:    apiPathPrefix + "/sku",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateSkuHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/sku",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllSkuHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sku/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetSkuHandler(dbSession, tc, cfg),
		},
		{
			Path:    apiPathPrefix + "/sku/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateSkuHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/sku/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteSkuHandler(dbSession, scp),
		},
		// Task endpoints (Flow). /rack/task/* and /task/* share get/cancel
		// handlers; filtered lists are also exposed under /rack/{id}/task and
		// /tray/{id}/task.
		{
			Path:    apiPathPrefix + "/rack/task/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTaskHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/task/:id/cancel",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCancelTaskHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllTaskHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/task/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTaskHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/:id/cancel",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCancelTaskHandler(dbSession, tc, scp, cfg),
		},
		// Operation Rule endpoints (Flow). Rules govern how tasks execute, so
		// they live under the /task namespace.
		{
			Path:    apiPathPrefix + "/task/rule",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateTaskRuleHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/rule",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllTaskRuleHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/rule/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTaskRuleHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/rule/:id",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateTaskRuleHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/rule/:id",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteTaskRuleHandler(dbSession, tc, scp, cfg),
		},
		// Run endpoints (Flow). A run is a phased, policy-gated execution of
		// one operation across many racks; Flow calls it an operation run.
		{
			Path:    apiPathPrefix + "/task/run",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCreateTaskRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllTaskRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTaskRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run/:id/target",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllTaskRunTargetHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run/:id/pause",
			Method:  http.MethodPost,
			Handler: apiHandler.NewPauseTaskRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run/:id/resume",
			Method:  http.MethodPost,
			Handler: apiHandler.NewResumeTaskRunHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run/:id/advance",
			Method:  http.MethodPost,
			Handler: apiHandler.NewAdvanceTaskRunPhaseHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/task/run/:id/cancel",
			Method:  http.MethodPost,
			Handler: apiHandler.NewCancelTaskRunHandler(dbSession, tc, scp, cfg),
		},
		// NVLink Domain operation endpoints (Flow).
		{
			Path:    apiPathPrefix + "/domain/nvlink/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewBatchUpdateNVLinkDomainPowerStateHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/domain/nvlink/firmware",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewBatchUpdateNVLinkDomainFirmwareHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/domain/nvlink/:id/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateNVLinkDomainPowerStateHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/domain/nvlink/:id/firmware",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateNVLinkDomainFirmwareHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/rack",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllRackHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/validation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewValidateRacksHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewBatchUpdateRackPowerStateHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/firmware",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewBatchUpdateRackFirmwareHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/bringup",
			Method:  http.MethodPost,
			Handler: apiHandler.NewBatchBringUpRackHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetRackHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/:id/validation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewValidateRackHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/:id/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateRackPowerStateHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/:id/firmware",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateRackFirmwareHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/:id/bringup",
			Method:  http.MethodPost,
			Handler: apiHandler.NewBringUpRackHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/rack/:id/task",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetRackTasksHandler(dbSession, tc, scp, cfg),
		},
		// Tray endpoints (Flow)
		{
			Path:    apiPathPrefix + "/tray",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetAllTrayHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewBatchUpdateTrayPowerStateHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/firmware",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewBatchUpdateTrayFirmwareHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/validation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewValidateTraysHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/:id",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTrayHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/:id/power",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateTrayPowerStateHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/:id/firmware",
			Method:  http.MethodPatch,
			Handler: apiHandler.NewUpdateTrayFirmwareHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/:id/validation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewValidateTrayHandler(dbSession, tc, scp, cfg),
		},
		{
			Path:    apiPathPrefix + "/tray/:id/task",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTrayTasksHandler(dbSession, tc, scp, cfg),
		},
		// Tenant Identity endpoints
		{
			Path:    apiPathPrefix + "/site/:siteID/tenant-identity/config",
			Method:  http.MethodPut,
			Handler: apiHandler.NewCreateOrUpdateTenantIdentityConfigHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/tenant-identity/config",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTenantIdentityConfigHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/tenant-identity/config",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteTenantIdentityConfigHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/tenant-identity/token-delegation",
			Method:  http.MethodPut,
			Handler: apiHandler.NewCreateOrUpdateTenantIdentityTokenDelegationHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/tenant-identity/token-delegation",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetTenantIdentityTokenDelegationHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/tenant-identity/token-delegation",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteTenantIdentityTokenDelegationHandler(dbSession, scp),
		},
		// Host Firmware Config endpoint (Provider Admin). Proxied to Core via
		// UpsertHostFirmwareConfig.
		{
			Path:    apiPathPrefix + "/firmware-config/host",
			Method:  http.MethodPut,
			Handler: apiHandler.NewCreateOrUpdateHostFirmwareConfigHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/firmware-config/host",
			Method:  http.MethodDelete,
			Handler: apiHandler.NewDeleteHostFirmwareConfigHandler(dbSession, scp),
		},
	}

	return apiRoutes
}

// NewWellKnownRoutes returns the public tenant-identity discovery routes.
// Registered before the auth middleware in server.go.
func NewWellKnownRoutes(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) []Route {
	apiName := cfg.GetAPIName()
	apiPathPrefix := "/org/:orgName/" + apiName

	return []Route{
		{
			Path:    apiPathPrefix + "/site/:siteID/.well-known/jwks.json",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetJWKSHandler(dbSession, scp, corev1.JwksKind_Oidc),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/.well-known/openid-configuration",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetOpenIDConfigurationHandler(dbSession, scp),
		},
		{
			Path:    apiPathPrefix + "/site/:siteID/.well-known/spiffe/jwks.json",
			Method:  http.MethodGet,
			Handler: apiHandler.NewGetJWKSHandler(dbSession, scp, corev1.JwksKind_Spiffe),
		},
	}
}
