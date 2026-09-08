// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"fmt"
	"math/rand"
	"net/http"

	goset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	powerutil "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

const (
	// batchSuffixLength is the length of the random suffix used for batch instance names
	batchSuffixLength = 6
)

// ~~~~~ Batch Create Handler ~~~~~ //

// BatchCreateInstanceHandler is the API Handler for creating multiple instances with topology-optimized allocation
type BatchCreateInstanceHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// NewBatchCreateInstanceHandler initializes and returns a new handler for batch creating Instances
func NewBatchCreateInstanceHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) BatchCreateInstanceHandler {
	return BatchCreateInstanceHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		dps:        dps,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// buildBatchInstanceCreateRequestOsConfig validates and retrieves OS configuration for batch instance creation.
// This mirrors the behavior of CreateInstanceHandler.buildInstanceCreateRequestOsConfig.
// Returns: osConfig, osID, and error (matching single API pattern)
func (bcih BatchCreateInstanceHandler) buildBatchInstanceCreateRequestOsConfig(c echo.Context, logger *zerolog.Logger, apiRequest *model.APIBatchInstanceCreateRequest, site *cdbm.Site) (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError) {

	ctx := c.Request().Context()

	// If no OS was selected
	if apiRequest.OperatingSystemID == nil || *apiRequest.OperatingSystemID == "" {

		if err := apiRequest.ValidateAndSetOperatingSystemData(bcih.cfg, nil); err != nil {
			logger.Error().Err(err).Msg("failed to validate OperatingSystem")
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to validate OperatingSystem data", err)
		}

		return &corev1.InstanceOperatingSystemConfig{
			RunProvisioningInstructionsOnEveryBoot: *apiRequest.AlwaysBootWithCustomIpxe, // Set by the earlier call to ValidateAndSetOperatingSystemData
			PhoneHomeEnabled:                       *apiRequest.PhoneHomeEnabled,         // Set by the earlier call to ValidateAndSetOperatingSystemData
			Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
				Ipxe: &corev1.InlineIpxe{
					IpxeScript: *apiRequest.IpxeScript,
				},
			},
			UserData: apiRequest.UserData,
		}, nil, nil
	}

	// Otherwise, we'll use the OS sent by the caller

	var id uuid.UUID
	var err error

	if id, err = uuid.Parse(*apiRequest.OperatingSystemID); err != nil {
		logger.Error().Err(err).Msg("failed to parse OperatingSystemID")
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Unable to parse `operatingSystemId` specified", validation.Errors{
			"operatingSystemId": errors.New(*apiRequest.OperatingSystemID),
		})
	}

	osID := &id

	// Retrieve the details for the OS
	osDAO := cdbm.NewOperatingSystemDAO(bcih.dbSession)
	os, serr := osDAO.GetByID(ctx, nil, *osID, nil)
	if serr != nil {
		if serr == cdb.ErrDoesNotExist {
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Could not find OperatingSystem with ID specified in request data", validation.Errors{
				"id": errors.New(osID.String()),
			})
		}
		logger.Error().Err(serr).Msg("error retrieving OperatingSystem from DB by ID")
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve OperatingSystem with ID specified in request data, DB error", validation.Errors{
			"id": errors.New(osID.String()),
		})
	}

	// Add the OS ID to the log fields now that we know we have a valid one.
	logger.UpdateContext(func(c zerolog.Context) zerolog.Context {
		return c.Str("OperatingSystem ID", os.ID.String())
	})

	// Confirm the Tenant can use the OS. Provider-owned Templated iPXE OSes are
	// shared through synchronized Site associations validated below.
	if !os.IsTenantUsable(apiRequest.TenantID) {
		logger.Error().Msg("OperatingSystem in request is not usable by tenant")
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "OperatingSystem specified in request is not owned by Tenant", nil)
	}

	if os.Type == cdbm.OperatingSystemTypeImage {
		if site.Config == nil || !site.Config.ImageBasedOperatingSystem {
			logger.Warn().Str("operatingSystemId", os.ID.String()).Str("siteId", site.ID.String()).Msg("Creation of Instance with Image based Operating System is not supported for Site, ImageBasedOperatingSystem capability is not enabled")
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Creation of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.", nil)
		}
	}

	// Confirm match between site and OS (only for Image type).
	/*
		if os.Type == cdbm.OperatingSystemTypeImage {
			ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(bcih.dbSession)
			_, ossaCount, err := ossaDAO.GetAll(
				ctx,
				nil,
				cdbm.OperatingSystemSiteAssociationFilterInput{
					OperatingSystemIDs: []uuid.UUID{id},
					SiteIDs:            []uuid.UUID{siteID},
				},
				cdbp.PageInput{Limit: cutil.GetPtr(1)},
				nil,
			)
			if err != nil {
				logger.Error().Msgf("Error retrieving OperatingSystemAssociations for OS: %s", err)
				return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve OperatingSystemAssociations for OS with ID specified in request data, DB error", validation.Errors{
					"id": errors.New(osID.String()),
				})
			}
			if ossaCount == 0 {
				logger.Error().Msg("OperatingSystem does not belong to VPC site")
				return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "OperatingSystem specified in request is not in VPC site", nil)
			}
		}*/

	// Validate any additional properties.
	// `os` could still be nil here if no OS ID was sent
	// in the request.

	err = apiRequest.ValidateAndSetOperatingSystemData(bcih.cfg, os)
	if err != nil {
		logger.Error().Msgf("OperatingSystem options validation failed: %s", err)
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "OperatingSystem options validation failed", err)
	}

	// Options below should all have been set by the
	// earlier call to ValidateAndSetOperatingSystemData

	if os.Type == cdbm.OperatingSystemTypeIPXE {
		return &corev1.InstanceOperatingSystemConfig{
			RunProvisioningInstructionsOnEveryBoot: *apiRequest.AlwaysBootWithCustomIpxe,
			PhoneHomeEnabled:                       *apiRequest.PhoneHomeEnabled,
			Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
				Ipxe: &corev1.InlineIpxe{
					IpxeScript: *apiRequest.IpxeScript,
				},
			},
			UserData: apiRequest.UserData,
		}, osID, nil
	} else if os.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
		if apiErr := validateTemplatedIpxeOsForSite(ctx, bcih.dbSession, logger, os, site.ID); apiErr != nil {
			return nil, nil, apiErr
		}
		return &corev1.InstanceOperatingSystemConfig{
			RunProvisioningInstructionsOnEveryBoot: *apiRequest.AlwaysBootWithCustomIpxe,
			PhoneHomeEnabled:                       *apiRequest.PhoneHomeEnabled,
			Variant: &corev1.InstanceOperatingSystemConfig_OperatingSystemId{
				OperatingSystemId: &corev1.OperatingSystemId{
					Value: os.ID.String(),
				},
			},
			UserData: apiRequest.UserData,
		}, osID, nil
	} else {
		return &corev1.InstanceOperatingSystemConfig{
			PhoneHomeEnabled: *apiRequest.PhoneHomeEnabled,
			Variant: &corev1.InstanceOperatingSystemConfig_OsImageId{
				OsImageId: &corev1.UUID{
					Value: os.ID.String(),
				},
			},
			UserData: apiRequest.UserData,
		}, osID, nil
	}
}

// Handle godoc
// @Summary Batch create multiple Instances with topology-optimized allocation
// @Description Create multiple Instances with topology-optimized machine allocation. If topologyOptimized is true, all instances must be allocated on the same rack/topology domain (e.g., for NVLink). If false, instances can be spread across racks.
// @Tags Instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIBatchInstanceCreateRequest true "Batch instance creation request"
// @Success 201 {object} []model.APIInstance
// @Router /v2/org/{org}/nico/instance/batch [post]
func (bcih BatchCreateInstanceHandler) Handle(c echo.Context) error {
	// Execution Steps:
	// 1. Authentication & Authorization
	//    - Extract user from context
	//    - Validate org membership
	//    - Validate Tenant Admin role
	// 2. Request Validation
	//    - Bind and validate batch request data (count, namePrefix, topology flag)
	//    - Validate tenant, instance type, VPC, site
	//    - Load and validate Interfaces (Subnets, VPC Prefixes, or VPC selection; shared across all instances)
	//    - Load and validate DPU Extension Service Deployments - shared across all instances
	//    - Load and validate Network Security Groups - shared across all instances
	//    - Load and validate SSH Key Groups - shared across all instances
	//    - Validate OS or iPXE script
	//    - Generate unique instance names and check for conflicts
	// 3. Database Transaction
	//    - Start transaction
	//    - Acquire advisory lock on Tenant + Instance Type
	// 4. Machine Selection
	//    - Verify allocation constraints have sufficient quota for batch
	//    - Build list of available allocation constraints with capacity
	//    - Allocate multiple machines (topology-optimized or cross-rack)
	//    - Mark all allocated machines as assigned with per-machine advisory locks
	// 5. Machine Capability Validation
	//    - Validate InfiniBand interfaces against Instance Type capabilities - shared across all instances
	//    - Validate InfiniBand partitions (Site, Tenant, Status)
	//    - Validate DPU interfaces against Instance Type capabilities - shared across all instances
	//    - Validate NVLink interfaces against Instance Type capabilities - shared across all instances
	//    - Validate NVLink logical partitions (Site, Tenant, Status)
	// 6. Create Instance Records (loop for each instance)
	//    - Create Instance record with allocated machine
	//    - Update ControllerInstanceID
	//    - Create SSH Key Group associations
	//    - Create Interface records
	//    - Create InfiniBand Interface records
	//    - Create NVLink Interface records
	//    - Create DPU Extension Service Deployment records
	//    - Create status detail record
	//    - Switch to next allocation constraint when current reaches capacity
	// 7. Workflow Trigger
	//    - Build batch instance allocation request with all configs
	//    - Execute synchronous Temporal workflow (CreateInstances)
	//    - Wait for site-agent to provision all instances
	//    - Handle timeout with workflow termination
	// 8. Commit & Response
	//    - Commit transaction after workflow succeeds
	//    - Return array of created instances to client
	//
	// Key Differences from Single Instance API:
	// - Creates multiple instances in single transaction (all-or-nothing)
	// - Topology-optimized machine allocation (same rack or cross-rack based on flag)
	// - Automatically distributes instances across multiple allocation constraints
	// - Shared interface configurations across all instances (Interfaces, InfiniBandInterfaces, NVLinkInterfaces)
	// - Batch workflow for atomic provisioning of all instances

	// ==================== Step 1: Authentication & Authorization ====================

	// Get context
	ctx := c.Request().Context()

	// Get org
	org := c.Param("orgName")

	// Initialize logger
	logger := log.With().Str("Model", "Instance").Str("Handler", "BatchCreate").Str("Org", org).Logger()

	logger.Info().Msg("started API handler for batch instance creation")

	// Create a child span and set the attributes for current request
	newctx, handlerSpan := bcih.tracerSpan.CreateChildInContext(ctx, "BatchCreateInstanceHandler", logger)
	if handlerSpan != nil {
		// Set newly created span context as a current context
		ctx = newctx

		defer handlerSpan.End()

		bcih.tracerSpan.SetAttribute(handlerSpan, attribute.String("org", org), logger)
	}

	dbUser, logger, err := common.GetUserAndEnrichLogger(c, logger, bcih.tracerSpan, handlerSpan)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Tenant Admins are allowed to create Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// ==================== Step 2: Request Validation ====================

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIBatchInstanceCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch instance creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating batch instance creation request data", verr)
	}
	// Set default for TopologyOptimized if not provided
	// Default to true for better performance and locality
	topologyOptimized := true
	if apiRequest.TopologyOptimized != nil {
		topologyOptimized = *apiRequest.TopologyOptimized
	}

	logger.Info().Int("Count", apiRequest.Count).
		Bool("TopologyOptimized", topologyOptimized).
		Interface("MachineLabelSelector", apiRequest.MachineLabelSelector).
		Msg("Input validation completed for batch Instance creation request")

	// Validate the tenant for which these Instances are being created
	tenant, err := common.GetTenantForOrg(ctx, nil, bcih.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant for org", nil)
	}

	// Deprecated: tenantId in request body. Infer from org when not provided.
	if apiRequest.TenantID == "" {
		apiRequest.TenantID = tenant.ID.String()
	}

	apiTenant, err := common.GetTenantFromIDString(ctx, nil, apiRequest.TenantID, bcih.dbSession)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving tenant from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "TenantID in request is not valid", nil)
	}
	if apiTenant.ID != tenant.ID {
		logger.Warn().Msg("tenant id in request does not match tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "TenantID in request does not match tenant in org", nil)
	}

	// Validate the instance type
	apiInstanceTypeID, err := uuid.Parse(apiRequest.InstanceTypeID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing instance type id in request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Instance Type ID in request is not valid", nil)
	}

	itDAO := cdbm.NewInstanceTypeDAO(bcih.dbSession)
	instancetype, err := itDAO.GetByID(ctx, nil, apiInstanceTypeID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Instance Type with ID specified in request data", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance Type from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Type with ID specified in request data", nil)
	}

	// Validate the VPC state
	vpc, err := common.GetVpcFromIDString(ctx, nil, apiRequest.VpcID, []string{cdbm.NVLinkLogicalPartitionRelationName}, bcih.dbSession)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find VPC with ID specified in request data", nil)
		}
		logger.Warn().Err(err).Str("vpcId", apiRequest.VpcID).Msg("error retrieving VPC from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VpcID in request is not valid", nil)
	}

	// Ensure that the VPC belongs to the Tenant (check ownership before status)
	if vpc.TenantID != tenant.ID {
		logger.Warn().Msg("tenant id in request does not match tenant in VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC specified in request is not owned by Tenant", nil)
	}

	// Validate VPC status
	if vpc.ControllerVpcID == nil || vpc.Status != cdbm.VpcStatusReady {
		logger.Warn().Msg("VPC specified in request data is not ready")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC specified in request data is not ready", nil)
	}
	// Validate request fields that depend on the resolved VPC (e.g.
	// `autoNetwork` requires a Flat VPC).
	verr = apiRequest.ValidateForVpc(vpc)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch Instance creation request against VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating batch Instance creation request data", verr)
	}

	var defaultNvllpID *uuid.UUID
	if vpc.NVLinkLogicalPartitionID != nil {
		// NOTE: No validation needed here because the VPC validation ensures the NVLink Logical Partition is valid for this instance
		defaultNvllpID = vpc.NVLinkLogicalPartitionID
	}

	// Verify that VPC and InstanceType are on the same Site
	if vpc.SiteID != *instancetype.SiteID {
		logger.Warn().
			Str("Site ID for VPC", vpc.SiteID.String()).
			Str("Site ID for Instance Type", instancetype.SiteID.String()).
			Msg("VPC and InstanceType are not on the same Site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC and Instance Type specified in request data do not belong to the same Site", nil)
	}

	// Get Site
	siteDAO := cdbm.NewSiteDAO(bcih.dbSession)
	site, err := siteDAO.GetByID(ctx, nil, vpc.SiteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where Instances are being created could not be found", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "The Site where Instances are being created could not be retrieved", nil)
	}

	// Validate Site status
	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Str("Site ID", site.ID.String()).Str("Site Status", site.Status).
			Msg("The Site where Instances are being created is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where Instances are being created is not in Registered state", nil)
	}

	// A non-empty label selector can narrow placement to a single Machine, so it
	// require the same site-scoped privilege as an explicit Machine ID.
	if len(apiRequest.MachineLabelSelector) > 0 {
		privilegedAccess, derr := common.TenantHasTargetedInstanceCreation(ctx, nil, bcih.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
		if derr != nil {
			logger.Error().Err(derr).Msg("error checking effective targeted instance creation for Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify capability for Site", nil)
		}
		if !privilegedAccess {
			logger.Warn().Msg("tenant does not have capability to create instances using Machine label selector")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have capability to create Instances using Machine label selector", nil)
		}
	}

	if apiErr := util.ValidateSitePowerManagement(site.Config, apiRequest.PowerProfile); apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if bcih.cfg.GetDPSEnabled() && apiRequest.PowerProfile != nil && vpc.PowerResourceGroup == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Power profile cannot be specified when creating Instances if VPC doesn't have power resource group populated.", nil)
	}
	apiErr := model.ValidatePowerProfile(ctx, bcih.cfg.GetDPSEnabled(), bcih.dps, apiRequest.PowerProfile)
	if apiErr != nil {
		logger.Warn().Err(apiErr.Diagnosis()).Msg("failed to validate batch Instance power profile")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	// Load and validate subnets and VPC prefixes (batch query for efficiency)
	subnetDAO := cdbm.NewSubnetDAO(bcih.dbSession)
	vpDAO := cdbm.NewVpcPrefixDAO(bcih.dbSession)

	// Collect all Subnet and VPC Prefix IDs for batch query
	subnetIDs := []uuid.UUID{}
	vpcPrefixIDs := []uuid.UUID{}

	for _, ifc := range apiRequest.Interfaces {
		if ifc.SubnetID != nil {
			subnetID, err := uuid.Parse(*ifc.SubnetID)
			if err != nil {
				logger.Error().Err(err).Str("subnetID", *ifc.SubnetID).
					Msg("error parsing subnet id")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Subnet ID format", nil)
			}
			subnetIDs = append(subnetIDs, subnetID)
		}
		if ifc.VpcPrefixID != nil {
			vpcPrefixID, err := uuid.Parse(*ifc.VpcPrefixID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing vpcprefix id in instance vpcprefix request")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC Prefix ID specified in request data is not valid", nil)
			}
			vpcPrefixIDs = append(vpcPrefixIDs, vpcPrefixID)
		}
	}

	// Batch fetch Subnets from DB
	subnetIDMap := make(map[uuid.UUID]*cdbm.Subnet)
	if len(subnetIDs) > 0 {
		subnets, _, err := subnetDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{SubnetIDs: subnetIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Subnets from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnets from DB by IDs", nil)
		}
		for i := range subnets {
			subnetIDMap[subnets[i].ID] = &subnets[i]
		}
	}

	// Batch fetch VPC Prefixes from DB
	vpcPrefixIDMap := make(map[uuid.UUID]*cdbm.VpcPrefix)
	if len(vpcPrefixIDs) > 0 {
		vpcPrefixes, _, err := vpDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{VpcPrefixIDs: vpcPrefixIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving VPC Prefixes from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Prefixes from DB by IDs", nil)
		}
		for i := range vpcPrefixes {
			vpcPrefixIDMap[vpcPrefixes[i].ID] = &vpcPrefixes[i]
		}
	}

	// Load VPCs only for interfaces using Core-managed prefix selection.
	interfaceVpcIDMap, interfaceVpcErr := loadInstanceInterfaceVpcs(ctx, &logger, bcih.dbSession, apiRequest.Interfaces, tenant.ID, site.ID)
	if interfaceVpcErr != nil {
		logger.Warn().Err(interfaceVpcErr).Msg("failed to validate VPCs specified by batch Instance interfaces")
		return cutil.NewAPIErrorResponse(c, interfaceVpcErr.Code, interfaceVpcErr.Message, interfaceVpcErr.Data)
	}

	// Validate each Interface against fetched data and build dbInterfaces
	dbInterfaces := []cdbm.Interface{}
	isDeviceInfoPresent := false
	pfWithinVPC := []uuid.UUID{}
	primaryPhysicalInterfaceUsesVpcSelection := false
	allFoundVpcIds := goset.NewSet[uuid.UUID]()

	// Prepare the unique set of all VPC IDs for this batch request.
	allRequestedVpcIds := goset.NewSet[uuid.UUID]()
	for _, secondaryVpcID := range apiRequest.SecondaryVpcIDs {
		id, err := uuid.Parse(secondaryVpcID)
		if err != nil {
			logger.Error().Msgf("invalid VPC ID %v in `secondaryVpcIds` in request data", secondaryVpcID)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid VPC ID `%s` in secondaryVpcIds in request data", secondaryVpcID), nil)
		}

		if !allRequestedVpcIds.Add(id) {
			logger.Error().Msgf("duplicate ID %s found in `secondaryVpcIds`", id)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Duplicate ID `%s` found in `secondaryVpcIds`", id), nil)
		}
	}

	// Add the primary VPC. It must not also appear in secondaryVpcIds.
	if !allRequestedVpcIds.Add(vpc.ID) {
		logger.Error().Msgf("primary VPC ID: %s for Instances must not be listed in `secondaryVpcIds`", vpc.ID)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Primary VPC ID: %s for Instances must not be listed in `secondaryVpcIds`", vpc.ID), nil)
	}

	for _, ifc := range apiRequest.Interfaces {
		if ifc.SubnetID != nil {
			subnetID := uuid.MustParse(*ifc.SubnetID)

			subnet, ok := subnetIDMap[subnetID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Subnet with ID specified in request data", nil)
			}

			if subnet.TenantID != tenant.ID {
				logger.Warn().Msg(fmt.Sprintf("Subnet: %v specified in request is not owned by Tenant", subnetID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Subnet: %v specified in request is not owned by Tenant", subnetID), nil)
			}

			if subnet.ControllerNetworkSegmentID == nil || subnet.Status != cdbm.SubnetStatusReady {
				logger.Warn().Msg(fmt.Sprintf("Subnet: %v specified in request data is not in Ready state", subnetID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Subnet: %v specified in request data is not in Ready state", subnetID), nil)
			}

			if subnet.VpcID != vpc.ID {
				logger.Warn().Msg(fmt.Sprintf("Subnet: %v specified in request does not match with VPC", subnetID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Subnet: %v specified in request does not match with VPC", subnetID), nil)
			}

			if vpc.NetworkVirtualizationType != nil && *vpc.NetworkVirtualizationType != cdbm.VpcEthernetVirtualizer {
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have Ethernet network virtualization type in order to create Subnet based interfaces", vpc.ID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have Ethernet network virtualization type in order to create Subnet based interfaces", vpc.ID), nil)
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				SubnetID:   &subnetID,
				IsPhysical: ifc.IsPhysical,
				Status:     cdbm.InterfaceStatusPending,
			})
		}

		if ifc.VpcPrefixID != nil {
			vpcPrefixUUID := uuid.MustParse(*ifc.VpcPrefixID)

			vpcPrefix, ok := vpcPrefixIDMap[vpcPrefixUUID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find VPC Prefix with ID specified in request data", nil)
			}

			if vpcPrefix.TenantID != tenant.ID {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request is not owned by Tenant", vpcPrefixUUID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request is not owned by Tenant", vpcPrefixUUID), nil)
			}

			if vpcPrefix.Status != cdbm.VpcPrefixStatusReady {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefixUUID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefixUUID), nil)
			}

			if vpcPrefix.SiteID != site.ID {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixUUID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixUUID), nil)
			}

			if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", vpc.ID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", vpc.ID), nil)
			}

			// If the interface is associated with a VPC ID that the user
			// didn't request, reject the request.
			if !allRequestedVpcIds.Contains(vpcPrefix.VpcID) {
				logger.Error().Msgf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix.ID, vpcPrefix.VpcID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix.ID, vpcPrefix.VpcID), nil)
			}

			// Collect the VPC IDs actually found based on interface definitions.
			allFoundVpcIds.Add(vpcPrefix.VpcID)

			if ifc.Device != nil && ifc.DeviceInstance != nil {
				isDeviceInfoPresent = true
			}

			// The requirement that the VpcID of a prefix being associated with an interface must match the VPC of the batch request
			// is only valid for the first interface where ifc.IsPhysical==true.
			// When DeviceInstance is present, "first interface" is the PF of the first DPU, defined as DeviceInstance==0.
			// For all other interfaces, there is no such requirement, and instances are allowed to attach to different VPCs
			// using additional interfaces.
			if ifc.IsPhysical {
				// If no device info, append it.
				// If DeviceInstance > 0, just ignore it.
				// If DeviceInstance==0, then just replace the slice.
				// This will give precedence to DeviceInstance==0
				// for defining whether the primary.  DeviceInstance > 0
				// is by definition not the primary.
				if !isDeviceInfoPresent {
					pfWithinVPC = append(pfWithinVPC, vpcPrefix.VpcID)
					primaryPhysicalInterfaceUsesVpcSelection = false
				} else if ifc.DeviceInstance != nil && *ifc.DeviceInstance == 0 {
					pfWithinVPC = []uuid.UUID{vpcPrefix.VpcID}
					primaryPhysicalInterfaceUsesVpcSelection = false
				}
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				VpcPrefixID:          &vpcPrefixUUID,
				VpcPrefix:            vpcPrefix,
				RequestedIpAddress:   nil, // Explicit IPs are not supported for batch create.
				InlineRoutingProfile: ifc.InlineRoutingProfile.ToDB(),
				Device:               ifc.Device,
				DeviceInstance:       ifc.DeviceInstance,
				VirtualFunctionID:    ifc.VirtualFunctionID,
				IsPhysical:           ifc.IsPhysical,
				Status:               cdbm.InterfaceStatusPending,
			})
		}

		if ifc.VpcID != nil {
			interfaceVpcID := uuid.MustParse(*ifc.VpcID)
			interfaceVpc := interfaceVpcIDMap[interfaceVpcID]
			if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC based interfaces", vpc.ID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC based interfaces", vpc.ID), nil)
			}

			if !allRequestedVpcIds.Contains(interfaceVpcID) {
				logger.Error().Msgf("One or more Interfaces specify VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", interfaceVpcID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("One or more Interfaces specify VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", interfaceVpcID), nil)
			}

			allFoundVpcIds.Add(interfaceVpcID)
			if ifc.Device != nil && ifc.DeviceInstance != nil {
				isDeviceInfoPresent = true
			}
			if ifc.IsPhysical {
				if !isDeviceInfoPresent {
					pfWithinVPC = append(pfWithinVPC, interfaceVpcID)
					primaryPhysicalInterfaceUsesVpcSelection = true
				} else if ifc.DeviceInstance != nil && *ifc.DeviceInstance == 0 {
					pfWithinVPC = []uuid.UUID{interfaceVpcID}
					primaryPhysicalInterfaceUsesVpcSelection = true
				}
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				VpcID:                &interfaceVpcID,
				Vpc:                  interfaceVpc,
				VpcIPFamilyMode:      cutil.GetPtr(ifc.VpcIPFamilyMode()),
				InlineRoutingProfile: ifc.InlineRoutingProfile.ToDB(),
				Device:               ifc.Device,
				DeviceInstance:       ifc.DeviceInstance,
				VirtualFunctionID:    ifc.VirtualFunctionID,
				IsPhysical:           ifc.IsPhysical,
				Status:               cdbm.InterfaceStatusPending,
			})
		}
	}

	// If there are ethernet interfaces for these Instances,
	// validate the network plan.
	if len(dbInterfaces) > 0 &&
		vpc.NetworkVirtualizationType != nil &&
		*vpc.NetworkVirtualizationType == cdbm.VpcFNN {
		// Throw an error if there are somehow no PFs, or if the VPC of the first
		// PF doesn't match the primary VPC of the batch request.
		if len(pfWithinVPC) == 0 || pfWithinVPC[0] != vpc.ID {
			// Use the VPC-selection response when the primary physical Interface selects a VPC
			// by ID. If no primary physical Interface was found, any Interface selecting a VPC
			// by ID is enough to prefer this response over the legacy VPC Prefix response.
			if primaryPhysicalInterfaceUsesVpcSelection || (len(pfWithinVPC) == 0 && len(interfaceVpcIDMap) > 0) {
				logger.Error().Msg("the primary physical interface must use the Instance VPC")
				if !isDeviceInfoPresent {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface must use the VPC specified in `vpcId`", nil)
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface for deviceInstance: 0 must use the VPC specified in `vpcId`", nil)
			}

			logger.Error().Msg("the primary physical interface must use a VPC prefix that matches with Instance VPC")

			if !isDeviceInfoPresent {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface must use a VPC Prefix that belongs to VPC specified in `vpcId`", nil)
			} else {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface for deviceInstance: 0 must use a VPC Prefix that belongs to VPC specified in `vpcId`", nil)
			}
		}

		// Reject the request if the requested VPC associations don't match
		// the VPC associations actually found based on interface definitions.
		if allRequestedVpcIds.Cardinality() != allFoundVpcIds.Cardinality() {
			// If any Interface selects a VPC by ID, use the generalized error because
			// either VPC IDs or VPC Prefixes can account for a mismatch with `vpcId` or `secondaryVpcIds`.
			if len(interfaceVpcIDMap) > 0 {
				logger.Error().Msg("one or more VPCs specified in `vpcId` or `secondaryVpcIds` are not used by Interfaces in request data")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "One or more VPCs specified in `vpcId` or `secondaryVpcIds` are not used by Interfaces in request data", nil)
			}
			logger.Error().Msg("one or more Interfaces in request data specify VPC Prefixes that do not belong to VPCs specified in `vpcId` or `secondaryVpcIds`")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "One or more Interfaces in request data specify VPC Prefixes that do not belong to VPCs specified in `vpcId` or `secondaryVpcIds`", nil)
		}
	}

	logger.Info().Int("uniqueSubnetCount", len(subnetIDMap)).Int("uniqueVpcPrefixCount", len(vpcPrefixIDMap)).
		Msg("validated all Subnets and VPC Prefixes (shared across all instances)")

	// Validate DPU Extension Service Deployments (shared across all instances)
	desIDMap := map[string]*cdbm.DpuExtensionService{}
	if len(apiRequest.DpuExtensionServiceDeployments) > 0 {
		// Validate DPU Extension Services: parse IDs, batch fetch from DB, verify tenant/site ownership
		// (1) Parse all DPU Extension Service IDs first (fail fast on parse errors)
		desIDs := make([]uuid.UUID, 0, len(apiRequest.DpuExtensionServiceDeployments))
		uniqueDesIDs := make([]uuid.UUID, 0, len(apiRequest.DpuExtensionServiceDeployments))
		seenDesIDs := make(map[uuid.UUID]bool, len(apiRequest.DpuExtensionServiceDeployments))
		for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
			desID, err := uuid.Parse(adesdr.DpuExtensionServiceID)
			if err != nil {
				logger.Warn().Err(err).Str("serviceID", adesdr.DpuExtensionServiceID).
					Msg("error parsing DPU Extension Service ID")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
					fmt.Sprintf("Invalid DPU Extension Service ID: %s", adesdr.DpuExtensionServiceID), nil)
			}
			desIDs = append(desIDs, desID)
			if !seenDesIDs[desID] {
				seenDesIDs[desID] = true
				uniqueDesIDs = append(uniqueDesIDs, desID)
			}
		}

		// (2) Batch fetch all DPU Extension Services in one query
		desDAO := cdbm.NewDpuExtensionServiceDAO(bcih.dbSession)
		desList, _, err := desDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceFilterInput{
			DpuExtensionServiceIDs: uniqueDesIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving DPU Extension Services from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError,
				"Failed to retrieve DPU Extension Services specified in request, DB error", nil)
		}

		// Build map for quick lookup
		desMap := make(map[uuid.UUID]*cdbm.DpuExtensionService, len(desList))
		for i := range desList {
			desMap[desList[i].ID] = &desList[i]
		}

		// (3) Validate each deployment request (ID + version) using the fetched service record.
		// Note: API model validation only rejects duplicate (ID, version) pairs; the same ID with
		// different versions is allowed and must be validated per request.
		for i, desID := range desIDs {
			des, exists := desMap[desID]
			if !exists {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
					fmt.Sprintf("Could not find DPU Extension Service with ID: %s", desID), nil)
			}

			// Validate service belongs to same tenant
			if des.TenantID != tenant.ID {
				logger.Warn().Str("tenantID", tenant.ID.String()).Str("serviceID", desID.String()).
					Msg("DPU Extension Service does not belong to current Tenant")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
					fmt.Sprintf("DPU Extension Service: %s does not belong to current Tenant", desID.String()), nil)
			}

			// Validate service belongs to same site
			if des.SiteID != site.ID {
				logger.Warn().Str("siteID", site.ID.String()).Str("serviceID", desID.String()).
					Msg("DPU Extension Service does not belong to Site")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
					fmt.Sprintf("DPU Extension Service: %s does not belong to Site where Instances are being created", desID.String()), nil)
			}

			// Validate version is in active versions list
			versionFound := false
			requestedVersion := apiRequest.DpuExtensionServiceDeployments[i].Version
			for _, version := range des.ActiveVersions {
				if version == requestedVersion {
					versionFound = true
					break
				}
			}
			if !versionFound {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
					fmt.Sprintf("Version: %s was not found for DPU Extension Service: %s", requestedVersion, desID.String()), nil)
			}

			desIDMap[desID.String()] = des
		}
	}

	logger.Info().Int("dpuExtensionServiceCount", len(apiRequest.DpuExtensionServiceDeployments)).
		Msg("validated DPU Extension Service Deployments")

	// Validate Network Security Group if specified (shared across all instances)
	if apiRequest.NetworkSecurityGroupID != nil {
		nsgDAO := cdbm.NewNetworkSecurityGroupDAO(bcih.dbSession)

		nsg, err := nsgDAO.GetByID(ctx, nil, *apiRequest.NetworkSecurityGroupID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
					fmt.Sprintf("Could not find Network Security Group with ID: %s", *apiRequest.NetworkSecurityGroupID), nil)
			}
			logger.Error().Err(err).Str("nsgID", *apiRequest.NetworkSecurityGroupID).
				Msg("error retrieving Network Security Group from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError,
				"Failed to retrieve Network Security Group specified in request, DB error", nil)
		}

		// Validate NSG belongs to same site
		if nsg.SiteID != site.ID {
			logger.Error().Str("siteID", site.ID.String()).Str("nsgID", *apiRequest.NetworkSecurityGroupID).
				Msg("Network Security Group does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
				"Network Security Group specified in request does not belong to Site", nil)
		}

		// Validate NSG belongs to same tenant
		if nsg.TenantID != tenant.ID {
			logger.Error().Str("tenantID", tenant.ID.String()).Str("nsgID", *apiRequest.NetworkSecurityGroupID).
				Msg("Network Security Group does not belong to Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
				"Network Security Group specified in request does not belong to Tenant", nil)
		}

		logger.Info().Str("nsgID", *apiRequest.NetworkSecurityGroupID).
			Msg("validated Network Security Group")
	}

	// Validate and load SSH key groups (shared across all instances)
	sshKeyGroups := []cdbm.SSHKeyGroup{}
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(bcih.dbSession)

	for _, skgIDStr := range apiRequest.SSHKeyGroupIDs {
		// Validate the SSH Key Group
		sshkeygroup, serr := common.GetSSHKeyGroupFromIDString(ctx, nil, skgIDStr, bcih.dbSession, nil)
		if serr != nil {
			if serr == common.ErrInvalidID {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create Instances, Invalid SSH Key Group ID: %s", skgIDStr), nil)
			}
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create Instances, Could not find SSH Key Group with ID: %s", skgIDStr), nil)
			}

			logger.Warn().Err(serr).Str("SSH Key Group ID", skgIDStr).Msg("error retrieving SSH Key Group from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to retrieve SSH Key Group with ID `%s` specified in request, DB error", skgIDStr), nil)
		}

		if sshkeygroup.TenantID != tenant.ID {
			logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("SSH Key Group ID", skgIDStr).Msg("SSH Key Group does not belong to current Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create Instances, SSH Key Group with ID: %s does not belong to Tenant", skgIDStr), nil)
		}

		// Verify if SSH Key Group Site Association exists
		_, serr = skgsaDAO.GetBySSHKeyGroupIDAndSiteID(ctx, nil, sshkeygroup.ID, site.ID, nil)
		if serr != nil {
			if serr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("SSH Key Group with ID: %s is not associated with the Site where Instances are being created", skgIDStr), nil)
			}
			logger.Warn().Err(serr).Str("SSH Key Group ID", skgIDStr).Msg("error retrieving SSH Key Group Site Association from DB by SSH Key Group ID & Site ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to determine if SSH Key Group: %s is associated with the Site where Instances are being created, DB error", skgIDStr), nil)
		}

		sshKeyGroups = append(sshKeyGroups, *sshkeygroup)
	}

	logger.Info().Int("sshKeyGroupCount", len(sshKeyGroups)).
		Msg("validated SSH Key Groups")

	// Validate and build OS configuration for temporal workflow
	// apiRequest will be mutated for use in CreateFromParams.
	// osConfig will hold the struct/data for use with Temporal/NICo calls.
	// Errors will be returned already in the form of cutil.NewAPIErrorResponse
	osConfig, osID, oserr := bcih.buildBatchInstanceCreateRequestOsConfig(c, &logger, &apiRequest, site)
	if oserr != nil {
		// buildBatchInstanceCreateRequestOsConfig already handles logging,
		// so this is a bit redundant, but this log brings you to the
		// actual call site.
		logger.Error().Err(errors.New(oserr.Message)).Msg("error building os config for creating Instances")
		return c.JSON(oserr.Code, oserr)
	}

	// Generate instance names with random suffix to avoid name conflicts
	// Format: namePrefix-randomSuffix-index (e.g., myapp-a1b2c3-1)
	batchSuffix := uuid.New().String()[:batchSuffixLength]
	generateInstanceName := func(index int) string {
		return fmt.Sprintf("%s-%s-%d", apiRequest.NamePrefix, batchSuffix, index+1)
	}

	// Check for name conflicts before allocating any resources (safety check)
	// Build all instance names first, then check in a single batch query
	inDAO := cdbm.NewInstanceDAO(bcih.dbSession)
	allInstanceNames := make([]string, apiRequest.Count)
	for i := 0; i < apiRequest.Count; i++ {
		allInstanceNames[i] = generateInstanceName(i)
	}

	// Single batch query to check all names at once
	existing, tot, err := inDAO.GetAll(ctx, nil,
		cdbm.InstanceFilterInput{
			Names:     allInstanceNames,
			TenantIDs: []uuid.UUID{tenant.ID},
			SiteIDs:   []uuid.UUID{site.ID},
		},
		cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error checking for name uniqueness")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check instance name uniqueness, DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("existingInstanceName", existing[0].Name).Str("existingInstanceID", existing[0].ID.String()).
			Msg("instance with same name already exists for tenant at site")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict,
			fmt.Sprintf("Instance with name '%s' already exists for Tenant at this Site. Please choose a different name prefix.", existing[0].Name), nil)
	}

	logger.Info().Int("instanceCount", apiRequest.Count).Str("batchSuffix", batchSuffix).
		Msg("validated instance names - all pre-transaction validations completed successfully")

	// ==================== Step 3: Machine Capability Validation (Pre-Tx) ====================
	//
	// Capability validations are pure reads against relatively static data
	// (capabilities, partitions, logical partitions) — they do not gate any
	// write decisions made under the in-tx advisory lock, so they live above
	// the transaction to avoid pinning a connection while doing validation.

	// Get Machine Capabilities for the InstanceType (shared across all instances)
	mcDAO := cdbm.NewMachineCapabilityDAO(bcih.dbSession)
	ibpDAO := cdbm.NewInfiniBandPartitionDAO(bcih.dbSession)
	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(bcih.dbSession)
	var dbibic []cdbm.InfiniBandInterface
	var dbnvlic []cdbm.NVLinkInterface

	// Validate InfiniBand interfaces (shared across all instances)
	if len(apiRequest.InfiniBandInterfaces) > 0 {
		// Get InfiniBand capabilities
		itIbCaps, itIbCapCount, derr := mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{apiInstanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeInfiniBand), nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving InfiniBand Machine Capabilities from DB for Instance Type")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Capabilities for Instance Type, DB error", nil)
		}
		if itIbCapCount == 0 {
			logger.Warn().Msg("InfiniBand interfaces specified but Instance Type doesn't have InfiniBand Capability")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "InfiniBand Interfaces cannot be specified if Instance Type doesn't have InfiniBand Capability", nil)
		}

		// Validate InfiniBand Partitions: parse IDs, batch fetch from DB, verify tenant/site ownership
		// (1) Parse all InfiniBand Partition IDs first (fail fast on parse errors)
		ibpIDs := make([]uuid.UUID, 0, len(apiRequest.InfiniBandInterfaces))
		for _, ibic := range apiRequest.InfiniBandInterfaces {
			ibpID, perr := uuid.Parse(ibic.InfiniBandPartitionID)
			if perr != nil {
				logger.Warn().Err(perr).Msg("error parsing infiniband partition id")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Partition ID %v is not valid", ibic.InfiniBandPartitionID), nil)
			}
			ibpIDs = append(ibpIDs, ibpID)
		}

		// (2) Batch fetch all InfiniBand Partitions in one query
		ibpList, _, derr := ibpDAO.GetAll(ctx, nil, cdbm.InfiniBandPartitionFilterInput{
			InfiniBandPartitionIDs: ibpIDs,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving InfiniBand Partitions from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Partitions specified in request data, DB error", nil)
		}

		// Build map for quick lookup
		ibpMap := make(map[uuid.UUID]*cdbm.InfiniBandPartition, len(ibpList))
		for i := range ibpList {
			ibpMap[ibpList[i].ID] = &ibpList[i]
		}

		// (3) Validate each InfiniBand Partition and build dbibic
		dbibic = []cdbm.InfiniBandInterface{}
		for i, ibic := range apiRequest.InfiniBandInterfaces {
			ibpID := ibpIDs[i]
			ibp, exists := ibpMap[ibpID]
			if !exists {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find Partition with ID %v", ibic.InfiniBandPartitionID), nil)
			}

			if ibp.SiteID != site.ID {
				logger.Warn().Msgf("InfiniBandPartition: %v does not match with Instance Site", ibpID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Partition %v does not match with Instance Site", ibpID), nil)
			}

			if ibp.TenantID != tenant.ID {
				logger.Warn().Msgf("InfiniBandPartition: %v is not owned by Tenant", ibpID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Partition %v is not owned by Tenant", ibpID), nil)
			}

			if ibp.ControllerIBPartitionID == nil || ibp.Status != cdbm.InfiniBandPartitionStatusReady {
				logger.Warn().Msgf("InfiniBandPartition: %v is not in Ready state", ibpID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Partition %v is not in Ready state", ibpID), nil)
			}

			dbibic = append(dbibic, cdbm.InfiniBandInterface{
				InfiniBandPartitionID: ibp.ID,
				Device:                ibic.Device,
				Vendor:                ibic.Vendor,
				DeviceInstance:        ibic.DeviceInstance,
				IsPhysical:            ibic.IsPhysical,
				VirtualFunctionID:     ibic.VirtualFunctionID,
			})
		}

		// Validate InfiniBand Interfaces against capabilities (after partition validation, matching single API)
		verr := model.ValidateInfiniBandInterfaces(itIbCaps, apiRequest.InfiniBandInterfaces)
		if verr != nil {
			logger.Error().Err(verr).Msg("InfiniBand interfaces validation failed")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("InfiniBand interfaces validation failed: %v", verr), verr)
		}
		logger.Info().Int("infiniBandInterfaceCount", len(dbibic)).Msg("validated InfiniBand interfaces (shared across all instances)")
	}

	// Validate DPU interfaces (shared across all instances)
	if isDeviceInfoPresent {
		// Get DPU capabilities for validation
		itDpuCaps, itDpuCapCount, derr := mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{apiInstanceTypeID},
			cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeNetwork), nil, nil, nil, nil, nil,
			cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil, nil, nil, nil, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving DPU Machine Capabilities")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError,
				"Failed to retrieve DPU Capabilities for Instance Type, DB error", nil)
		}

		if itDpuCapCount == 0 {
			logger.Warn().Msg("Device/DeviceInstance specified but Instance Type doesn't have DPU Capability")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
				"Device and DeviceInstance cannot be specified if Instance Type doesn't have Network Capabilities with DPU device type", nil)
		}

		// Validate DPU interfaces against capabilities
		verr := model.ValidateMultiEthernetDeviceInterfaces(itDpuCaps, dbInterfaces)
		if verr != nil {
			logger.Error().Err(verr).Msg("DPU interfaces validation failed")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
				fmt.Sprintf("DPU interfaces validation failed: %v", verr), verr)
		}
		logger.Info().Msg("validated DPU interfaces (shared across all instances)")
	}

	// Validate NVLink interfaces (shared across all instances)
	// Get GPU (NVLink) capabilities
	itNvlCaps, itNvlCapCount, err := mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{apiInstanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeGPU), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving GPU (NVLink) Machine Capabilities from DB for Instance Type")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve GPU Capabilities for Instance Type, DB error", nil)
	}

	if len(apiRequest.NVLinkInterfaces) > 0 {
		if itNvlCapCount == 0 {
			logger.Warn().Msg("NVLink interfaces specified but Instance Type doesn't have GPU (NVLink) Capability")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "NVLink Interfaces cannot be specified if Instance Type doesn't have GPU Capabilities", nil)
		}

		// Validate NVLink interface configuration against capabilities
		verr := model.ValidateNVLinkInterfaces(itNvlCaps, apiRequest.NVLinkInterfaces)
		if verr != nil {
			logger.Error().Err(verr).Msg("NVLink interfaces validation failed")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink interfaces validation failed: %v", verr), verr)
		}

		// Validate NVLink Logical Partitions: parse IDs, batch fetch from DB, verify VPC ownership
		// (1) Parse all NVLink Logical Partition IDs first (fail fast on parse errors)
		nvllpIDs := make([]uuid.UUID, 0, len(apiRequest.NVLinkInterfaces))
		for _, nvlifc := range apiRequest.NVLinkInterfaces {
			nvllpID, perr := uuid.Parse(nvlifc.NVLinkLogicalPartitionID)
			if perr != nil {
				logger.Warn().Err(perr).Msg("error parsing NVLink Logical Partition id")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition ID %v is not valid", nvlifc.NVLinkLogicalPartitionID), nil)
			}
			nvllpIDs = append(nvllpIDs, nvllpID)
		}

		// (2) Batch fetch NVLink Logical Partitions if needed (only when no default)
		var nvllpMap map[uuid.UUID]*cdbm.NVLinkLogicalPartition
		if defaultNvllpID == nil {
			// Collect unique IDs for batch query
			uniqueNvllpIDs := make([]uuid.UUID, 0, len(nvllpIDs))
			seenIDs := make(map[uuid.UUID]bool)
			for _, id := range nvllpIDs {
				if !seenIDs[id] {
					seenIDs[id] = true
					uniqueNvllpIDs = append(uniqueNvllpIDs, id)
				}
			}

			nvllpList, _, derr := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{
				NVLinkLogicalPartitionIDs: uniqueNvllpIDs,
			}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if derr != nil {
				logger.Error().Err(derr).Msg("error retrieving NVLink Logical Partitions from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions specified in request data, DB error", nil)
			}

			nvllpMap = make(map[uuid.UUID]*cdbm.NVLinkLogicalPartition, len(nvllpList))
			for i := range nvllpList {
				nvllpMap[nvllpList[i].ID] = &nvllpList[i]
			}
		}

		// (3) Validate each NVLink Logical Partition and build dbnvlic
		dbnvlic = []cdbm.NVLinkInterface{}
		for i, nvlifc := range apiRequest.NVLinkInterfaces {
			nvllpID := nvllpIDs[i]

			// Validate that the NVLink Logical Partition ID matches the default NVLink Logical Partition ID
			if defaultNvllpID != nil {
				if nvllpID != *defaultNvllpID {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "NVLink Logical Partition specified for NVLink Interface does not match NVLink Logical Partition of VPC", nil)
				}
			} else {
				// Validate NVLink Logical Partition only if it's not the default
				nvllp, exists := nvllpMap[nvllpID]
				if !exists {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find NVLink Logical Partition with ID %v", nvllpID), nil)
				}

				if nvllp.SiteID != site.ID {
					logger.Warn().Msgf("NVLink Logical Partition: %v does not match with Instance Site", nvllpID)
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition %v does not match with Instance Site", nvllpID), nil)
				}

				if nvllp.TenantID != tenant.ID {
					logger.Warn().Msgf("NVLink Logical Partition: %v is not owned by Tenant", nvllpID)
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition %v is not owned by Tenant", nvllpID), nil)
				}

				if nvllp.Status != cdbm.NVLinkLogicalPartitionStatusReady {
					logger.Warn().Msgf("NVLink Logical Partition: %v is not in Ready state", nvllpID)
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition %v is not in Ready state", nvllpID), nil)
				}
			}

			// Build NVLink interface data for later creation
			dbnvlic = append(dbnvlic, cdbm.NVLinkInterface{
				NVLinkLogicalPartitionID: nvllpID,
				DeviceInstance:           nvlifc.DeviceInstance,
			})
		}
		logger.Info().Int("nvlinkInterfaceCount", len(dbnvlic)).Msg("validated NVLink interfaces (shared across all instances)")
	} else if defaultNvllpID != nil {
		// Generate Interfaces for the default NVLink Logical Partition
		// For a given Machine, all the GPUs should be connected to the same NVLink Logical Partition
		dbnvlic = []cdbm.NVLinkInterface{}
		for _, nvlCap := range itNvlCaps {
			if nvlCap.Count != nil {
				for deviceInstance := range *nvlCap.Count {
					dbnvlic = append(dbnvlic, cdbm.NVLinkInterface{
						NVLinkLogicalPartitionID: *defaultNvllpID,
						Device:                   cutil.GetPtr(nvlCap.Name),
						DeviceInstance:           deviceInstance,
					})
				}
			}
		}
		logger.Info().Int("nvlinkInterfaceCount", len(dbnvlic)).Msg("generated default NVLink interfaces (shared across all instances)")
	}

	logger.Info().Msg("completed machine capability validation pre-transaction")

	// Build SSH key group IDs for temporal workflow (shared by all instances)
	instanceSshKeyGroupIds := make([]string, 0, len(sshKeyGroups))
	for _, skg := range sshKeyGroups {
		instanceSshKeyGroupIds = append(instanceSshKeyGroupIds, skg.ID.String())
	}

	// Pre-parse DPU Extension Service IDs (shared validation, done once)
	dpuServiceIDs := make([]uuid.UUID, 0, len(apiRequest.DpuExtensionServiceDeployments))
	for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
		desdID, derr := uuid.Parse(adesdr.DpuExtensionServiceID)
		if derr != nil {
			logger.Warn().Err(derr).Str("serviceID", adesdr.DpuExtensionServiceID).
				Msg("error parsing DPU Extension Service ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
				fmt.Sprintf("Invalid DPU Extension Service ID: %s", adesdr.DpuExtensionServiceID), nil)
		}
		dpuServiceIDs = append(dpuServiceIDs, desdID)
	}

	// ==================== Step 3: Database Transaction ====================

	// instanceData holds all the per-instance rows + workflow configs that
	// get assembled inside the closure and reused for the HTTP response after
	// the closure returns.
	type instanceData struct {
		instance *cdbm.Instance
		ifcs     []cdbm.Interface
		ibifcs   []cdbm.InfiniBandInterface
		nvlifcs  []cdbm.NVLinkInterface
		desds    []cdbm.DpuExtensionServiceDeployment
		ssd      *cdbm.StatusDetail
		// Temporal workflow configs
		interfaceConfigs    []*corev1.InstanceInterfaceConfig
		ibInterfaceConfigs  []*corev1.InstanceIBInterfaceConfig
		nvlInterfaceConfigs []*corev1.InstanceNVLinkGpuConfig
		desdConfigs         []*corev1.InstanceDpuExtensionServiceConfig
	}

	// Values populated inside the transaction closure that are needed for
	// the HTTP response built after the closure returns.
	var createdInstancesData []instanceData
	var createdInstanceCount int

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var dpsRollback func() error

	err = cdb.WithTx(ctx, bcih.dbSession, func(tx *cdb.Tx) error {
		if bcih.cfg.GetDPSEnabled() {
			lockErr := powerutil.AcquireVPCPowerLock(ctx, tx, vpc.ID)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to serialize DPS operations for VPC")
				return cutil.NewAPIError(http.StatusConflict, "Another power operation is already in progress for the VPC", nil)
			}
			lockedVPC, lockErr := cdbm.NewVpcDAO(bcih.dbSession).GetByID(ctx, tx, vpc.ID, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to reload VPC after acquiring its power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload VPC power configuration", nil)
			}
			vpc = lockedVPC
			lockedSite, lockErr := siteDAO.GetByID(ctx, tx, vpc.SiteID, nil, false)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("siteID", vpc.SiteID.String()).Msg("failed to reload Site after acquiring the VPC power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload Site power configuration", nil)
			}
			apiErr := util.ValidateSitePowerManagement(lockedSite.Config, apiRequest.PowerProfile)
			if apiErr != nil {
				return apiErr
			}
			if apiRequest.PowerProfile != nil && vpc.PowerResourceGroup == nil {
				return cutil.NewAPIError(http.StatusBadRequest, "Power profile cannot be specified when creating Instances if VPC doesn't have power resource group populated.", nil)
			}
		}

		// ==================== Step 4: Machine Selection ====================

		// Acquire the shared quota lock for this tenant/site/instance-type pool.
		// This prevents concurrent quota mutations and admissions from racing.
		lerr := common.AcquireInstanceTypeQuotaLock(ctx, tx, tenant.ID, instancetype.ID)
		if lerr != nil {
			logger.Error().Err(lerr).Msg("Failed to acquire advisory lock on Instance Type quota pool")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error creating Instances, detected multiple parallel requests on Instance Type by Tenant", nil)
		}

		// Ensure that Tenant has an Allocation with specified Tenant InstanceType Site
		aDAO := cdbm.NewAllocationDAO(bcih.dbSession)
		allocationFilter := cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{*instancetype.SiteID}}
		allocationPage := cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}
		tnas, _, serr := aDAO.GetAll(ctx, tx, allocationFilter, allocationPage, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("error retrieving allocations for tenant")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocation for Tenant", nil)
		}
		if len(tnas) == 0 {
			return cutil.NewAPIError(http.StatusForbidden,
				"Tenant does not have any Allocations for Site and Instance Type specified in request data", nil)
		}

		alconstraints, acerr := common.GetAllocationConstraintsForInstanceType(ctx, tx, bcih.dbSession, tenant.ID, instancetype, tnas)
		if acerr != nil {
			if acerr == common.ErrAllocationConstraintNotFound {
				return cutil.NewAPIError(http.StatusInternalServerError, "No Allocations for specified Instance Type were found for current Tenant", nil)
			}
			logger.Error().Err(acerr).Msg("error retrieving Allocation Constraints from DB for InstanceType and Allocation")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocations for specified Instance Type, DB error", nil)
		}

		// Getting active instances for the tenant on requested instance type
		var siteIDs []uuid.UUID
		if instancetype.SiteID != nil {
			siteIDs = []uuid.UUID{*instancetype.SiteID}
		}
		_, insTotal, iderr := inDAO.GetAll(ctx, tx, cdbm.InstanceFilterInput{
			TenantIDs:       []uuid.UUID{tenant.ID},
			SiteIDs:         siteIDs,
			InstanceTypeIDs: []uuid.UUID{instancetype.ID},
		}, cdbp.PageInput{}, nil)
		if iderr != nil {
			logger.Error().Err(iderr).Msg("error retrieving Active Instances from DB for Tenant and InstanceType")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve active instances for Tenant and Instance Type, DB error", nil)
		}

		// Calculate the total constraint value across all matching allocations.
		totalConstraintValue := 0
		for _, alcs := range alconstraints {
			totalConstraintValue += alcs.ConstraintValue
		}

		// Check if we have enough allocation for all requested instances
		if insTotal+apiRequest.Count > totalConstraintValue {
			return cutil.NewAPIError(http.StatusForbidden,
				fmt.Sprintf("Tenant has reached the maximum number of Instances for Instance Type. Current: %d, Requested: %d, Max: %d", insTotal, apiRequest.Count, totalConstraintValue), nil)
		}

		// Allocate machines with topology optimization
		machines, apiErr := allocateMachinesForBatch(ctx, tx, bcih.dbSession, instancetype, apiRequest.Count, topologyOptimized, apiRequest.MachineLabelSelector, logger)
		if apiErr != nil {
			return apiErr
		}
		if bcih.cfg.GetDPSEnabled() && vpc.PowerResourceGroup != nil {
			assignments := make([]powerutil.MachinePowerAssignment, 0, len(machines))
			for _, machine := range machines {
				assignment := powerutil.MachinePowerAssignment{MachineID: machine.ID}
				if apiRequest.PowerProfile != nil {
					assignment.PowerProfile = *apiRequest.PowerProfile
				}
				assignments = append(assignments, assignment)
			}
			dpsRollback, serr = powerutil.ProvisionMachineBatchPower(ctx, bcih.dps, *vpc.PowerResourceGroup, assignments)
			if serr != nil {
				logger.Error().Err(serr).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("DPS rejected batch Instance allocation")
				return cutil.NewAPIError(http.StatusServiceUnavailable, "DPS rejected batch Instance power allocation", nil)
			}
		}

		// ==================== Step 5: Batch Instance Creation (Optimized with Batch DB Operations) ====================

		// --- Build all InstanceCreateInputs ---
		instanceCreateInputs := make([]cdbm.InstanceCreateInput, 0, len(machines))
		for i, machine := range machines {
			instanceCreateInputs = append(instanceCreateInputs, cdbm.InstanceCreateInput{
				Name:                     generateInstanceName(i),
				Description:              apiRequest.Description,
				TenantID:                 tenant.ID,
				InfrastructureProviderID: machine.InfrastructureProviderID,
				SiteID:                   machine.SiteID,
				VpcID:                    vpc.ID,
				MachineID:                cutil.GetPtr(machine.ID),
				OperatingSystemID:        osID,
				IpxeScript:               apiRequest.IpxeScript,
				AlwaysBootWithCustomIpxe: *apiRequest.AlwaysBootWithCustomIpxe,
				PhoneHomeEnabled:         *apiRequest.PhoneHomeEnabled,
				UserData:                 apiRequest.UserData,
				AutoNetwork:              apiRequest.AutoNetwork,
				NetworkSecurityGroupID:   apiRequest.NetworkSecurityGroupID,
				Labels:                   apiRequest.Labels,
				InstanceTypeID:           &apiInstanceTypeID,
				IsUpdatePending:          false,
				Status:                   cdbm.InstanceStatusPending,
				PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
				PowerProfile:             apiRequest.PowerProfile,
				CreatedBy:                dbUser.ID,
			})
		}

		// --- Batch create all instances ---
		createdInstances, cerr := inDAO.CreateMultiple(ctx, tx, instanceCreateInputs)
		if cerr != nil {
			logger.Error().Err(cerr).Msg("failed to batch create instance records")
			return cutil.NewAPIError(http.StatusInternalServerError,
				fmt.Sprintf("Failed to batch create instances: %v", cerr), nil)
		}
		logger.Info().Int("count", len(createdInstances)).Msg("batch created all instance records")

		// --- Set each instance's ControllerInstanceID to its own ID ---
		//
		// Each row needs a *different* ControllerInstanceID value (its own
		// ID), so this doesn't fit `UpdateMultiple`'s shared-mask /
		// shared-values contract. We issue one single-row Update per
		// instance inside the existing transaction -- atomic and cheap at
		// the batch sizes we support (capped at MaxBatchSize, ~18 today).
		updatedInstances := make([]cdbm.Instance, 0, len(createdInstances))
		for _, inst := range createdInstances {
			updated, uerr := inDAO.Update(ctx, tx, cdbm.InstanceUpdateInput{
				InstanceID: inst.ID,
				InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
					ControllerInstanceID: cutil.GetPtr(inst.ID),
				},
			})
			if uerr != nil {
				logger.Error().Err(uerr).Msg("failed to update controller instance ID")
				return cutil.NewAPIError(http.StatusInternalServerError,
					fmt.Sprintf("Failed to update Instance: %v", uerr), nil)
			}
			updatedInstances = append(updatedInstances, *updated)
		}
		logger.Info().Int("count", len(updatedInstances)).Msg("batch updated all controller instance IDs")

		// --- Build and batch create SSH Key Group Instance Associations ---
		skgiaInputs := make([]cdbm.SSHKeyGroupInstanceAssociationCreateInput, 0, len(updatedInstances)*len(sshKeyGroups))
		for _, inst := range updatedInstances {
			for _, skg := range sshKeyGroups {
				skgiaInputs = append(skgiaInputs, cdbm.SSHKeyGroupInstanceAssociationCreateInput{
					SSHKeyGroupID: skg.ID,
					SiteID:        site.ID,
					InstanceID:    inst.ID,
					CreatedBy:     dbUser.ID,
				})
			}
		}

		if len(skgiaInputs) > 0 {
			skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(bcih.dbSession)
			_, skerr := skgiaDAO.CreateMultiple(ctx, tx, skgiaInputs)
			if skerr != nil {
				logger.Error().Err(skerr).Msg("failed to batch create SSH key group associations")
				return cutil.NewAPIError(http.StatusInternalServerError,
					fmt.Sprintf("Failed to batch create SSH key group associations: %v", skerr), nil)
			}
			logger.Info().Int("count", len(skgiaInputs)).Msg("batch created all SSH key group associations")
		}

		// --- Build and batch create Interfaces ---
		ifcInputs := make([]cdbm.InterfaceCreateInput, 0, len(updatedInstances)*len(dbInterfaces))
		for _, inst := range updatedInstances {
			for _, dbifc := range dbInterfaces {
				ifcInputs = append(ifcInputs, cdbm.InterfaceCreateInput{
					InstanceID:           inst.ID,
					SubnetID:             dbifc.SubnetID,
					VpcID:                dbifc.VpcID,
					VpcIPFamilyMode:      dbifc.VpcIPFamilyMode,
					VpcPrefixID:          dbifc.VpcPrefixID,
					Device:               dbifc.Device,
					DeviceInstance:       dbifc.DeviceInstance,
					VirtualFunctionID:    dbifc.VirtualFunctionID,
					RequestedIpAddress:   dbifc.RequestedIpAddress,
					InlineRoutingProfile: dbifc.InlineRoutingProfile,
					IsPhysical:           dbifc.IsPhysical,
					Status:               cdbm.InterfaceStatusPending,
					CreatedBy:            dbUser.ID,
				})
			}
		}

		var createdIfcsAll []cdbm.Interface
		if len(ifcInputs) > 0 {
			ifcDAO := cdbm.NewInterfaceDAO(bcih.dbSession)
			ifcsCreated, ierr := ifcDAO.CreateMultiple(ctx, tx, ifcInputs)
			if ierr != nil {
				logger.Error().Err(ierr).Msg("failed to batch create interfaces")
				return cutil.NewAPIError(http.StatusInternalServerError,
					fmt.Sprintf("Failed to batch create interfaces: %v", ierr), nil)
			}
			createdIfcsAll = ifcsCreated
			logger.Info().Int("count", len(createdIfcsAll)).Msg("batch created all interfaces")
		}

		// --- Build and batch create InfiniBand Interfaces ---
		var createdIbIfcsAll []cdbm.InfiniBandInterface
		if len(dbibic) > 0 {
			ibifcInputs := make([]cdbm.InfiniBandInterfaceCreateInput, 0, len(updatedInstances)*len(dbibic))
			for _, inst := range updatedInstances {
				for _, ibic := range dbibic {
					ibifcInputs = append(ibifcInputs, cdbm.InfiniBandInterfaceCreateInput{
						InstanceID:            inst.ID,
						SiteID:                site.ID,
						InfiniBandPartitionID: ibic.InfiniBandPartitionID,
						Device:                ibic.Device,
						DeviceInstance:        ibic.DeviceInstance,
						VirtualFunctionID:     ibic.VirtualFunctionID,
						IsPhysical:            ibic.IsPhysical,
						Vendor:                ibic.Vendor,
						Status:                cdbm.InfiniBandInterfaceStatusPending,
						CreatedBy:             dbUser.ID,
					})
				}
			}

			ibifcDAO := cdbm.NewInfiniBandInterfaceDAO(bcih.dbSession)
			ibCreated, iberr := ibifcDAO.CreateMultiple(ctx, tx, ibifcInputs)
			if iberr != nil {
				logger.Error().Err(iberr).Msg("failed to batch create InfiniBand interfaces")
				return cutil.NewAPIError(http.StatusInternalServerError,
					fmt.Sprintf("Failed to batch create InfiniBand interfaces: %v", iberr), nil)
			}
			createdIbIfcsAll = ibCreated
			logger.Info().Int("count", len(createdIbIfcsAll)).Msg("batch created all InfiniBand interfaces")
		}

		// --- Build and batch create NVLink Interfaces ---
		var createdNvlIfcsAll []cdbm.NVLinkInterface
		if len(dbnvlic) > 0 {
			nvlifcInputs := make([]cdbm.NVLinkInterfaceCreateInput, 0, len(updatedInstances)*len(dbnvlic))
			for _, inst := range updatedInstances {
				for _, nvlic := range dbnvlic {
					nvlifcInputs = append(nvlifcInputs, cdbm.NVLinkInterfaceCreateInput{
						InstanceID:               inst.ID,
						SiteID:                   site.ID,
						NVLinkLogicalPartitionID: nvlic.NVLinkLogicalPartitionID,
						Device:                   nvlic.Device,
						DeviceInstance:           nvlic.DeviceInstance,
						Status:                   cdbm.NVLinkInterfaceStatusPending,
						CreatedBy:                dbUser.ID,
					})
				}
			}

			nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(bcih.dbSession)
			nvlCreated, nverr := nvlifcDAO.CreateMultiple(ctx, tx, nvlifcInputs)
			if nverr != nil {
				logger.Error().Err(nverr).Msg("failed to batch create NVLink interfaces")
				return cutil.NewAPIError(http.StatusInternalServerError,
					fmt.Sprintf("Failed to batch create NVLink interfaces: %v", nverr), nil)
			}
			createdNvlIfcsAll = nvlCreated
			logger.Info().Int("count", len(createdNvlIfcsAll)).Msg("batch created all NVLink interfaces")
		}

		// --- Build and batch create DPU Extension Service Deployments ---
		var createdDesdsAll []cdbm.DpuExtensionServiceDeployment
		if len(apiRequest.DpuExtensionServiceDeployments) > 0 {
			desdInputs := make([]cdbm.DpuExtensionServiceDeploymentCreateInput, 0, len(updatedInstances)*len(dpuServiceIDs))
			for _, inst := range updatedInstances {
				for j, desdID := range dpuServiceIDs {
					desdInputs = append(desdInputs, cdbm.DpuExtensionServiceDeploymentCreateInput{
						SiteID:                site.ID,
						TenantID:              tenant.ID,
						InstanceID:            inst.ID,
						DpuExtensionServiceID: desdID,
						Version:               apiRequest.DpuExtensionServiceDeployments[j].Version,
						Status:                cdbm.DpuExtensionServiceDeploymentStatusPending,
						CreatedBy:             dbUser.ID,
					})
				}
			}

			desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(bcih.dbSession)
			desdsCreated, derr := desdDAO.CreateMultiple(ctx, tx, desdInputs)
			if derr != nil {
				logger.Error().Err(derr).Msg("failed to batch create DPU Extension Service Deployments")
				return cutil.NewAPIError(http.StatusInternalServerError,
					fmt.Sprintf("Failed to batch create DPU Extension Service Deployments: %v", derr), nil)
			}
			createdDesdsAll = desdsCreated
			logger.Info().Int("count", len(createdDesdsAll)).Msg("batch created all DPU Extension Service Deployments")
		}

		// --- Build and batch create Status Details ---
		sdInputs := make([]cdbm.StatusDetailCreateInput, 0, len(updatedInstances))
		for _, inst := range updatedInstances {
			sdInputs = append(sdInputs, cdbm.StatusDetailCreateInput{
				EntityID: inst.ID.String(),
				Status:   cdbm.InstanceStatusPending,
				Message:  cutil.GetPtr("received instance creation request, pending"),
			})
		}

		sdDAO := cdbm.NewStatusDetailDAO(bcih.dbSession)
		createdSdsAll, sderr := sdDAO.CreateMultiple(ctx, tx, sdInputs)
		if sderr != nil {
			logger.Error().Err(sderr).Msg("failed to batch create status details")
			return cutil.NewAPIError(http.StatusInternalServerError,
				fmt.Sprintf("Failed to batch create status details: %v", sderr), nil)
		}
		logger.Info().Int("count", len(createdSdsAll)).Msg("batch created all status details")

		// --- Organize created records by instance and build workflow configs ---
		// Tracks all created data for the temporal workflow request and for
		// the post-WithTx HTTP response build.
		createdInstancesData = make([]instanceData, len(updatedInstances))

		// Initialize data structures for each instance
		for i, inst := range updatedInstances {
			instCopy := inst // Make a copy to avoid loop variable capture
			createdInstancesData[i] = instanceData{
				instance:            &instCopy,
				ifcs:                make([]cdbm.Interface, 0, len(dbInterfaces)),
				ibifcs:              make([]cdbm.InfiniBandInterface, 0, len(dbibic)),
				nvlifcs:             make([]cdbm.NVLinkInterface, 0, len(dbnvlic)),
				desds:               make([]cdbm.DpuExtensionServiceDeployment, 0, len(dpuServiceIDs)),
				interfaceConfigs:    make([]*corev1.InstanceInterfaceConfig, 0, len(dbInterfaces)),
				ibInterfaceConfigs:  make([]*corev1.InstanceIBInterfaceConfig, 0, len(dbibic)),
				nvlInterfaceConfigs: make([]*corev1.InstanceNVLinkGpuConfig, 0, len(dbnvlic)),
				desdConfigs:         make([]*corev1.InstanceDpuExtensionServiceConfig, 0, len(dpuServiceIDs)),
			}
		}

		// Build instance ID to index map for efficient lookup
		instanceIDToIdx := make(map[uuid.UUID]int, len(updatedInstances))
		for i, inst := range updatedInstances {
			instanceIDToIdx[inst.ID] = i
		}

		// Distribute Interfaces and build workflow configs
		for _, ifc := range createdIfcsAll {
			idx := instanceIDToIdx[ifc.InstanceID]
			if ifc.VpcID != nil {
				ifc.Vpc = interfaceVpcIDMap[*ifc.VpcID]
			}

			// NewAPIInstance derives SecondaryVpcIDs from VPC intent or explicit-prefix
			// relations. Reattach validated relations because CreateMultiple does not preload them.
			if ifc.VpcPrefixID != nil {
				ifc.VpcPrefix = vpcPrefixIDMap[*ifc.VpcPrefixID]
			}

			createdInstancesData[idx].ifcs = append(createdInstancesData[idx].ifcs, ifc)

			// Build temporal workflow config
			interfaceConfig := &corev1.InstanceInterfaceConfig{
				FunctionType: corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
			}
			if ifc.SubnetID != nil {
				interfaceConfig.NetworkSegmentId = &corev1.NetworkSegmentId{
					Value: subnetIDMap[*ifc.SubnetID].ControllerNetworkSegmentID.String(),
				}
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_SegmentId{
					SegmentId: &corev1.NetworkSegmentId{
						Value: subnetIDMap[*ifc.SubnetID].ControllerNetworkSegmentID.String(),
					},
				}
			}
			vpcSelection, ierr := instanceInterfaceVpcSelection(&ifc)
			if ierr != nil {
				logger.Error().Err(ierr).Msg("failed to build VPC selection for batch Instance Interface")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to build VPC selection for batch Instance Interface", nil)
			}
			if vpcSelection != nil {
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_Vpc{Vpc: vpcSelection}
			} else if ifc.VpcPrefixID != nil {
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_VpcPrefixId{
					VpcPrefixId: &corev1.VpcPrefixId{Value: ifc.VpcPrefixID.String()},
				}
			}
			if ifc.IsPhysical {
				interfaceConfig.FunctionType = corev1.InterfaceFunctionType_PHYSICAL_FUNCTION
			}
			if ifc.Device != nil && ifc.DeviceInstance != nil {
				interfaceConfig.Device = ifc.Device
				interfaceConfig.DeviceInstance = uint32(*ifc.DeviceInstance)
			}
			if !ifc.IsPhysical && ifc.VirtualFunctionID != nil {
				vfID := uint32(*ifc.VirtualFunctionID)
				interfaceConfig.VirtualFunctionId = &vfID
			}
			if ifc.InlineRoutingProfile != nil {
				interfaceConfig.RoutingProfile = ifc.InlineRoutingProfile.ToProto()
			}
			createdInstancesData[idx].interfaceConfigs = append(createdInstancesData[idx].interfaceConfigs, interfaceConfig)
		}

		// Distribute InfiniBand Interfaces and build workflow configs
		for _, ibifc := range createdIbIfcsAll {
			idx := instanceIDToIdx[ibifc.InstanceID]
			createdInstancesData[idx].ibifcs = append(createdInstancesData[idx].ibifcs, ibifc)

			// Build temporal workflow config
			ibInterfaceConfig := &corev1.InstanceIBInterfaceConfig{
				Device:         ibifc.Device,
				Vendor:         ibifc.Vendor,
				DeviceInstance: uint32(ibifc.DeviceInstance),
				FunctionType:   corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
				IbPartitionId:  &corev1.IBPartitionId{Value: ibifc.InfiniBandPartitionID.String()},
			}
			if !ibifc.IsPhysical {
				ibInterfaceConfig.FunctionType = corev1.InterfaceFunctionType_VIRTUAL_FUNCTION
				if ibifc.VirtualFunctionID != nil {
					vfID := uint32(*ibifc.VirtualFunctionID)
					ibInterfaceConfig.VirtualFunctionId = &vfID
				}
			}
			createdInstancesData[idx].ibInterfaceConfigs = append(createdInstancesData[idx].ibInterfaceConfigs, ibInterfaceConfig)
		}

		// Distribute NVLink Interfaces and build workflow configs
		for _, nvlifc := range createdNvlIfcsAll {
			idx := instanceIDToIdx[nvlifc.InstanceID]
			createdInstancesData[idx].nvlifcs = append(createdInstancesData[idx].nvlifcs, nvlifc)

			// Build temporal workflow config
			nvlInterfaceConfig := &corev1.InstanceNVLinkGpuConfig{
				DeviceInstance:     uint32(nvlifc.DeviceInstance),
				LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvlifc.NVLinkLogicalPartitionID.String()},
			}
			createdInstancesData[idx].nvlInterfaceConfigs = append(createdInstancesData[idx].nvlInterfaceConfigs, nvlInterfaceConfig)
		}

		// Distribute DPU Extension Service Deployments and build workflow configs
		for _, desd := range createdDesdsAll {
			idx := instanceIDToIdx[desd.InstanceID]
			createdInstancesData[idx].desds = append(createdInstancesData[idx].desds, desd)

			// Build temporal workflow config
			desdConfig := &corev1.InstanceDpuExtensionServiceConfig{
				ServiceId: desd.DpuExtensionServiceID.String(),
				Version:   desd.Version,
			}
			createdInstancesData[idx].desdConfigs = append(createdInstancesData[idx].desdConfigs, desdConfig)
		}

		// Distribute Status Details
		for i := range createdSdsAll {
			// Status details are created in the same order as instances
			createdInstancesData[i].ssd = &createdSdsAll[i]
		}

		createdInstanceCount = len(createdInstances)

		logger.Info().
			Int("instanceCount", len(createdInstancesData)).
			Msg("all instance records created using batch operations, now triggering batch Temporal workflow before commit")

		// ==================== Step 6: Workflow Trigger ====================

		// Get Temporal client for the site
		stc, scerr := bcih.scp.GetClientByID(site.ID)
		if scerr != nil {
			logger.Error().Err(scerr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Build batch workflow request using pre-built configs (no DB queries)
		batchRequest := &corev1.BatchInstanceAllocationRequest{
			InstanceRequests: make([]*corev1.InstanceAllocationRequest, 0, len(createdInstancesData)),
		}

		// The request carries one set of SpectrumX attachments for every Instance in the batch.
		spectrumXAttachmentConfigs := make([]*corev1.InstanceSpxAttachment, 0, len(apiRequest.SpectrumXAttachments))
		for _, sac := range apiRequest.SpectrumXAttachments {
			spectrumXAttachmentConfigs = append(spectrumXAttachmentConfigs, sac.ToProto())
		}

		for _, data := range createdInstancesData {
			instance := data.instance

			createLabels := util.ProtobufLabelsFromAPILabels(instance.Labels)

			description := ""
			if instance.Description != nil {
				description = *instance.Description
			}

			// Build instance allocation request using pre-built configs
			instanceRequest := &corev1.InstanceAllocationRequest{
				InstanceId: &corev1.InstanceId{Value: instance.GetSiteID().String()},
				MachineId:  &corev1.MachineId{Id: *instance.MachineID},
				Metadata: &corev1.Metadata{
					Name:        instance.Name,
					Description: description,
					Labels:      createLabels,
				},
				Config: &corev1.InstanceConfig{
					NetworkSecurityGroupId: instance.NetworkSecurityGroupID,
					PowerProfile:           instance.PowerProfile,
					Tenant: &corev1.TenantConfig{
						TenantOrganizationId: tenant.Org,
						TenantKeysetIds:      instanceSshKeyGroupIds,
					},
					Os:      osConfig,
					Network: buildInstanceNetworkConfig(instance.AutoNetwork, data.interfaceConfigs, vpc.ControllerVpcID),
					Infiniband: &corev1.InstanceInfinibandConfig{
						IbInterfaces: data.ibInterfaceConfigs,
					},
					DpuExtensionServices: &corev1.InstanceDpuExtensionServicesConfig{
						ServiceConfigs: data.desdConfigs,
					},
					Nvlink: &corev1.InstanceNVLinkConfig{
						GpuConfigs: data.nvlInterfaceConfigs,
					},
					Spxconfig: &corev1.InstanceSpxConfig{SpxAttachments: spectrumXAttachmentConfigs},
				},
				AllowUnhealthyMachine: false,
			}

			if instance.InstanceTypeID != nil {
				instanceRequest.InstanceTypeId = cutil.GetPtr(instance.InstanceTypeID.String())
			}

			batchRequest.InstanceRequests = append(batchRequest.InstanceRequests, instanceRequest)
		}

		logger.Info().Int("instanceCount", createdInstanceCount).
			Msg("triggering batch create Instances workflow")

		// Trigger batch workflow (use batchSuffix for consistency with instance names)
		workflowID := "instance-batch-create-" + batchSuffix
		workflowOptions := temporalClient.StartWorkflowOptions{
			ID: workflowID,
			// TODO: temporary config, to be tuned
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Add context timeout
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Execute batch workflow with full request
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateInstances", batchRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to create batch Instances")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to create batch Instances on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous batch create Instances workflow")

		// Wait for workflow to complete (synchronous, matching single API error handling)
		wferr = we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Str("Workflow ID", wid).
					Msg("failed to create batch Instances, timeout occurred executing workflow on Site.")
				timeoutCause := wferr // explicit capture; defensive against future refactors
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "Instance", "CreateInstances")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Batch Instance creation workflow timed out", nil)
			}

			// Handle other workflow errors (matching single API pattern)
			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Str("Workflow ID", wid).Msg("failed to synchronously execute Temporal workflow to create batch Instances")
			return cutil.NewAPIError(code,
				fmt.Sprintf("Failed to execute sync workflow to create batch Instances on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Int("instanceCount", createdInstanceCount).
			Msg("completed synchronous batch create Instances workflow")

		return nil
	})
	// wrapping if err != nil collapses both branches into one handler
	// call: real tx-helper errors (non-APIError) bubble out immediately,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			if dpsRollback != nil {
				rollbackErr := dpsRollback()
				if rollbackErr != nil {
					logger.Error().Err(rollbackErr).Msg("failed to compensate DPS after batch Instance creation failure")
				}
			}
			return common.HandleTxError(c, logger, err, "Failed to create batch Instances, DB transaction error")
		}
	}
	if timeoutResp != nil {
		responseErr := timeoutResp()
		if dpsRollback != nil {
			rollbackErr := dpsRollback()
			if rollbackErr != nil {
				logger.Error().Err(rollbackErr).Msg("failed to compensate DPS after batch Instance creation failure")
			}
		}
		return responseErr
	}

	// ==================== Step 7: Response ====================

	// Build complete API response with relations from the data collected inside the tx.
	apiInstances := []model.APIInstance{}
	for _, data := range createdInstancesData {
		// Build complete API instance using the same method as Single API
		sds := []cdbm.StatusDetail{}
		if data.ssd != nil {
			sds = append(sds, *data.ssd)
		}
		apiInstance := model.NewAPIInstance(data.instance, site, data.ifcs, data.ibifcs, data.desds, data.nvlifcs, sshKeyGroups, sds)

		apiInstances = append(apiInstances, *apiInstance)
	}

	logger.Info().Int("instancesCreated", len(createdInstancesData)).Msg("finishing API handler for batch instance creation")
	return c.JSON(http.StatusCreated, apiInstances)
}

// allocateMachinesForBatch allocates machines for batch instance creation with optional topology optimization.
// If topologyOptimized is true, all machines must be allocated on the same NVLink domain.
// If false, machines can be allocated across different NVLink domains without topology consideration.
//
// Returns:
//   - machines: the allocated machines
//   - error: API error if allocation fails
func allocateMachinesForBatch(
	ctx context.Context,
	tx *cdb.Tx,
	dbSession *cdb.Session,
	instancetype *cdbm.InstanceType,
	count int,
	topologyOptimized bool,
	machineLabelSelector map[string]string,
	logger zerolog.Logger,
) ([]cdbm.Machine, *cutil.APIError) {
	if instancetype == nil || count <= 0 {
		return nil, cutil.NewAPIError(http.StatusBadRequest, "Invalid parameters for machine allocation", nil)
	}

	if tx == nil {
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Transaction required for machine allocation", nil)
	}

	mcDAO := cdbm.NewMachineDAO(dbSession)

	// Get all available Machines for the Instance Type
	filterInput := cdbm.MachineFilterInput{
		InstanceTypeIDs: []uuid.UUID{instancetype.ID},
		IsAssigned:      cutil.GetPtr(false),
		Statuses:        []string{cdbm.MachineStatusReady},
		Labels:          machineLabelSelector,
	}
	machines, _, err := mcDAO.GetAll(ctx, tx, filterInput, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve available machines")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve available machines", nil)
	}

	if len(machines) < count {
		logger.Warn().Int("available", len(machines)).Int("requested", count).
			Msg("insufficient machines available")
		return nil, cutil.NewAPIError(http.StatusConflict,
			fmt.Sprintf("Insufficient machines available: requested %d, available %d", count, len(machines)), nil)
	}

	var candidateMachines []*cdbm.Machine

	// If topologyOptimized is false, no need to consider NVLink domain - just use all available machines
	if !topologyOptimized {
		logger.Info().Int("available", len(machines)).Int("requested", count).
			Msg("topology optimization disabled - allocating machines without NVLink domain constraint")
		candidateMachines = make([]*cdbm.Machine, len(machines))
		for i := range machines {
			candidateMachines[i] = &machines[i]
		}
	} else {
		// topologyOptimized is true - must allocate all machines from the same NVLink domain
		logger.Info().Int("available", len(machines)).Int("requested", count).
			Msg("topology optimization enabled - ensuring all machines from same NVLink domain")

		// Group machines by NVLink Domain ID from Metadata
		nvlinkDomainMap := make(map[string][]*cdbm.Machine)
		noNvlinkDomainMachines := []*cdbm.Machine{}

		for idx := range machines {
			machine := &machines[idx]
			domainID := getNVLinkDomainID(machine)
			if domainID != "" {
				nvlinkDomainMap[domainID] = append(nvlinkDomainMap[domainID], machine)
			} else {
				noNvlinkDomainMachines = append(noNvlinkDomainMachines, machine)
			}
		}

		// Find the NVLink domain with the most available machines
		var bestDomainID string
		for domainID, domainMachines := range nvlinkDomainMap {
			if len(domainMachines) > len(nvlinkDomainMap[bestDomainID]) {
				bestDomainID = domainID
			}
		}

		// Check if we have enough machines in a single NVLink domain
		if len(nvlinkDomainMap[bestDomainID]) < count {
			logger.Warn().Str("bestDomainID", bestDomainID).Int("bestDomainCount", len(nvlinkDomainMap[bestDomainID])).Int("requested", count).
				Msg("topology optimization requires same NVLink domain but insufficient machines in any single domain")
			return nil, cutil.NewAPIError(http.StatusConflict,
				fmt.Sprintf("Topology optimization requires all %d machines on same NVLink domain, but best domain only has %d available", count, len(nvlinkDomainMap[bestDomainID])), nil)
		}

		// Use machines from the best NVLink domain
		candidateMachines = nvlinkDomainMap[bestDomainID]
		logger.Info().Str("nvlinkDomainID", bestDomainID).Int("available", len(nvlinkDomainMap[bestDomainID])).Int("requested", count).
			Msg("allocating all machines from single NVLink domain")
	}

	// Randomize the list of machines to help distribute load and avoid bad machines
	rand.Shuffle(
		len(candidateMachines),
		func(i, j int) {
			candidateMachines[i], candidateMachines[j] = candidateMachines[j], candidateMachines[i]
		},
	)

	// Phase 1: Acquire locks and verify machines, collect update inputs
	updateInputs := make([]cdbm.MachineUpdateInput, 0, count)
	verifiedMachines := make([]*cdbm.Machine, 0, count)

	for _, mc := range candidateMachines {
		if len(verifiedMachines) >= count {
			break
		}

		// Acquire an advisory lock on the MachineID
		err = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(mc.ID), nil)
		if err != nil {
			continue
		}

		// Re-obtain the Machine record to ensure it is still available
		umc, err := mcDAO.GetByID(ctx, tx, mc.ID, nil, true)
		if err != nil {
			continue
		}

		if umc.Status != cdbm.MachineStatusReady {
			continue
		}

		if umc.IsAssigned {
			continue
		}

		if !umc.MatchesLabelSelector(machineLabelSelector) {
			continue
		}

		// Machine is verified, add to batch update list
		updateInputs = append(updateInputs, cdbm.MachineUpdateInput{
			MachineID:  mc.ID,
			IsAssigned: cutil.GetPtr(true),
		})
		verifiedMachines = append(verifiedMachines, umc)
	}

	if len(verifiedMachines) < count {
		logger.Error().Int("verified", len(verifiedMachines)).Int("requested", count).
			Msg("could not verify sufficient machines for allocation")
		return nil, cutil.NewAPIError(http.StatusConflict,
			fmt.Sprintf("Could not allocate sufficient machines: requested %d, verified %d", count, len(verifiedMachines)), nil)
	}

	// Phase 2: Batch update all verified machines to assigned
	allocatedMachines, err := mcDAO.UpdateMultiple(ctx, tx, updateInputs)
	if err != nil {
		logger.Error().Err(err).Msg("failed to batch update machines to assigned")
		return nil, cutil.NewAPIError(http.StatusInternalServerError,
			fmt.Sprintf("Failed to batch update machines: %v", err), nil)
	}

	// Log NVLink domain distribution for observability
	nvlinkDomainDistribution := make(map[string]int)
	for _, machine := range allocatedMachines {
		domainID := getNVLinkDomainID(&machine)
		nvlinkDomainDistribution[domainID]++
	}
	logger.Info().Interface("nvlinkDomainDistribution", nvlinkDomainDistribution).
		Interface("MachineLabelSelector", machineLabelSelector).
		Bool("topologyOptimized", topologyOptimized).
		Int("nvlinkDomainCount", len(nvlinkDomainDistribution)).
		Int("machinesAllocated", len(allocatedMachines)).
		Msg("successfully allocated machines for batch creation")

	return allocatedMachines, nil
}

// getNVLinkDomainID extracts the NVLink domain ID from a machine's metadata.
// Returns empty string if the machine has no NVLink domain information.
func getNVLinkDomainID(machine *cdbm.Machine) string {
	if machine.Metadata != nil {
		if nvlinkInfo := machine.Metadata.GetStatus().GetNvlinkInfo(); nvlinkInfo != nil {
			if domainUuid := nvlinkInfo.GetDomainUuid(); domainUuid != nil {
				return domainUuid.GetValue()
			}
		}
	}
	return ""
}
