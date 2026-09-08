// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"strings"
	"time"

	goset "github.com/deckarep/golang-set/v2"
	"github.com/labstack/echo/v4"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/rs/zerolog"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	powerutil "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

const (
	NVLinkInterfaceStatusSyncGraceWindow     = 90 * time.Second
	InfiniBandInterfaceStatusSyncGraceWindow = 90 * time.Second
)

// ~~~~~ Create Handler ~~~~~ //

// CreateInstanceHandler is the API Handler for creating new Instance
type CreateInstanceHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// buildInstanceNetworkConfig assembles the workflow
// InstanceNetworkConfig from the persisted auto flag and the
// per-interface configs built earlier in the handler. When auto is
// true the explicit interface list is intentionally omitted: NICo
// resolves interfaces from the host's HostInband segments, so
// sending an explicit list alongside auto=true or auto_config=...
// is contradictory (rejected by Core, and on update could otherwise
// carry forward the instance's previously-persisted interfaces).
func buildInstanceNetworkConfig(auto bool, interfaceConfigs []*corev1.InstanceInterfaceConfig, controllerVpcID *uuid.UUID) *corev1.InstanceNetworkConfig {
	nc := &corev1.InstanceNetworkConfig{Auto: auto}
	if auto {
		nc.AutoConfig = &corev1.InstanceNetworkAutoConfig{}
		if controllerVpcID != nil {
			nc.AutoConfig.VpcId = &corev1.VpcId{Value: controllerVpcID.String()}
		}
	} else {
		nc.Interfaces = interfaceConfigs
	}

	return nc
}

// loadInstanceInterfaceVpcs validates VPC-selection intent and returns each
// requested REST VPC keyed by its REST ID.
func loadInstanceInterfaceVpcs(ctx context.Context, logger *zerolog.Logger, dbSession *cdb.Session, interfaces []model.APIInterfaceCreateOrUpdateRequest, tenantID, siteID uuid.UUID) (map[uuid.UUID]*cdbm.Vpc, *cutil.APIError) {
	requestedVpcIDs := make([]uuid.UUID, 0, len(interfaces))
	seenVpcIDs := make(map[uuid.UUID]struct{}, len(interfaces))
	for _, ifc := range interfaces {
		if ifc.VpcID == nil {
			continue
		}

		vpcID, err := uuid.Parse(*ifc.VpcID)
		if err != nil {
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("VPC ID: %s specified in interfaces data in request is not valid", *ifc.VpcID), nil)
		}
		if _, ok := seenVpcIDs[vpcID]; !ok {
			seenVpcIDs[vpcID] = struct{}{}
			requestedVpcIDs = append(requestedVpcIDs, vpcID)
		}
	}

	if len(requestedVpcIDs) == 0 {
		return map[uuid.UUID]*cdbm.Vpc{}, nil
	}

	// A REST-side VPC or prefix lock was considered, but it would be held
	// across the synchronous Core call. Core owns ordering, capacity, and locks.
	vpcDAO := cdbm.NewVpcDAO(dbSession)
	vpcs, _, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{VpcIDs: requestedVpcIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve VPCs from DB by IDs")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve VPCs from DB by IDs", nil)
	}

	vpcByID := make(map[uuid.UUID]*cdbm.Vpc, len(vpcs))
	for i := range vpcs {
		vpcByID[vpcs[i].ID] = &vpcs[i]
	}

	for _, vpcID := range requestedVpcIDs {
		vpc, ok := vpcByID[vpcID]
		if !ok {
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in Interface request data is not found in DB", vpcID), nil)
		}
		if vpc.TenantID != tenantID {
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in Interface request is not owned by Tenant", vpcID), nil)
		}
		if vpc.SiteID != siteID {
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in Interface request does not belong to Site", vpcID), nil)
		}
		if vpc.ControllerVpcID == nil || vpc.Status != cdbm.VpcStatusReady {
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in Interface request data is not in Ready state", vpcID), nil)
		}
		if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
			return nil, cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in Interface request must have FNN network virtualization type", vpcID), nil)
		}
	}

	return vpcByID, nil
}

// instanceInterfaceVpcSelection converts persisted REST VPC intent to the
// Controller request shape and returns nil for non-VPC-selection interfaces.
func instanceInterfaceVpcSelection(ifc *cdbm.Interface) (*corev1.InstanceInterfaceVpcSelection, error) {
	if ifc.VpcID == nil {
		return nil, nil
	}
	if ifc.Vpc == nil || ifc.Vpc.ControllerVpcID == nil {
		return nil, fmt.Errorf("interface %s is missing its Controller VPC relation", ifc.ID)
	}
	if ifc.VpcIPFamilyMode == nil {
		return nil, fmt.Errorf("interface %s is missing its VPC IP family mode", ifc.ID)
	}

	var familyMode corev1.InstanceInterfaceIpFamilyMode
	switch *ifc.VpcIPFamilyMode {
	case cdbm.InterfaceVpcIPFamilyModeIPv4Only:
		familyMode = corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_IPV4_ONLY
	case cdbm.InterfaceVpcIPFamilyModeIPv6Only:
		familyMode = corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_IPV6_ONLY
	case cdbm.InterfaceVpcIPFamilyModeDualStack:
		familyMode = corev1.InstanceInterfaceIpFamilyMode_INSTANCE_INTERFACE_IP_FAMILY_MODE_DUAL_STACK
	default:
		return nil, fmt.Errorf("interface %s has unsupported VPC IP family mode %q", ifc.ID, *ifc.VpcIPFamilyMode)
	}

	return &corev1.InstanceInterfaceVpcSelection{
		VpcId:      &corev1.VpcId{Value: ifc.Vpc.ControllerVpcID.String()},
		FamilyMode: familyMode,
	}, nil
}

// NewCreateInstanceHandler initializes and returns a new handler for creating Instance
func NewCreateInstanceHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) CreateInstanceHandler {
	return CreateInstanceHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		dps:        dps,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// validateTemplatedIpxeOsForSite guards the Templated iPXE Operating System
// selection paths (Instance create / update / batch-create) before the OS ID is
// sent to Core. Caller authorization and tenant/OS access are already enforced
// by the handlers (ValidateOrgMembership / ValidateUserRoles) and the per-request
// usability check, so this enforces the site-availability contract specific to
// templated OSes: the OS definition must be synchronized to the Instance's Site
// (a Synced OperatingSystemSiteAssociation) so the Site can render the template
// at provisioning time.
//
// This is intentionally independent of how the OS was created or which side owns
// its definition: single-site OSes sync bidirectionally with nico-core, while
// multi-site OSes are REST-owned and pushed out to their Sites. In every case a
// Synced association is the signal that the definition is actually available at
// the Site, so we gate on that alone.
func validateTemplatedIpxeOsForSite(ctx context.Context, dbSession *cdb.Session, logger *zerolog.Logger, os *cdbm.OperatingSystem, siteID uuid.UUID) *cutil.APIError {
	ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(dbSession)
	_, ossaCount, err := ossaDAO.GetAll(
		ctx,
		nil,
		cdbm.OperatingSystemSiteAssociationFilterInput{
			OperatingSystemIDs: []uuid.UUID{os.ID},
			SiteIDs:            []uuid.UUID{siteID},
			Statuses:           []string{cdbm.OperatingSystemSiteAssociationStatusSynced},
		},
		cdbp.PageInput{Limit: cutil.GetPtr(1)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Str("operatingSystemId", os.ID.String()).Msg("error retrieving OperatingSystemSiteAssociations for Templated iPXE OS")
		return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve OperatingSystem site associations, DB error", nil)
	}
	if ossaCount == 0 {
		logger.Warn().Str("operatingSystemId", os.ID.String()).Str("siteId", siteID.String()).Msg("Templated iPXE Operating System is not synchronized to the Instance's Site")
		return cutil.NewAPIError(http.StatusBadRequest, "Templated iPXE Operating System specified in request is not synchronized to the Instance's Site", nil)
	}

	return nil
}

// Returns either a default OS or an existing instance OS config.
// apiRequest will be mutated for use in create.
// osConfig will hold the struct/data for use with Temporal/NICo calls.
// Errors should be returned in the form of cutil.NewAPIErrorResponse
func (cih CreateInstanceHandler) buildInstanceCreateRequestOsConfig(c echo.Context, logger *zerolog.Logger, apiRequest *model.APIInstanceCreateRequest, site *cdbm.Site) (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError) {

	ctx := c.Request().Context()

	// If no OS was selected
	if apiRequest.OperatingSystemID == nil || *apiRequest.OperatingSystemID == "" {

		if err := apiRequest.ValidateAndSetOperatingSystemData(cih.cfg, nil); err != nil {
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
	osDAO := cdbm.NewOperatingSystemDAO(cih.dbSession)
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
	if os.Type == cdbm.OperatingSystemTypeImage {
		ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(cih.dbSession)
		_, ossaCount, err := ossaDAO.GetAll(
			ctx,
			nil,
			cdbm.OperatingSystemSiteAssociationFilterInput{
				OperatingSystemIDs: []uuid.UUID{id},
				SiteIDs:            []uuid.UUID{site.ID},
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
	}

	// Validate any additional properties.
	// `os` could still be nil here if no OS ID was sent
	// in the request.

	err = apiRequest.ValidateAndSetOperatingSystemData(cih.cfg, os)
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
		if apiErr := validateTemplatedIpxeOsForSite(ctx, cih.dbSession, logger, os, site.ID); apiErr != nil {
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
// @Summary Create an Instance
// @Description Create an Instance for the org.
// @Tags Instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIInstanceCreateRequest true "Instance create request"
// @Success 201 {object} model.APIInstance
// @Router /v2/org/{org}/nico/instance [post]
func (cih CreateInstanceHandler) Handle(c echo.Context) error {
	// Execution Steps:
	// 1. Authentication & Authorization
	//    - Extract user from context
	//    - Validate org membership
	//    - Validate Tenant Admin role
	// 2. Request Validation
	//    - Bind and validate request data
	//    - Validate tenant, VPC, site
	//    - Load and validate Interfaces (Subnets, VPC Prefixes, or VPC selection)
	//    - Load and validate DPU Extension Service Deployments
	//    - Load and validate Network Security Groups
	//    - Load and validate SSH Key Groups
	//    - Validate OS or iPXE script
	//    - Check instance name uniqueness
	// 3. Database Transaction
	//    - Start transaction
	// 4. Machine Selection
	//    - Path A: Machine ID specified → validate and assign specific machine
	//    - Path B: Instance Type ID specified → acquire advisory lock, verify allocation constraints, find available machine
	// 5. Machine Capability Validation
	//    - Validate InfiniBand interfaces against Instance Type capabilities
	//    - Validate InfiniBand partitions (Site, Tenant, Status)
	//    - Validate DPU interfaces against Instance Type capabilities
	//    - Validate NVLink interfaces against Instance Type capabilities
	//    - Validate NVLink logical partitions (Site, Tenant, Status)
	// 6. Create Instance Records
	//    - Create Instance record
	//    - Update ControllerInstanceID
	//    - Create SSH Key Group associations
	//    - Create Interface records
	//    - Create InfiniBand Interface records
	//    - Create NVLink Interface records
	//    - Create DPU Extension Service Deployment records
	//    - Create status detail record
	// 7. Workflow Trigger
	//    - Build instance allocation request with all configs
	//    - Execute synchronous Temporal workflow (CreateInstanceV2)
	//    - Wait for site-agent to provision the instance
	//    - Handle timeout with workflow termination
	// 8. Commit & Response
	//    - Commit transaction after workflow succeeds
	//    - Return created instance to client

	// ==================== Step 1: Authentication & Authorization ====================

	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Instance", "Create", c, cih.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
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
	apiRequest := model.APIInstanceCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Instance creation request data", verr)
	}
	logger.Info().Interface("MachineLabelSelector", apiRequest.MachineLabelSelector).
		Msg("validated Instance creation placement selector")

	// Validate the tenant for which this Instance is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, cih.dbSession, org)
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

	apiTenant, err := common.GetTenantFromIDString(ctx, nil, apiRequest.TenantID, cih.dbSession)
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving tenant from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "TenantID in request is not valid", nil)
	}
	if apiTenant.ID != tenant.ID {
		logger.Warn().Msg("tenant id in request does not match tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "TenantID in request does not match tenant in org", nil)
	}

	// Validate the VPC state
	vpc, err := common.GetVpcFromIDString(ctx, nil, apiRequest.VpcID, []string{cdbm.NVLinkLogicalPartitionRelationName}, cih.dbSession)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find VPC with ID specified in request data", nil)
		}
		logger.Warn().Err(err).Msg("error retrieving VPC from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VpcID in request is not valid", nil)
	}

	if vpc.TenantID != tenant.ID {
		logger.Warn().Msg("tenant id in request does not match tenant in VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC specified in request is not owned by Tenant", nil)
	}

	if vpc.ControllerVpcID == nil || vpc.Status != cdbm.VpcStatusReady {
		logger.Warn().Msg("VPC specified in request data is not ready")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC specified in request data is not ready", nil)
	}
	// Validate request fields that depend on the resolved VPC (e.g.
	// `autoNetwork` requires a Flat VPC).
	verr = apiRequest.ValidateForVpc(vpc)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance creation request against VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Instance creation request data", verr)
	}

	var defaultNvllpID *uuid.UUID
	if vpc.NVLinkLogicalPartitionID != nil {
		// NOTE: No validation needed here because the VPC validation ensures the NVLink Logical Partition is valid for this instance
		defaultNvllpID = vpc.NVLinkLogicalPartitionID
	}

	// Verify if site is ready
	stDAO := cdbm.NewSiteDAO(cih.dbSession)
	site, err := stDAO.GetByID(ctx, nil, vpc.SiteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where this Instance is being created could not be found", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "The Site where this Instance is being created could not be retrieved", nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("The Site: %v where this Instance is being created is not in Registered state", vpc.SiteID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where this Instance is being created is not in Registered state", nil)
	}
	if apiErr := util.ValidateSitePowerManagement(site.Config, apiRequest.PowerProfile); apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if cih.cfg.GetDPSEnabled() && apiRequest.PowerProfile != nil && vpc.PowerResourceGroup == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Power profile cannot be specified when creating Instances if VPC doesn't have power resource group populated.", nil)
	}
	apiErr := model.ValidatePowerProfile(ctx, cih.cfg.GetDPSEnabled(), cih.dps, apiRequest.PowerProfile)
	if apiErr != nil {
		logger.Warn().Err(apiErr.Diagnosis()).Msg("failed to validate Instance power profile")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	// Begin validating interfaces
	// Fetch and validate Subnets, VPC Prefixes, and VPC selections
	sbDAO := cdbm.NewSubnetDAO(cih.dbSession)
	vpDAO := cdbm.NewVpcPrefixDAO(cih.dbSession)

	subnetIDs := []uuid.UUID{}
	vpcPrefixIDs := []uuid.UUID{}
	subnetIfcMap := map[uuid.UUID]int{}
	vpcPrefixIfcMap := map[uuid.UUID]int{}

	for _, ifc := range apiRequest.Interfaces {
		if ifc.SubnetID != nil {
			subnetID, err := uuid.Parse(*ifc.SubnetID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing subnet id in instance subnet request")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Subnet ID: %s specified in interfaces data in request is not valid", *ifc.SubnetID), nil)
			}
			subnetIDs = append(subnetIDs, subnetID)
			subnetIfcMap[subnetID]++
		}
		if ifc.VpcPrefixID != nil {
			vpcPrefixID, err := uuid.Parse(*ifc.VpcPrefixID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing vpcprefix id in instance vpcprefix request")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix ID: %s specified in interfaces data in request is not valid", *ifc.VpcPrefixID), nil)
			}
			vpcPrefixIDs = append(vpcPrefixIDs, vpcPrefixID)
			vpcPrefixIfcMap[vpcPrefixID]++
		}
	}

	// Fetch Subnets from DB by IDs
	subnetIDMap := make(map[uuid.UUID]*cdbm.Subnet)
	if len(subnetIDs) > 0 {
		subnets, _, err := sbDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{SubnetIDs: subnetIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Subnets from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnets from DB by IDs", nil)
		}
		for i := range subnets {
			subnetIDMap[subnets[i].ID] = &subnets[i]
		}
	}

	// Fetch VPC Prefixes from DB by IDs
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
	interfaceVpcIDMap, interfaceVpcErr := loadInstanceInterfaceVpcs(ctx, &logger, cih.dbSession, apiRequest.Interfaces, tenant.ID, site.ID)
	if interfaceVpcErr != nil {
		logger.Warn().Err(interfaceVpcErr).Msg("failed to validate VPCs specified by Instance interfaces")
		return cutil.NewAPIErrorResponse(c, interfaceVpcErr.Code, interfaceVpcErr.Message, interfaceVpcErr.Data)
	}

	dbInterfaces := []cdbm.Interface{}
	isInterfaceDeviceInfoPresent := false

	pfWithinVPC := []uuid.UUID{}
	primaryPhysicalInterfaceUsesVpcSelection := false
	allFoundVpcIds := goset.NewSet[uuid.UUID]()

	// Prepare the unique set of all VPC IDs for this instance.
	allRequestedVpcIds := goset.NewSet[uuid.UUID]()
	for _, vpcID := range apiRequest.SecondaryVpcIDs {
		id, err := uuid.Parse(vpcID)
		if err != nil {
			logger.Error().Msgf("invalid VPC ID %v in `secondaryVpcIds` in request data", vpcID)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid VPC ID `%s` in secondaryVpcIds in request data", vpcID), nil)
		}

		if !allRequestedVpcIds.Add(id) {
			logger.Error().Msgf("duplicate ID %s found in `secondaryVpcIds`", id)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Duplicate ID `%s` found in `secondaryVpcIds`", id), nil)
		}
	}

	// Now add the primary VPC to the list.
	// If add failed, it means the ID already existed, but
	// the primary VPC of the instance shouldn't exist in the secondary list.
	if !allRequestedVpcIds.Add(vpc.ID) {
		logger.Error().Msgf("primary VPC ID: %s for Instance must not be listed in `secondaryVpcIds`", vpc.ID)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Primary VPC ID: %s for Instance must not be listed in `secondaryVpcIds`", vpc.ID), nil)
	}

	subnetsForUsage := make([]*cdbm.Subnet, 0, len(subnetIfcMap))
	for subnetID := range subnetIfcMap {
		if sn, ok := subnetIDMap[subnetID]; ok {
			subnetsForUsage = append(subnetsForUsage, sn)
		}
	}
	subnetUsageMap, usageErr := sbDAO.GetPrefixUsage(ctx, nil, subnetsForUsage...)
	if usageErr != nil {
		logger.Error().Err(usageErr).Msg("error getting prefix usage for Subnets")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to get prefix usage for Subnet", nil)
	}

	vpcPrefixesForUsage := make([]*cdbm.VpcPrefix, 0, len(vpcPrefixIfcMap))
	for vpcPrefixID := range vpcPrefixIfcMap {
		if vp, ok := vpcPrefixIDMap[vpcPrefixID]; ok {
			vpcPrefixesForUsage = append(vpcPrefixesForUsage, vp)
		}
	}
	vpcPrefixUsageMap, vpUsageErr := vpDAO.GetPrefixUsage(ctx, nil, vpcPrefixesForUsage...)
	if vpUsageErr != nil {
		logger.Error().Err(vpUsageErr).Msg("error getting prefix usage for VPC Prefixes")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to get prefix usage for VPC Prefix", nil)
	}

	for _, ifc := range apiRequest.Interfaces {
		if ifc.SubnetID != nil {
			subnetID := uuid.MustParse(*ifc.SubnetID)

			subnet, ok := subnetIDMap[subnetID]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("Subnet: %v specified in request data is not found in DB", subnetID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Subnet: %v specified in request data is not found in DB", subnetID), nil)
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

			// Check if Subnet is exhausted
			incomingInterfaceIPs := subnetIfcMap[subnetID]
			subnetUsage := subnetUsageMap[subnetID]
			if subnetUsage != nil && subnetUsage.AvailableIPs > 0 && subnetUsage.AcquiredIPs+uint64(incomingInterfaceIPs) > subnetUsage.AvailableIPs {
				msg := fmt.Sprintf(
					"Subnet %v does not have enough IP addresses: %d of %d IP addresses remain available, but the %d interface(s) in this request require %d IP address(es)",
					subnetID, subnetUsage.AvailableIPs-subnetUsage.AcquiredIPs, subnetUsage.AvailableIPs, incomingInterfaceIPs, incomingInterfaceIPs,
				)
				logger.Warn().Msg(msg)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				SubnetID:           &subnetID,
				Subnet:             subnet,
				IsPhysical:         ifc.IsPhysical,
				RequestedIpAddress: nil, // RequestedIpAddress requires a VPC prefix, and model validation enforces this.
				Status:             cdbm.InterfaceStatusPending,
			})
		}

		if ifc.VpcPrefixID != nil {
			vpcPrefixID := uuid.MustParse(*ifc.VpcPrefixID)

			vpcPrefix, ok := vpcPrefixIDMap[vpcPrefixID]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request data is not found in DB", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request data is not found in DB", vpcPrefixID), nil)
			}

			if vpcPrefix.TenantID != tenant.ID {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request is not owned by Tenant", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request is not owned by Tenant", vpcPrefixID), nil)
			}

			if vpcPrefix.Status != cdbm.VpcPrefixStatusReady {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefixID), nil)
			}

			if vpcPrefix.SiteID != site.ID {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixID), nil)
			}

			// If the interface is associated with a VPC ID that the user
			// didn't expect, reject the request.
			if !allRequestedVpcIds.Contains(vpcPrefix.VpcID) {
				logger.Error().Msgf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix.ID, vpcPrefix.VpcID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix.ID, vpcPrefix.VpcID), nil)
			}

			// Collect the VPC IDs actually found based on
			// interface definitions.
			allFoundVpcIds.Add(vpcPrefix.VpcID)

			if ifc.Device != nil && ifc.DeviceInstance != nil {
				isInterfaceDeviceInfoPresent = true
			}

			// The requirement that the VpcID of a prefix being associated with an interface must match the VPC of the instance
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
				if !isInterfaceDeviceInfoPresent {
					pfWithinVPC = append(pfWithinVPC, vpcPrefix.VpcID)
					primaryPhysicalInterfaceUsesVpcSelection = false
				} else if ifc.DeviceInstance != nil && *ifc.DeviceInstance == 0 {
					pfWithinVPC = []uuid.UUID{vpcPrefix.VpcID}
					primaryPhysicalInterfaceUsesVpcSelection = false
				}
			}

			if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", vpc.ID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", vpc.ID), nil)
			}

			// Check if VPC Prefix is exhausted
			incomingInterfaceIPs := vpcPrefixIfcMap[vpcPrefixID]
			vpUsage := vpcPrefixUsageMap[vpcPrefixID]
			if vpUsage != nil && vpUsage.AvailableIPs > 0 && vpUsage.AcquiredIPs+uint64(incomingInterfaceIPs)*2 > vpUsage.AvailableIPs {
				msg := fmt.Sprintf(
					"VPC Prefix %v does not have enough IP addresses: %d of %d IP addresses remain available, but the %d interface(s) in this request require %d IP addresses",
					vpcPrefixID, vpUsage.AvailableIPs-vpUsage.AcquiredIPs, vpUsage.AvailableIPs, incomingInterfaceIPs, incomingInterfaceIPs*2,
				)
				logger.Warn().Msg(msg)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				VpcPrefixID:          &vpcPrefixID,
				VpcPrefix:            vpcPrefix, // We attach this here so it can be used when we convert to the API model.
				RequestedIpAddress:   ifc.IPAddress,
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
				isInterfaceDeviceInfoPresent = true
			}
			if ifc.IsPhysical {
				if !isInterfaceDeviceInfoPresent {
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

	// If there are ethernet interfaces for this Instance,
	// validate the network plan.
	if len(dbInterfaces) > 0 &&
		vpc.NetworkVirtualizationType != nil &&
		*vpc.NetworkVirtualizationType == cdbm.VpcFNN {
		// Throw an error if there are somehow no PFs (shouldn't
		// be possible at this point), or if the VPC of the first
		// PF doesn't match the (primary) VPC of the instance.
		if len(pfWithinVPC) == 0 || pfWithinVPC[0] != vpc.ID {
			// Use the VPC-selection response when the primary physical Interface selects a VPC
			// by ID. If no primary physical Interface was found, any Interface selecting a VPC
			// by ID is enough to prefer this response over the legacy VPC Prefix response.
			if primaryPhysicalInterfaceUsesVpcSelection || (len(pfWithinVPC) == 0 && len(interfaceVpcIDMap) > 0) {
				logger.Error().Msg("the primary physical interface must use the Instance VPC")
				if !isInterfaceDeviceInfoPresent {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface must use the VPC specified in `vpcId`", nil)
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface for deviceInstance: 0 must use the VPC specified in `vpcId`", nil)
			}

			logger.Error().Msg("the primary physical interface must use a VPC prefix that matches with Instance VPC")

			if !isInterfaceDeviceInfoPresent {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface must use a VPC Prefix that belongs to VPC specified in `vpcId`", nil)
			} else {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The primary physical Interface for deviceInstance: 0 must use a VPC Prefix that belongs to VPC specified in `vpcId`", nil)
			}
		}
		// Reject the requeste if the planned VPC associations doesn't match
		// the reality of the VPC associations found based on interface
		// definitions.
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

	// End validating interfaces

	// Begin validating DPU Extension Service Deployments
	desIDs := []uuid.UUID{}
	for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
		desID, err := uuid.Parse(adesdr.DpuExtensionServiceID)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid DPU Extension Service ID: %s specified in request", adesdr.DpuExtensionServiceID), nil)
		}
		desIDs = append(desIDs, desID)
	}

	desDAO := cdbm.NewDpuExtensionServiceDAO(cih.dbSession)
	desIDMap := map[uuid.UUID]*cdbm.DpuExtensionService{}
	if len(desIDs) > 0 {
		dess, _, err := desDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceFilterInput{DpuExtensionServiceIDs: desIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving DPU Extension Services from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DPU Extension Services from DB by IDs", nil)
		}
		for i := range dess {
			desIDMap[dess[i].ID] = &dess[i]
		}
	}

	for _, apiDesd := range apiRequest.DpuExtensionServiceDeployments {
		desID := uuid.MustParse(apiDesd.DpuExtensionServiceID)

		des, ok := desIDMap[desID]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("DPU Extension Service: %v specified in request data is not found in DB", desID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("DPU Extension Service: %v specified in request data is not found in DB", desID), nil)
		}

		if des.TenantID != tenant.ID {
			logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("DPU Extension Service ID", desID.String()).Msg("DPU Extension Service does not belong to current Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("DPU Extension Service: %s does not belong to current Tenant", desID.String()), nil)
		}

		if des.SiteID != site.ID {
			logger.Warn().Str("Site ID", site.ID.String()).Str("DPU Extension Service ID", desID.String()).Msg("DPU Extension Service does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("DPU Extension Service: %s does not belong to Site where Instance is being created", desID.String()), nil)
		}

		versionFound := false
		for _, version := range des.ActiveVersions {
			if version == apiDesd.Version {
				versionFound = true
				break
			}
		}
		if !versionFound {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Version: %s was not found for DPU Extension Service: %s", apiDesd.Version, desID.String()), nil)
		}
	}
	// End validating DPU Extension Service Deployments

	// Begin validating Network Security Group
	if apiRequest.NetworkSecurityGroupID != nil {
		nsgDAO := cdbm.NewNetworkSecurityGroupDAO(cih.dbSession)

		nsg, err := nsgDAO.GetByID(ctx, nil, *apiRequest.NetworkSecurityGroupID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find NetworkSecurityGroup with ID: %s specified in request", *apiRequest.NetworkSecurityGroupID), nil)
			}

			logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroup with ID specified in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NetworkSecurityGroup with ID specified in request data", nil)
		}

		if nsg.SiteID != site.ID {
			logger.Error().Msg("NetworkSecurityGroup in request does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NetworkSecurityGroup with ID specified in request data does not belong to Site", nil)
		}

		if nsg.TenantID != tenant.ID {
			logger.Error().Msg("NetworkSecurityGroup in request does not belong to Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NetworkSecurityGroup with ID specified in request data does not belong to Tenant", nil)
		}
	}
	// End validating Network Security Group

	// Begin validating SSH Key Group
	sshKeyGroupIDs := []uuid.UUID{}
	for _, skgStrID := range apiRequest.SSHKeyGroupIDs {
		skgID, err := uuid.Parse(skgStrID)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid SSH Key Group ID: %s specified in request", skgStrID), nil)
		}
		sshKeyGroupIDs = append(sshKeyGroupIDs, skgID)
	}

	skgDAO := cdbm.NewSSHKeyGroupDAO(cih.dbSession)
	skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(cih.dbSession)

	sshKeyGroupIDMap := map[uuid.UUID]*cdbm.SSHKeyGroup{}
	skgs := []cdbm.SSHKeyGroup{}
	skgsas := []cdbm.SSHKeyGroupSiteAssociation{}
	if len(sshKeyGroupIDs) > 0 {
		var err error
		skgs, _, err = skgDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupFilterInput{SSHKeyGroupIDs: sshKeyGroupIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving SSH Key Groups from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Groups from DB by IDs", nil)
		}
		for i := range skgs {
			sshKeyGroupIDMap[skgs[i].ID] = &skgs[i]
		}

		skgsas, _, err = skgsaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupSiteAssociationFilterInput{
			SSHKeyGroupIDs: sshKeyGroupIDs,
			SiteID:         &site.ID,
		}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving SSH Key Group Site Associations from DB by SSH Key Group IDs & Site ID")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Group Site Associations from DB", nil)
		}
	}

	skgSiteAssociationIDMap := map[uuid.UUID]*cdbm.SSHKeyGroupSiteAssociation{}
	for _, skgsa := range skgsas {
		skgSiteAssociationIDMap[skgsa.SSHKeyGroupID] = &skgsa
	}

	for _, skgStrID := range apiRequest.SSHKeyGroupIDs {
		skgID := uuid.MustParse(skgStrID)

		skg, ok := sshKeyGroupIDMap[skgID]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("SSH Key Group: %v specified in request data is not found in DB", skgID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("SSH Key Group: %s specified in request data was not found in DB", skgID.String()), nil)
		}

		if skg.TenantID != tenant.ID {
			logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("SSH Key Group ID", skgID.String()).Msg("SSH Key Group does not belong to current Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create Instance, SSH Key Group with ID: %s does not belong to Tenant", skgID), nil)
		}

		// Verify if SSH Key Group Site Association exists
		_, ok = skgSiteAssociationIDMap[skg.ID]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("SSH Key Group: %s specified in request data is not associated with the Site where Instance is being created", skgID.String()))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("SSH Key Group: %s specified in request data is not associated with the Site where Instance is being created", skgID.String()), nil)
		}
	}

	// apiRequest will be mutated for use in CreateFromParams.
	// osConfig will hold the struct/data for use with Temporal/NICo calls.
	// Errors will be returned already in the form of cutil.NewAPIErrorResponse
	osConfig, osID, oserr := cih.buildInstanceCreateRequestOsConfig(c, &logger, &apiRequest, site)
	if oserr != nil {
		// buildInstanceCreateRequestOsConfig already handles logging,
		// so this is a bit redundant, but this log brings you to the
		// actual call site.  I think buildInstanceCreateRequestOsConfig
		// would ideally return only `error` and let the logging and
		// and cutil.NewAPIErrorResponse(...) happen here, but we
		// have at least one StatusInternalServerError case that would
		// be hidden if we merge it all under StatusBadRequest here.
		logger.Error().Err(errors.New(oserr.Message)).Msg("error building os config for creating Instance")
		return c.JSON(oserr.Code, oserr)
	}

	// Ensure we have one and only one of InstanceTypeID or MachineID
	if apiRequest.InstanceTypeID != nil && apiRequest.MachineID != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Either InstanceType ID or Machine ID must be specified, but not both", nil)
	} else if apiRequest.InstanceTypeID == nil && apiRequest.MachineID == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Either InstanceType ID or Machine ID must be specified", nil)
	}

	// Common pre-requisites for both InstanceType and Machine ID cases
	var instanceTypeID *uuid.UUID
	var machine *cdbm.Machine

	instanceDAO := cdbm.NewInstanceDAO(cih.dbSession)

	// Check for name uniqueness for the tenant, ie, tenant cannot have another instance with same name at the site
	// TODO: Consider doing this with an advisory lock for correctness
	instances, matchCount, err := instanceDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{Names: []string{apiRequest.Name}, TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{vpc.SiteID}}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Instance due to DB error", nil)
	}
	if matchCount > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.Name).Msg("instance with same name already exists for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "An Instance with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(instances[0].ID.String()),
		})
	}

	// ==================== Step 3: Database Transaction ====================

	// Default to false, will be set to true for data sent to Core if allowUnhealthyMachine is set to true in request
	allowUnhealthyMachine := false

	// Values populated inside the transaction closure that are needed for the response.
	var instance *cdbm.Instance
	var ifcs []cdbm.Interface
	var ibifcs []cdbm.InfiniBandInterface
	var desds []cdbm.DpuExtensionServiceDeployment
	var nvlifcs []cdbm.NVLinkInterface
	var ssd *cdbm.StatusDetail

	// Validate the targeted instance creation capability before opening the
	// transaction so no writes or locks happen for an unauthorized request.
	// A non-empty label selector can narrow placement to a single Machine, so it
	// require the same privilege as an explicit Machine ID.
	if apiRequest.MachineID != nil || len(apiRequest.MachineLabelSelector) > 0 {
		privilegedAccess, derr := common.TenantHasTargetedInstanceCreation(ctx, nil, cih.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
		if derr != nil {
			logger.Error().Err(derr).Msg("error checking effective targeted instance creation for Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify capability for Site", nil)
		}
		if !privilegedAccess {
			if apiRequest.MachineID != nil {
				logger.Warn().Msg("tenant does not have capability to create instances from specific machine")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have capability to create Instances using specific Machine ID", nil)
			}

			logger.Warn().Msg("tenant does not have capability to create instances using Machine label selector")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have capability to create Instances using Machine label selector", nil)
		}
	}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var dpsRollback func() error

	err = cdb.WithTx(ctx, cih.dbSession, func(tx *cdb.Tx) error {
		if cih.cfg.GetDPSEnabled() {
			lockErr := powerutil.AcquireVPCPowerLock(ctx, tx, vpc.ID)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to serialize DPS operations for VPC")
				return cutil.NewAPIError(http.StatusConflict, "Another power operation is already in progress for the VPC", nil)
			}
			lockedVPC, lockErr := cdbm.NewVpcDAO(cih.dbSession).GetByID(ctx, tx, vpc.ID, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to reload VPC after acquiring its power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload VPC power configuration", nil)
			}
			vpc = lockedVPC
			lockedSite, lockErr := stDAO.GetByID(ctx, tx, vpc.SiteID, nil, false)
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

		// ==================== Step 4: Machine Selection  ====================

		// Begin validating Machine ID
		if apiRequest.MachineID != nil {
			mDAO := cdbm.NewMachineDAO(cih.dbSession)

			// Acquire a lock on the MachineID
			err = tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(*apiRequest.MachineID), nil)
			if err != nil {
				logger.Error().Err(err).Msg("Failed to acquire advisory lock on Machine")
				return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to lock Machine: %s for Instance creation. It is likely being considered for another Instance creation request", *apiRequest.MachineID), nil)
			}

			// Retrieve Machine by ID
			machine, err = mDAO.GetByID(ctx, tx, *apiRequest.MachineID, nil, true)
			if err != nil {
				if err == cdb.ErrDoesNotExist {
					return cutil.NewAPIError(http.StatusBadRequest, "Could not find Machine with ID specified in request data", nil)
				}
				logger.Error().Err(err).Msg("error retrieving Machine from DB by ID")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine with ID specified in request data", nil)
			}

			// Validate that the Machine is part of the Site
			if machine.SiteID != site.ID {
				logger.Warn().Msg("Machine specified in request is not part of the site")
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine specified in request does not belong to Site: %s", site.Name), nil)
			}

			if !machine.MatchesLabelSelector(apiRequest.MachineLabelSelector) {
				logger.Warn().Str("MachineID", machine.ID).
					Interface("MachineLabelSelector", apiRequest.MachineLabelSelector).
					Msg("Machine specified in request does not match Machine label selector")
				return cutil.NewAPIError(http.StatusBadRequest,
					"Machine specified in request does not match machineLabelSelector", nil)
			}

			// Validate Machine availability. Note: allowUnhealthyMachine also bypasses
			// the Ready status check, not just health - consider renaming the parameter later.
			if apiRequest.AllowUnhealthyMachine != nil {
				allowUnhealthyMachine = *apiRequest.AllowUnhealthyMachine
			}

			// Check if Machine is missing on site
			if machine.IsMissingOnSite {
				logger.Warn().Str("MachineID", machine.ID).Msg("Machine is missing on site, cannot be used for new Instance")
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %s is missing on site, cannot be used for new Instance", machine.ID), nil)
			}

			// Always check if Machine is already assigned
			if machine.IsAssigned {
				logger.Warn().Str("MachineID", machine.ID).Bool("AllowUnhealthyMachine", allowUnhealthyMachine).Msg("Machine is already assigned to an Instance, cannot be used for new Instance")
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %s is assigned to an Instance, cannot be used for new Instance", machine.ID), nil)
			}

			// Check if it's possible to provision the Machine
			if machine.Status == cdbm.MachineStatusReady {
				logger.Info().Str("MachineID", machine.ID).Str("Status", machine.Status).Bool("AllowUnhealthyMachine", allowUnhealthyMachine).Msg("Machine is Ready, proceeding with Instance creation")
			} else {
				isProvisionable := false

				// Get the controller state from the machine
				controllerState := machine.GetControllerState()
				if strings.HasPrefix(controllerState, cdbm.MachineStatusReady) {
					isProvisionable = true
				}

				mlogger := logger.With().Str("MachineID", machine.ID).Str("Status", machine.Status).Str("ControllerState", controllerState).Bool("AllowUnhealthyMachine", allowUnhealthyMachine).Logger()

				if allowUnhealthyMachine {
					if isProvisionable {
						mlogger.Info().Msg("Machine is provisionable, proceeding with Instance creation")
					} else {
						if machine.Status == cdbm.MachineStatusMaintenance || machine.Status == cdbm.MachineStatusError {
							mlogger.Warn().Msg("Machine has controller state that does not allow Instance creation")
							return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %s has controller state: %s that does not allow Instance creation even with `allowUnhealthyMachine` set to true", machine.ID, controllerState), nil)
						} else {
							mlogger.Warn().Msg("Machine has status that does not allow Instance creation")
							return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %s has status: %s that does not allow Instance creation even with `allowUnhealthyMachine` set to true", machine.ID, machine.Status), nil)
						}
					}
				} else {
					if isProvisionable {
						mlogger.Warn().Msg("Machine is not in Ready state, but it can be provisioned by setting `allowUnhealthyMachine` to true in request")
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %s is not in Ready state, but it can be provisioned by setting `allowUnhealthyMachine` to true in request", machine.ID), nil)
					} else {
						mlogger.Warn().Msg("Machine has status that does not allow Instance creation")
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Machine: %s has status: %s that does not allow Instance creation", machine.ID, machine.Status), nil)
					}
				}
			}

			// Update the machine status to assigned
			updateInput := cdbm.MachineUpdateInput{
				MachineID:  machine.ID,
				IsAssigned: cutil.GetPtr(true),
			}
			machine, err = mDAO.Update(ctx, tx, updateInput)
			if err != nil {
				if err == cdb.ErrDoesNotExist {
					return cutil.NewAPIError(http.StatusInternalServerError, "Could not find Machine with ID specified in request data for update", nil)
				}
				logger.Error().Err(err).Msg("error retrieving Machine from DB by ID")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Machine with ID specified in request data", nil)
			}

			instanceTypeID = machine.InstanceTypeID
		} // End validating Machine ID

		// Begin validating Instance Type ID
		if apiRequest.InstanceTypeID != nil {
			// Validate the Instance Type ID
			parsedID, err := uuid.Parse(*apiRequest.InstanceTypeID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing instance type id in request")
				return cutil.NewAPIError(http.StatusBadRequest, "Instance Type ID in request is not valid", nil)
			}
			instanceTypeID = &parsedID

			itDAO := cdbm.NewInstanceTypeDAO(cih.dbSession)

			instanceType, err := itDAO.GetByID(ctx, nil, *instanceTypeID, nil)
			if err != nil {
				if err == cdb.ErrDoesNotExist {
					return cutil.NewAPIError(http.StatusBadRequest, "Could not find Instance Type with ID specified in request data", nil)
				}
				logger.Error().Err(err).Msg("error retrieving Instance Type from DB by ID")
				return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve Instance Type with ID: %s specified in request data", *instanceTypeID), nil)
			}

			// Acquire the shared quota lock for this tenant/site/instance-type pool.
			// This lock is released when the transaction commits or rolls back.
			err = common.AcquireInstanceTypeQuotaLock(ctx, tx, tenant.ID, instanceType.ID)
			if err != nil {
				logger.Error().Err(err).Msg("Failed to acquire advisory lock on Instance Type quota pool")
				return cutil.NewAPIError(http.StatusInternalServerError, "Error creating Instance, detected multiple parallel request on Instance Type by Tenant", nil)
			}

			// Ensure that Tenant has an Allocation with specified Tenant InstanceType Site
			aDAO := cdbm.NewAllocationDAO(cih.dbSession)

			allocationFilter := cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{*instanceType.SiteID}}
			allocationPage := cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}

			tnas, _, serr := aDAO.GetAll(ctx, tx, allocationFilter, allocationPage, nil)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving Allocations from DB for Tenant and Site")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocation for Tenant", nil)
			}
			if len(tnas) == 0 {
				return cutil.NewAPIError(http.StatusForbidden,
					"Tenant does not have any Allocations for Site and Instance Type specified in request data", nil)
			}

			alconstraints, err := common.GetAllocationConstraintsForInstanceType(ctx, tx, cih.dbSession, tenant.ID, instanceType, tnas)
			if err != nil {
				if err == common.ErrAllocationConstraintNotFound {
					return cutil.NewAPIError(http.StatusInternalServerError, "No Allocations for specified Instance Type were found for current Tenant", nil)
				}
				logger.Error().Err(err).Msg("error retrieving Allocation Constraints from DB for InstanceType and Allocation")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Allocations for specified Instance Type, DB error", nil)
			}

			// Getting active instances for the tenant on requested instance type
			var siteIDs []uuid.UUID
			if instanceType.SiteID != nil {
				siteIDs = []uuid.UUID{*instanceType.SiteID}
			}
			_, insTotal, err := instanceDAO.GetAll(ctx, tx, cdbm.InstanceFilterInput{
				TenantIDs:       []uuid.UUID{tenant.ID},
				SiteIDs:         siteIDs,
				InstanceTypeIDs: []uuid.UUID{instanceType.ID},
			}, cdbp.PageInput{}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Active Instances from DB for Tenant and InstanceType")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve active instances for Tenant and Instance Type, DB error", nil)
			}

			// Calculate the total constraint value across all matching allocations.
			totalConstraintValue := 0
			for _, alcs := range alconstraints {
				totalConstraintValue += alcs.ConstraintValue
			}

			// If the current number of active instances has already
			// reached or exceeded the limit, then we can't add one
			// more.
			if insTotal >= totalConstraintValue {
				return cutil.NewAPIError(http.StatusForbidden,
					"Tenant has reached the maximum number of Instances for Instance Type specified in request data", nil)
			}

			// Select unallocated Machine for the requested instance type
			machine, err = common.GetUnallocatedMachineForInstanceType(ctx, logger, tx, cih.dbSession, instanceType, &apiRequest)
			if err != nil {
				var ibSelErr *common.InfiniBandMachineSelectionError
				if errors.As(err, &ibSelErr) {
					return cutil.NewAPIError(http.StatusBadRequest, ibSelErr.Error(), ibSelErr.ValidationError())
				}
				if err == common.ErrInstanceTypeMachineNotFound {
					return cutil.NewAPIError(http.StatusBadRequest,
						"No Machines are available for specified Instance Type", nil)

				}
				logger.Error().Err(err).Msg("error retrieving Machine from DB for Instance Type")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve available baremetal Machines for specified Instance Type", nil)
			}
		} // if apiRequest.InstanceTypeID != nil

		// NOTE: At this stage, we have a Machine ID whether it was provided in request or selected through Instance Type
		logger.Info().Str("MachineID", machine.ID).
			Interface("MachineLabelSelector", apiRequest.MachineLabelSelector).
			Msg("selected Machine for Instance creation")

		mcDAO := cdbm.NewMachineCapabilityDAO(cih.dbSession)

		// Fetch InfiniBand Capabilities from Instance Type or Machine and validate InfiniBand Interfaces
		var dbibic []cdbm.InfiniBandInterface

		if len(apiRequest.InfiniBandInterfaces) > 0 {
			var ibCapCount int
			var ibCaps []cdbm.MachineCapability

			if instanceTypeID != nil {
				ibCaps, ibCapCount, err = mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{*instanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeInfiniBand), nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil)
				if err != nil {
					logger.Error().Err(err).Msg("error retrieving Machine Capabilities from DB for Instance Type")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve InfiniBand Capabilities for Instance Type, DB error", nil)
				}
			}

			// If Instance Type does not have InfiniBand Capability, get capabilities from Machine
			if ibCapCount == 0 {
				ibCaps, ibCapCount, err = mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeInfiniBand), nil, nil, nil, nil, nil, nil, nil, nil, nil, nil, nil)
				if err != nil {
					logger.Error().Err(err).Msg("error retrieving Machine Capabilities from DB for Machine")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve InfiniBand Capabilities for Machine, DB error", nil)
				}
			}

			if ibCapCount == 0 {
				return cutil.NewAPIError(http.StatusBadRequest, "InfiniBand Interfaces cannot be specified if Instance Type or Machine doesn't have InfiniBand Capability", nil)
			}

			// Validate InfiniBand Interfaces against the selected Machine's InfiniBand Capabilities
			err = apiRequest.ValidateInfiniBandInterfaces(ibCaps)
			if err != nil {
				logger.Error().Err(err).Msg("Failed to validate InfiniBand interfaces in request data")
				return cutil.NewAPIError(http.StatusBadRequest, "Failed to validate InfiniBand Interfaces specified in request data", err)
			}

			// Validate InfiniBand Interface data
			var ibpIDs []uuid.UUID

			for _, ibic := range apiRequest.InfiniBandInterfaces {
				ibpID, err := uuid.Parse(ibic.InfiniBandPartitionID)
				if err != nil {
					logger.Warn().Err(err).Msg("error parsing infiniband partition id in instance infiniband interface request")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition ID: %v specified in request data is not valid", ibic.InfiniBandPartitionID), nil)
				}
				ibpIDs = append(ibpIDs, ibpID)
			}

			ibpDAO := cdbm.NewInfiniBandPartitionDAO(cih.dbSession)

			ibpIDMap := make(map[string]*cdbm.InfiniBandPartition)

			if len(ibpIDs) > 0 {
				ibps, _, err := ibpDAO.GetAll(ctx, nil, cdbm.InfiniBandPartitionFilterInput{InfiniBandPartitionIDs: ibpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				if err != nil {
					logger.Error().Err(err).Msg("error retrieving InfiniBand Partitions from DB by IDs")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve InfiniBand Partitions from DB by IDs", nil)
				}
				for i := range ibps {
					ibpIDMap[ibps[i].ID.String()] = &ibps[i]
				}
			}

			for _, ibic := range apiRequest.InfiniBandInterfaces {
				// Validate Instance infiniband interface information to create DB records later
				ibp, ok := ibpIDMap[ibic.InfiniBandPartitionID]
				if !ok {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not find InfiniBand Partition with ID: %v specified in request data", ibic.InfiniBandPartitionID), nil)
				}

				if ibp.SiteID != site.ID {
					logger.Warn().Msg(fmt.Sprintf("InfiniBandPartition: %v specified in request does not match with Instance Site", ibp.ID))
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition: %v specified in request does not match with Instance Site", ibp.ID), nil)
				}

				if ibp.TenantID != tenant.ID {
					logger.Warn().Msg(fmt.Sprintf("InfiniBandPartition: %v specified in request is not owned by Tenant", ibp.ID))
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition: %v specified in request is not owned by Tenant", ibp.ID), nil)
				}

				if ibp.ControllerIBPartitionID == nil || ibp.Status != cdbm.InfiniBandPartitionStatusReady {
					logger.Warn().Msg(fmt.Sprintf("InfiniBandPartition: %v specified in request data is not in Ready state", ibp.ID))
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition: %v specified in request data is not in Ready state", ibp.ID), nil)
				}

				dbibic = append(dbibic, cdbm.InfiniBandInterface{InfiniBandPartitionID: ibp.ID, Device: ibic.Device, Vendor: ibic.Vendor, DeviceInstance: ibic.DeviceInstance, IsPhysical: ibic.IsPhysical, VirtualFunctionID: ibic.VirtualFunctionID})
			}
		}

		// Validate DPU Interfaces if Instance Type has Network Capability with DPU device type
		if isInterfaceDeviceInfoPresent {
			var dpuNetworkCapCount int
			var dpuNetworkCaps []cdbm.MachineCapability

			if instanceTypeID != nil {
				dpuNetworkCaps, dpuNetworkCapCount, err = mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{*instanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeNetwork), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil, nil, nil, nil, nil)
				if err != nil {
					logger.Error().Err(err).Msg("error retrieving Machine Capabilities from DB for Instance Type")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve DPU aware Network Capabilities for Instance Type, DB error", nil)
				}
			}

			if dpuNetworkCapCount == 0 {
				dpuNetworkCaps, dpuNetworkCapCount, err = mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeNetwork), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil, nil, nil, nil, nil)
				if err != nil {
					logger.Error().Err(err).Msg("error retrieving Machine Capabilities from DB for Machine")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve DPU aware Network Capabilities for Machine, DB error", nil)
				}
			}

			if dpuNetworkCapCount == 0 {
				return cutil.NewAPIError(http.StatusBadRequest, "Device and Device Instance cannot be specified if Instance Type doesn't have Network Capability with DPU device type", nil)
			}

			// Validate DPU Interfaces if Instance Type DPU capability is present and matches with the request
			err = apiRequest.ValidateMultiEthernetDeviceInterfaces(dpuNetworkCaps, dbInterfaces)
			if err != nil {
				logger.Error().Err(err).Msg("Failed to validate DPU aware interfaces in request data")
				return cutil.NewAPIError(http.StatusBadRequest, "Failed to validate DPU aware interfaces specified in request data", err)
			}
		}

		// Validate NVLink Interfaces
		var nvllpIDs []uuid.UUID

		for _, nvlifc := range apiRequest.NVLinkInterfaces {
			nvllpID, err := uuid.Parse(nvlifc.NVLinkLogicalPartitionID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing NVLink Logical Partition id in instance NVLink Interface request")
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition ID: %v specified in request data is not valid", nvlifc.NVLinkLogicalPartitionID), nil)
			}
			nvllpIDs = append(nvllpIDs, nvllpID)
		}

		nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(cih.dbSession)

		nvllpIDMap := make(map[string]*cdbm.NVLinkLogicalPartition)

		if len(nvllpIDs) > 0 {
			nvllps, _, err := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{NVLinkLogicalPartitionIDs: nvllpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving NVLink Logical Partitions from DB by IDs")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions from DB by IDs", nil)
			}
			for i := range nvllps {
				nvllpIDMap[nvllps[i].ID.String()] = &nvllps[i]
			}
		}

		var nvlCaps []cdbm.MachineCapability
		var nvlCapCount int

		// Fetch GPU Capabilities from Instance Type or Machine
		if instanceTypeID != nil {
			nvlCaps, nvlCapCount, err = mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{*instanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeGPU), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving GPU Machine Capabilities from DB for Instance Type")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve GPU Capabilities for Instance Type, DB error", nil)
			}
		}

		if nvlCapCount == 0 {
			nvlCaps, nvlCapCount, err = mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeGPU), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving GPU Machine Capabilities from DB for Machine")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve GPU Capabilities for Machine, DB error", nil)
			}
		}

		var dbnvlic []cdbm.NVLinkInterface

		if len(apiRequest.NVLinkInterfaces) > 0 {
			if nvlCapCount == 0 {
				return cutil.NewAPIError(http.StatusBadRequest, "NVLink Interfaces cannot be specified if Instance Type doesn't have NVLink GPU Capability", nil)
			}

			// Validate NVLink interfaces if Instance Type has GPU Capability
			err = apiRequest.ValidateNVLinkInterfaces(nvlCaps)
			if err != nil {
				logger.Error().Err(err).Msg("Failed to validate NVLink interfaces specified in request data")
				return cutil.NewAPIError(http.StatusBadRequest, "Failed to validate NVLink interfaces specified in request data", err)
			}

			for _, nvlifc := range apiRequest.NVLinkInterfaces {
				nvllp, ok := nvllpIDMap[nvlifc.NVLinkLogicalPartitionID]
				if !ok {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not find NVLink Logical Partition with ID: %v specified in request data", nvlifc.NVLinkLogicalPartitionID), nil)
				}

				// Validate that the NVLink Logical Partition ID matches the default NVLink Logical Partition ID
				if defaultNvllpID != nil {
					if nvllp.ID != *defaultNvllpID {
						return cutil.NewAPIError(http.StatusBadRequest, "NVLink Logical Partition specified for NVLink Interface does not match NVLink Logical Partition of VPC", nil)
					}
				} else {
					// Validate NVLink Logical Partition only if it's not the default
					if nvllp.SiteID != site.ID {
						logger.Warn().Msg(fmt.Sprintf("NVLink Logical Partition: %v specified in request does not match with Instance Site", nvllp.ID))
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition: %v specified in request does not match with Instance Site", nvllp.ID), nil)
					}

					if nvllp.TenantID != tenant.ID {
						logger.Warn().Msg(fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not owned by Tenant", nvllp.ID))
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not owned by Tenant", nvllp.ID), nil)
					}

					if nvllp.Status != cdbm.NVLinkLogicalPartitionStatusReady {
						logger.Warn().Msg(fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not in Ready state", nvllp.ID))
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not in Ready state", nvllp.ID), nil)
					}
				}

				// Validate Instance NVLink Interface information to create DB records later
				dbnvlic = append(dbnvlic, cdbm.NVLinkInterface{NVLinkLogicalPartitionID: nvllp.ID, DeviceInstance: nvlifc.DeviceInstance})
			}
		} else if defaultNvllpID != nil {
			// Generate Interfaces for the default NVLink Logical Partition
			// For a given Machine, all the GPUs should be connected to the same NVLink Logical Partition
			for _, nvlCap := range nvlCaps {
				if nvlCap.Count != nil {
					for deviceInstance := range *nvlCap.Count {
						dbnvlic = append(dbnvlic, cdbm.NVLinkInterface{NVLinkLogicalPartitionID: *defaultNvllpID, Device: cutil.GetPtr(nvlCap.Name), DeviceInstance: deviceInstance})
					}
				}
			}
		}

		if cih.cfg.GetDPSEnabled() && vpc.PowerResourceGroup != nil {
			assignment := powerutil.MachinePowerAssignment{MachineID: machine.ID}
			if apiRequest.PowerProfile != nil {
				assignment.PowerProfile = *apiRequest.PowerProfile
			}
			dpsRollback, err = powerutil.ProvisionMachinePower(ctx, cih.dps, *vpc.PowerResourceGroup, assignment)
			if err != nil {
				logger.Error().Err(err).Str("machineID", machine.ID).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("DPS rejected Instance allocation")
				return cutil.NewAPIError(http.StatusServiceUnavailable, "DPS rejected Instance power allocation", nil)
			}
		}

		// ==================== Step 5: Create Instance Records  ====================

		instanceCreateInput := cdbm.InstanceCreateInput{
			Name:                     apiRequest.Name,
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
			IsUpdatePending:          false,
			Status:                   cdbm.InstanceStatusPending,
			PowerStatus:              cutil.GetPtr(cdbm.InstancePowerStatusRebooting),
			PowerProfile:             apiRequest.PowerProfile,
			CreatedBy:                dbUser.ID,
		}

		// NOTE: Set InstanceTypeID only if it is provided in the request.
		// Machine ID based Instance creation does not require an Instance Type.
		if apiRequest.InstanceTypeID != nil {
			instanceCreateInput.InstanceTypeID = instanceTypeID
		}

		var derr error
		instance, derr = instanceDAO.Create(ctx, tx, instanceCreateInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create Instance record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating Instance record, DB error", nil)
		}

		// Update the controller ID
		// We need this to match the instance ID.  This was previously handled
		// by the async cloud workflow after successful creation on site.
		instance, derr = instanceDAO.Update(ctx, tx, cdbm.InstanceUpdateInput{InstanceID: instance.ID, InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{ControllerInstanceID: cutil.GetPtr(instance.ID)}})
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to update Instance record controllerInstanceID in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed updating new Instance record, DB error", nil)
		}

		// We'll need a list of the IDs as a string slice to
		// send along in the config update request to nico.
		instanceSshKeyGroupIds := []string{}

		// create the ssh key group instance association in the db
		skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(cih.dbSession)
		for _, skg := range skgs {
			_, err := skgiaDAO.Create(ctx, tx, cdbm.SSHKeyGroupInstanceAssociationCreateInput{
				SSHKeyGroupID: skg.ID,
				SiteID:        site.ID,
				InstanceID:    instance.ID,
				CreatedBy:     dbUser.ID,
			})
			if err != nil {
				logger.Error().Err(err).Msg("failed to create the SSH Key Group Instance Association record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate one or more SSH Key Group with Instance, DB error", nil)
			}

			instanceSshKeyGroupIds = append(instanceSshKeyGroupIds, skg.ID.String())
		}

		// Prepare interface details to pass to nico call
		interfaceConfigs := []*corev1.InstanceInterfaceConfig{}

		// Create the instance subnet record in the db from info gathered earlier
		// The first Subnet is automatically added to the physical interface
		ifcs = []cdbm.Interface{}
		ifcDAO := cdbm.NewInterfaceDAO(cih.dbSession)
		for _, dbifc := range dbInterfaces {
			input := cdbm.InterfaceCreateInput{
				InstanceID:           instance.ID,
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
				Status:               dbifc.Status,
				CreatedBy:            dbUser.ID,
			}

			retifc, serr := ifcDAO.Create(ctx, tx, input)
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Instance Subnet DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Instance Subnet entry for Instance, DB error", nil)
			}

			ifc := *retifc
			ifc.Vpc = dbifc.Vpc
			ifc.VpcPrefix = dbifc.VpcPrefix // We created the interface in the DB based on the values in dbifc, so we can populate this as well.
			ifcs = append(ifcs, ifc)

			interfaceConfig := &corev1.InstanceInterfaceConfig{
				FunctionType: corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
			}

			// Assign InstanceInterfaceConfig_SegmentId in case of Subnet
			if dbifc.SubnetID != nil {
				interfaceConfig.NetworkSegmentId = &corev1.NetworkSegmentId{
					Value: subnetIDMap[*dbifc.SubnetID].ControllerNetworkSegmentID.String(),
				}
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_SegmentId{
					SegmentId: &corev1.NetworkSegmentId{
						Value: subnetIDMap[*dbifc.SubnetID].ControllerNetworkSegmentID.String(),
					},
				}
			}

			// Preserve unresolved VPC intent; otherwise use the explicit prefix.
			vpcSelection, serr := instanceInterfaceVpcSelection(&dbifc)
			if serr != nil {
				logger.Error().Err(serr).Msg("failed to build VPC selection for Instance Interface")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to build VPC selection for Instance Interface", nil)
			}
			if vpcSelection != nil {
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_Vpc{Vpc: vpcSelection}
			} else if dbifc.VpcPrefixID != nil {
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_VpcPrefixId{
					VpcPrefixId: &corev1.VpcPrefixId{Value: dbifc.VpcPrefixID.String()},
				}
			}

			if dbifc.IsPhysical {
				interfaceConfig.FunctionType = corev1.InterfaceFunctionType_PHYSICAL_FUNCTION
			}

			// Assign Device and DeviceInstance in case of Multi DPU Interface
			if dbifc.Device != nil && dbifc.DeviceInstance != nil {
				interfaceConfig.Device = dbifc.Device
				interfaceConfig.DeviceInstance = uint32(*dbifc.DeviceInstance)
			}

			if !dbifc.IsPhysical {
				if dbifc.VirtualFunctionID != nil {
					vfID := uint32(*dbifc.VirtualFunctionID)
					interfaceConfig.VirtualFunctionId = &vfID
				}
			}

			if dbifc.RequestedIpAddress != nil {
				interfaceConfig.IpAddress = dbifc.RequestedIpAddress
			}
			if dbifc.InlineRoutingProfile != nil {
				interfaceConfig.RoutingProfile = dbifc.InlineRoutingProfile.ToProto()
			}

			interfaceConfigs = append(interfaceConfigs, interfaceConfig)
		}

		//We'll need this later for the nico call
		ibInterfaceConfigs := []*corev1.InstanceIBInterfaceConfig{}

		// Create the instance infiniband interface record in the db from info gathered earlier IF instance type was used
		ibifcs = []cdbm.InfiniBandInterface{}
		ibifcDAO := cdbm.NewInfiniBandInterfaceDAO(cih.dbSession)

		for _, ibifc := range dbibic {
			retibifc, serr := ibifcDAO.Create(
				ctx,
				tx,
				cdbm.InfiniBandInterfaceCreateInput{
					InstanceID:            instance.ID,
					SiteID:                site.ID,
					InfiniBandPartitionID: ibifc.InfiniBandPartitionID,
					Device:                ibifc.Device,
					Vendor:                ibifc.Vendor,
					DeviceInstance:        ibifc.DeviceInstance,
					IsPhysical:            ibifc.IsPhysical,
					VirtualFunctionID:     ibifc.VirtualFunctionID,
					Status:                cdbm.InfiniBandInterfaceStatusPending,
					CreatedBy:             dbUser.ID,
				},
			)
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Instance InfiniBand Interface DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Instance InfiniBand Interface entry for Instance, DB error", nil)
			}

			ifc := *retibifc
			ibifcs = append(ibifcs, ifc)

			ibInterfaceConfig := &corev1.InstanceIBInterfaceConfig{
				Device:         ifc.Device,
				Vendor:         ifc.Vendor,
				DeviceInstance: uint32(ifc.DeviceInstance),
				FunctionType:   corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
				IbPartitionId:  &corev1.IBPartitionId{Value: ifc.InfiniBandPartitionID.String()},
			}
			ibInterfaceConfigs = append(ibInterfaceConfigs, ibInterfaceConfig)

			if !ifc.IsPhysical {
				ibInterfaceConfig.FunctionType = corev1.InterfaceFunctionType_VIRTUAL_FUNCTION

				if ifc.VirtualFunctionID != nil {
					vfID := uint32(*ifc.VirtualFunctionID)
					ibInterfaceConfig.VirtualFunctionId = &vfID
				}
			}
		}

		// Create the instance NVLink Interface record in the db from info gathered earlier IF instance type was used
		nvlifcs = []cdbm.NVLinkInterface{}
		nvlifcDAO := cdbm.NewNVLinkInterfaceDAO(cih.dbSession)
		nvlInterfaceConfigs := []*corev1.InstanceNVLinkGpuConfig{}
		for _, nvlifc := range dbnvlic {
			retnvlifc, serr := nvlifcDAO.Create(
				ctx,
				tx,
				cdbm.NVLinkInterfaceCreateInput{
					InstanceID:               instance.ID,
					SiteID:                   site.ID,
					NVLinkLogicalPartitionID: nvlifc.NVLinkLogicalPartitionID,
					Device:                   nvlifc.Device,
					DeviceInstance:           nvlifc.DeviceInstance,
					Status:                   cdbm.NVLinkInterfaceStatusPending,
					CreatedBy:                dbUser.ID,
				},
			)
			if serr != nil {
				logger.Error().Err(serr).Msg("error creating Instance NVLink Interface DB entry")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Instance NVLink Interface entry for Instance, DB error", nil)
			}

			nvlfc := *retnvlifc
			nvlifcs = append(nvlifcs, nvlfc)

			nvlInterfaceConfig := &corev1.InstanceNVLinkGpuConfig{
				DeviceInstance:     uint32(nvlifc.DeviceInstance),
				LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: nvlfc.NVLinkLogicalPartitionID.String()},
			}
			nvlInterfaceConfigs = append(nvlInterfaceConfigs, nvlInterfaceConfig)
		}

		// Create the DpuExtensionServiceDeployment records in DB
		desdConfigs := []*corev1.InstanceDpuExtensionServiceConfig{}

		desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(cih.dbSession)
		desds = []cdbm.DpuExtensionServiceDeployment{}

		for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
			desdID, err := uuid.Parse(adesdr.DpuExtensionServiceID)
			if err != nil {
				return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Invalid DPU Extension Service ID: %s specified in request", adesdr.DpuExtensionServiceID), nil)
			}

			desd, err := desdDAO.Create(ctx, tx, cdbm.DpuExtensionServiceDeploymentCreateInput{
				SiteID:                site.ID,
				TenantID:              tenant.ID,
				InstanceID:            instance.ID,
				DpuExtensionServiceID: desdID,
				Version:               adesdr.Version,
				Status:                cdbm.DpuExtensionServiceDeploymentStatusPending,
				CreatedBy:             dbUser.ID,
			})
			if err != nil {
				logger.Error().Err(err).Msg("error creating Instance DpuExtensionServiceDeployment record in DB")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create DPU Extension Service Deployment for Instance, DB error", nil)
			}

			des, _ := desIDMap[desdID]
			desd.DpuExtensionService = des

			desds = append(desds, *desd)

			desdConfigs = append(desdConfigs, &corev1.InstanceDpuExtensionServiceConfig{
				ServiceId: desd.DpuExtensionServiceID.String(),
				Version:   desd.Version,
			})
		}

		// Create the status detail record
		sdDAO := cdbm.NewStatusDetailDAO(cih.dbSession)
		var serr error
		ssd, serr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: instance.ID.String(), Status: *cutil.GetPtr(cdbm.InstanceStatusPending), Message: cutil.GetPtr("received instance creation request, pending")})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Instance, DB error", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Instance", nil)
		}

		// ==================== Step 6: Workflow Trigger  ====================

		// Get the temporal client for the site we are working with.
		stc, err := cih.scp.GetClientByID(instance.SiteID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		createLabels := util.ProtobufLabelsFromAPILabels(instance.Labels)

		description := ""
		if instance.Description != nil {
			description = *instance.Description
		}

		spectrumXAttachmentConfigs := make([]*corev1.InstanceSpxAttachment, 0, len(apiRequest.SpectrumXAttachments))
		for _, sac := range apiRequest.SpectrumXAttachments {
			spectrumXAttachmentConfigs = append(spectrumXAttachmentConfigs, sac.ToProto())
		}

		// Prepare the create request workflow object
		createInstanceRequest := &corev1.InstanceAllocationRequest{
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
				Network: buildInstanceNetworkConfig(instance.AutoNetwork, interfaceConfigs, vpc.ControllerVpcID),
				Infiniband: &corev1.InstanceInfinibandConfig{
					IbInterfaces: ibInterfaceConfigs,
				},
				DpuExtensionServices: &corev1.InstanceDpuExtensionServicesConfig{
					ServiceConfigs: desdConfigs,
				},
				Nvlink: &corev1.InstanceNVLinkConfig{
					GpuConfigs: nvlInterfaceConfigs,
				},
				Spxconfig: &corev1.InstanceSpxConfig{SpxAttachments: spectrumXAttachmentConfigs},
			},
			AllowUnhealthyMachine: allowUnhealthyMachine,
		}

		if apiRequest.InstanceTypeID != nil {
			createInstanceRequest.InstanceTypeId = cutil.GetPtr(*apiRequest.InstanceTypeID)
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-create-" + instance.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering instance create workflow")

		// Add context deadlines
		ctx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow to update instance
		// TODO: Once Site Agent offers CreateInstanceV2 re-registered as CreateInstance then update workflow name here
		we, err := stc.ExecuteWorkflow(ctx, workflowOptions, "CreateInstanceV2", createInstanceRequest)
		if err != nil {
			logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to create Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to create Instance on Site: %s", err), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create Instance workflow")

		// Execute the workflow synchronously
		err = we.Get(ctx, nil)
		if err != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(err, &timeoutErr) || err == context.DeadlineExceeded || ctx.Err() != nil {
				logger.Error().Err(err).Msg("failed to create Instance, timeout occurred executing workflow on Site.")
				timeoutCause := err
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "Instance", "CreateInstanceV2")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Instance create workflow timed out", nil)
			}

			code, err := common.UnwrapWorkflowError(err)
			logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to create Instance")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create Instance on Site: %s", err), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create Instance workflow")

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			if dpsRollback != nil {
				rollbackErr := dpsRollback()
				if rollbackErr != nil {
					logger.Error().Err(rollbackErr).Str("machineID", machine.ID).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("failed to compensate DPS after Instance creation failure")
				}
			}
			return common.HandleTxError(c, logger, err, "Failed to create Instance, DB transaction error")
		}
	}
	if timeoutResp != nil {
		responseErr := timeoutResp()
		if dpsRollback != nil {
			rollbackErr := dpsRollback()
			if rollbackErr != nil {
				logger.Error().Err(rollbackErr).Str("machineID", machine.ID).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("failed to compensate DPS after Instance creation failure")
			}
		}
		return responseErr
	}

	// ==================== Step 7: Response ====================

	// Create response
	apiInstance := model.NewAPIInstance(instance, site, ifcs, ibifcs, desds, nvlifcs, skgs, []cdbm.StatusDetail{*ssd})

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiInstance)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateInstanceHandler is the API Handler for updating an Instance
type UpdateInstanceHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// NewUpdateInstanceHandler initializes and returns a new handler for updating Instance
func NewUpdateInstanceHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) UpdateInstanceHandler {
	return UpdateInstanceHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		dps:        dps,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

func (uih UpdateInstanceHandler) handleReboot(c echo.Context, logger *zerolog.Logger, apiRequest *model.APIInstanceUpdateRequest, instance *cdbm.Instance) error {
	ctx := c.Request().Context()

	// Get the temporal client for the site we are working with.
	stc, err := uih.scp.GetClientByID(instance.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Values populated inside the transaction closure that are needed for the response.
	var ui *cdbm.Instance
	var retifc []cdbm.Interface
	var dbskgs []cdbm.SSHKeyGroup
	var ssds []cdbm.StatusDetail
	reqCtx := ctx

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, uih.dbSession, func(tx *cdb.Tx) error {
		// Prepare DAOs
		sdDAO := cdbm.NewStatusDetailDAO(uih.dbSession)

		// Check for reboot request
		powerStatus := cutil.GetPtr(cdbm.InstancePowerStatusRebooting)
		powerStatusMessage := cutil.GetPtr("received Instance reboot request, processing")

		// Check if instance request for rebooting with ipxe
		rebootWithCustomIpxe := false
		if apiRequest.RebootWithCustomIpxe != nil && *apiRequest.RebootWithCustomIpxe {
			rebootWithCustomIpxe = true
		}

		// Check if instance request for updating before reboot
		applyUpdatesOnReboot := false
		if apiRequest.ApplyUpdatesOnReboot != nil && *apiRequest.ApplyUpdatesOnReboot {
			applyUpdatesOnReboot = true
			powerStatusMessage = cutil.GetPtr("received Instance reboot request with apply updates, processing")
		}

		// Update Instance
		instanceDAO := cdbm.NewInstanceDAO(uih.dbSession)
		var derr error
		ui, derr = instanceDAO.Update(ctx, tx,
			cdbm.InstanceUpdateInput{
				InstanceID: instance.ID,
				InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
					Name:        apiRequest.Name,
					Description: apiRequest.Description,
					PowerStatus: powerStatus,
				},
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Instance", nil)
		}

		_, serr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: instance.ID.String(), Status: *cutil.GetPtr(cdbm.InstancePowerStatusRebooting), Message: powerStatusMessage})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Instance reboot", nil)
		}

		// Get the instance subnets record from the db
		ifcDAO := cdbm.NewInterfaceDAO(uih.dbSession)
		retifc, _, derr = ifcDAO.GetAll(ctx, tx, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{}, []string{cdbm.SubnetRelationName, cdbm.VpcPrefixRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Instance Subnets Details from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Instance Subnets for Instance", nil)
		}

		// Get the ssh key group instance associations record from the db
		skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(uih.dbSession)
		skgias, _, derr := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
			SiteIDs:     []uuid.UUID{instance.Site.ID},
			InstanceIDs: []uuid.UUID{instance.ID},
		}, cdbp.PageInput{}, []string{cdbm.SSHKeyGroupRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving ssh key group instance association Details from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Instance Association for Instance", nil)
		}

		for _, skgia := range skgias {
			dbskgs = append(dbskgs, *skgia.SSHKeyGroup)
		}

		// Get status details
		ssds, _, derr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{ui.ID.String()}}, cdbp.PageInput{})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for Instance from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for Instance", nil)
		}

		// Prepare the config update request workflow object
		rebootInstanceRequest := &corev1.InstancePowerRequest{
			InstanceId:           &corev1.InstanceId{Value: instance.GetSiteID().String()},
			Operation:            corev1.InstancePowerRequest_POWER_RESET,
			BootWithCustomIpxe:   rebootWithCustomIpxe,
			ApplyUpdatesOnReboot: applyUpdatesOnReboot,
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-reboot-" + instance.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering instance reboot workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow to update instance
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "RebootInstanceV2", rebootInstanceRequest)

		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to reboot Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to reboot Instance on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous reboot Instance workflow")

		// Execute the workflow synchronously
		wferr = we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to reboot Instance, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, *logger, stc, wid, timeoutCause, "Instance", "RebootInstanceV2")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Instance reboot workflow timed out", nil)
			}
			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to execute Temporal workflow to reboot Instance")

			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to reboot Instance on Site: %s", unwrapped), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous reboot Instance workflow")

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, *logger, err, "Failed to reboot Instance, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	apiInstance := model.NewAPIInstance(ui, instance.Site, retifc, nil, nil, nil, dbskgs, ssds)
	if ui.NetworkSecurityGroupID == nil {
		err = AttachVpcNsgPropagationDetailsToApiInstance(c, reqCtx, logger, uih.dbSession, ui, retifc, apiInstance)
		if err != nil {
			logger.Error().Err(err).Msg("failed to attach VPC NSG propagation details to rebooted Instance response")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC NSG propagation details for Instance", nil)
		}
	}

	logger.Info().Msg("finishing rebootHandler")

	return c.JSON(http.StatusOK, apiInstance)
}

// Returns either an existing instance OS config or an updated OS config based on the incoming request.
// apiRequest will be mutated for use in UpdateFromParams.
// osConfig will hold the struct/data for use with Temporal/NICo calls.
// Errors should be returned in the form of cutil.NewAPIErrorResponse
func (uih UpdateInstanceHandler) buildInstanceUpdateRequestOsConfig(c echo.Context, logger *zerolog.Logger, apiRequest *model.APIInstanceUpdateRequest, instance *cdbm.Instance, site *cdbm.Site) (*corev1.InstanceOperatingSystemConfig, *uuid.UUID, *cutil.APIError) {

	var os *cdbm.OperatingSystem
	var osID *uuid.UUID

	ctx := c.Request().Context()

	// The OS is being cleared.
	if apiRequest.OperatingSystemID != nil && *apiRequest.OperatingSystemID == "" {

		if err := apiRequest.ValidateAndSetOperatingSystemData(uih.cfg, instance, nil); err != nil {
			logger.Error().Err(err).Msg("failed to validate OperatingSystem")
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to validate OperatingSystem data", err)
		}

		return &corev1.InstanceOperatingSystemConfig{
			RunProvisioningInstructionsOnEveryBoot: instance.AlwaysBootWithCustomIpxe,
			PhoneHomeEnabled:                       *apiRequest.PhoneHomeEnabled, // Set by the earlier call to ValidateAndSetOperatingSystemData
			Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
				Ipxe: &corev1.InlineIpxe{
					IpxeScript: *apiRequest.IpxeScript,
				},
			},
			UserData: apiRequest.UserData,
		}, nil, nil
	}

	// If the base OS is either not changing OR the base is changing to another OS and NOT simply being cleared,
	// then we'll need to pull OS data.

	// Default to the OS of the instance.
	osID = instance.OperatingSystemID

	// Use the OS sent by the caller if one was sent in.
	if apiRequest.OperatingSystemID != nil {
		var id uuid.UUID
		var err error

		if id, err = uuid.Parse(*apiRequest.OperatingSystemID); err != nil {
			logger.Error().Err(err).Msg("failed to parse OperatingSystemID")
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Unable to parse `operatingSystemId` specified", validation.Errors{
				"operatingSystemId": errors.New(*apiRequest.OperatingSystemID),
			})
		}

		osID = &id
	}

	if osID != nil {
		var serr error

		// Retrieve the details for the OS
		osDAO := cdbm.NewOperatingSystemDAO(uih.dbSession)
		os, serr = osDAO.GetByID(ctx, nil, *osID, nil)
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

		// Confirm the Tenant can use the OS. Provider-owned Templated iPXE OSes
		// are shared through synchronized Site associations validated below.
		if !os.IsTenantUsable(instance.Tenant.ID.String()) {
			logger.Error().Msg("OperatingSystem in request is not usable by tenant")
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Operating system specified in request is not owned by Tenant", nil)
		}

		// Validate the Site has the ImageBasedOperatingSystem capability enabled for Image based Operating Systems
		if os.Type == cdbm.OperatingSystemTypeImage {
			if site.Config == nil || !site.Config.ImageBasedOperatingSystem {
				logger.Warn().Str("operatingSystemId", os.ID.String()).Str("siteId", site.ID.String()).Msg("Instance update with Image based Operating System is not supported for Site, ImageBasedOperatingSystem capability is not enabled")
				return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Update of Instance with Image based Operating System is not supported. Site must have ImageBasedOperatingSystem capability enabled.", nil)
			}
		}

		// Confirm match between site and OS (only for Image type).
		if os.Type == cdbm.OperatingSystemTypeImage {
			ossaDAO := cdbm.NewOperatingSystemSiteAssociationDAO(uih.dbSession)
			_, ossaCount, err := ossaDAO.GetAll(
				ctx,
				nil,
				cdbm.OperatingSystemSiteAssociationFilterInput{
					OperatingSystemIDs: []uuid.UUID{*osID},
					SiteIDs:            []uuid.UUID{site.ID},
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
		}
	}

	// reject deactivated OS except if OS stays the same:
	if os != nil && !os.IsActive {
		if apiRequest.OperatingSystemID != nil && instance.OperatingSystemID != nil && *apiRequest.OperatingSystemID != instance.OperatingSystemID.String() {
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Operating System specified in request has been deactivated and cannot be used to update an instance", nil)
		}
	}

	// Validate any additional properties.
	// `os` could still be nil here if no OS ID was sent
	// in the request _and_ the instance didn't have an OS ID
	// to begin with or had previously been cleared (nil'ed)
	// by an earlier request.

	err := apiRequest.ValidateAndSetOperatingSystemData(uih.cfg, instance, os)
	if err != nil {
		logger.Error().Msgf("OperatingSystem options validation failed: %s", err)
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "OperatingSystem options validation failed", err)
	}

	// Here, we'll default to whatever the instance already had set,
	// but will give precedence to any property being set by the request.
	// Some or all of these might have been set in ValidateAndSetOperatingSystemData
	// to the desired/expected override value(s).

	alwaysBootWithCustomIpxe := instance.AlwaysBootWithCustomIpxe
	if apiRequest.AlwaysBootWithCustomIpxe != nil {
		alwaysBootWithCustomIpxe = *apiRequest.AlwaysBootWithCustomIpxe
	}

	ipxeScript := instance.IpxeScript
	if apiRequest.IpxeScript != nil {
		ipxeScript = apiRequest.IpxeScript
	}

	userData := instance.UserData
	if apiRequest.UserData != nil {
		userData = apiRequest.UserData
	}

	phoneHomeEnabled := instance.PhoneHomeEnabled
	if apiRequest.PhoneHomeEnabled != nil {
		phoneHomeEnabled = *apiRequest.PhoneHomeEnabled
	}

	if os != nil {
		if os.Type == cdbm.OperatingSystemTypeIPXE {
			return &corev1.InstanceOperatingSystemConfig{
				RunProvisioningInstructionsOnEveryBoot: alwaysBootWithCustomIpxe,
				PhoneHomeEnabled:                       phoneHomeEnabled,
				Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
					Ipxe: &corev1.InlineIpxe{
						IpxeScript: *ipxeScript,
					},
				},
				UserData: userData,
			}, osID, nil
		} else if os.Type == cdbm.OperatingSystemTypeTemplatedIPXE {
			if apiErr := validateTemplatedIpxeOsForSite(ctx, uih.dbSession, logger, os, site.ID); apiErr != nil {
				return nil, nil, apiErr
			}
			return &corev1.InstanceOperatingSystemConfig{
				RunProvisioningInstructionsOnEveryBoot: alwaysBootWithCustomIpxe,
				PhoneHomeEnabled:                       phoneHomeEnabled,
				Variant: &corev1.InstanceOperatingSystemConfig_OperatingSystemId{
					OperatingSystemId: &corev1.OperatingSystemId{
						Value: os.ID.String(),
					},
				},
				UserData: userData,
			}, osID, nil
		} else if os.Type == cdbm.OperatingSystemTypeImage {
			return &corev1.InstanceOperatingSystemConfig{
				PhoneHomeEnabled: phoneHomeEnabled,
				Variant: &corev1.InstanceOperatingSystemConfig_OsImageId{
					OsImageId: &corev1.UUID{
						Value: os.ID.String(),
					},
				},
				UserData: userData,
			}, osID, nil
		}
	}

	return &corev1.InstanceOperatingSystemConfig{
		RunProvisioningInstructionsOnEveryBoot: alwaysBootWithCustomIpxe,
		PhoneHomeEnabled:                       phoneHomeEnabled,
		Variant: &corev1.InstanceOperatingSystemConfig_Ipxe{
			Ipxe: &corev1.InlineIpxe{
				IpxeScript: *ipxeScript,
			},
		},
		UserData: userData,
	}, osID, nil
}

// Handle godoc
// @Summary Update an existing Instance
// @Description Update an existing Instance for the org
// @Tags Instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Instance"
// @Param message body model.APIInstanceUpdateRequest true "Instance update request"
// @Success 200 {object} model.APIInstance
// @Router /v2/org/{org}/nico/instance/{id} [patch]
func (uih UpdateInstanceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Instance", "Update", c, uih.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
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

	// Validate role, only Tenant Admins are allowed to update Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get instance ID from URL param
	instanceStrID := c.Param("id")
	instanceID, err := uuid.Parse(instanceStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance ID in URL", nil)
	}

	uih.tracerSpan.SetAttribute(handlerSpan, attribute.String("instance_id", instanceStrID), logger)

	// Add the instance ID to the log fields now that we know we have a valid one.
	logger = logger.With().Str("Instance ID", instanceID.String()).Logger()

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIInstanceUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Instance update request data", verr)
	}
	instanceDAO := cdbm.NewInstanceDAO(uih.dbSession)

	// Check that Instance exists
	instance, err := instanceDAO.GetByID(ctx, nil, instanceID, []string{cdbm.SiteRelationName, cdbm.TenantRelationName, cdbm.VpcRelationName, cdbm.MachineRelationName})
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Instance DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Instance to update", nil)
	}

	if instance.Site == nil {
		logger.Error().Msg("error retrieving Site as included relation for Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Instance", nil)
	}

	if instance.Tenant == nil {
		logger.Error().Msg("error retrieving Tenant as included relation for Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant details for Instance", nil)
	}

	if instance.Vpc == nil {
		logger.Error().Msg("error retrieving VPC as included relation for Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC details for Instance", nil)
	}

	if instance.Machine == nil {
		logger.Error().Msg("error retrieving Machine as included relation for Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Machine details for Instance", nil)
	}

	tenant := instance.Tenant
	site := instance.Site
	vpc := instance.Vpc
	machine := instance.Machine
	if uih.cfg.GetDPSEnabled() && apiRequest.PowerProfile != nil && vpc.PowerResourceGroup == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "A power profile update requires the VPC to have a power resource group", nil)
	}

	// Confirm that the Instance's org matches the org sent in the request
	if tenant.Org != org {
		logger.Error().Err(err).Msg("org specified in request does not match org of Tenant associated with Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org specified in request does not match Org of Tenant associated with Instance", nil)
	}

	// Add the tenant to the log fields
	logger = logger.With().Str("Tenant ID", tenant.ID.String()).Logger()

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Error().Str("Site ID", site.ID.String()).Str("Site Status", site.Status).Msg("Unable to update Instance, Site is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state - cannot update Instance", nil)
	}
	if apiErr := util.ValidateSitePowerManagement(site.Config, apiRequest.PowerProfile); apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	apiErr := model.ValidatePowerProfile(ctx, uih.cfg.GetDPSEnabled(), uih.dps, apiRequest.PowerProfile)
	if apiErr != nil {
		logger.Warn().Err(apiErr.Diagnosis()).Msg("failed to validate Instance power profile")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	// If the instance is in some stage of deprovisioning, there's nothing to update.
	// We could move this up even higher, but we might not want to reveal status at all until
	// we know the caller has access to this instance.
	if instance.Status == cdbm.InstanceStatusTerminating || instance.Status == cdbm.InstanceStatusTerminated {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Instance is terminating and cannot be updated", nil)
	}

	// Validate network fields that depend on the resolved VPC and the
	// Instance's currently-persisted auto state (e.g. `autoNetwork: true`
	// requires a Flat VPC; explicit interfaces can't be set while the
	// effective post-update auto state is true).
	verr = apiRequest.ValidateForVpc(vpc, instance.AutoNetwork)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance update request against VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Instance update request data", verr)
	}

	if instance.IsMissingOnSite {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Instance is missing on site and cannot be updated", nil)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another instance with same name at the site
	if apiRequest.Name != nil && *apiRequest.Name != instance.Name {
		ins, tot, serr := instanceDAO.GetAll(ctx, nil,
			cdbm.InstanceFilterInput{
				Names:     []string{*apiRequest.Name},
				TenantIDs: []uuid.UUID{tenant.ID},
				SiteIDs:   []uuid.UUID{site.ID},
			},
			cdbp.PageInput{},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant instance")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update Instance due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another Instance with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(ins[0].ID.String()),
			})
		}
	}

	// Check for reboot request
	instanceRebootRequest := false
	if apiRequest.TriggerReboot != nil && *apiRequest.TriggerReboot {
		instanceRebootRequest = true
	}

	// If this was only a reboot request, handle it and return.
	if instanceRebootRequest {
		return uih.handleReboot(c, &logger, &apiRequest, instance)
	}

	// Otherwise, this is a real Instance config update.
	var instanceStatusConfiguring *string
	if apiRequest.IsInterfaceUpdateRequest() {
		instanceStatusConfiguring = cutil.GetPtr(cdbm.InstanceStatusConfiguring)
	}

	// If an NSG was requested, validate it.
	// A blank NSG ID means the user is updating to clear the field.
	var nsgID *string
	if apiRequest.NetworkSecurityGroupID != nil && *apiRequest.NetworkSecurityGroupID != "" {
		nsgDAO := cdbm.NewNetworkSecurityGroupDAO(uih.dbSession)

		nsg, err := nsgDAO.GetByID(ctx, nil, *apiRequest.NetworkSecurityGroupID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				logger.Error().Err(err).Msg("could not find NetworkSecurityGroup with ID specified in request data")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find NetworkSecurityGroup with ID specified in request data", nil)
			}

			logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroup with ID specified in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NetworkSecurityGroup with ID specified in request data", nil)
		}

		if nsg.SiteID != site.ID {
			logger.Error().Msg("NetworkSecurityGroup in request does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NetworkSecurityGroup with ID specified in request data does not belong to Site", nil)
		}

		if nsg.TenantID != tenant.ID {
			logger.Error().Msg("NetworkSecurityGroup in request does not belong to Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NetworkSecurityGroup with ID specified in request data does not belong to Tenant", nil)
		}

		nsgID = cutil.GetPtr(nsg.ID)
	}

	// Validate Interfaces if present
	sbDAO := cdbm.NewSubnetDAO(uih.dbSession)
	vpDAO := cdbm.NewVpcPrefixDAO(uih.dbSession)

	// Collect all Subnet and VPC Prefix IDs for batch query
	subnetIDs := []uuid.UUID{}
	vpcPrefixIDs := []uuid.UUID{}
	subnetIfcMap := map[uuid.UUID]uint64{}
	vpcPrefixIfcMap := map[uuid.UUID]uint64{}

	for _, ifc := range apiRequest.Interfaces {
		if ifc.SubnetID != nil {
			subnetID, err := uuid.Parse(*ifc.SubnetID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing subnet id in instance subnet request")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet ID specified in request data is not valid", nil)
			}
			subnetIDs = append(subnetIDs, subnetID)
			subnetIfcMap[subnetID]++
		}
		if ifc.VpcPrefixID != nil {
			vpcPrefixID, err := uuid.Parse(*ifc.VpcPrefixID)
			if err != nil {
				logger.Warn().Err(err).Msg("error parsing vpcprefix id in instance vpcprefix request")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC Prefix ID specified in request data is not valid", nil)
			}
			vpcPrefixIDs = append(vpcPrefixIDs, vpcPrefixID)
			vpcPrefixIfcMap[vpcPrefixID]++
		}
	}

	// Batch fetch Subnets from DB
	subnetIDMap := make(map[uuid.UUID]*cdbm.Subnet)
	if len(subnetIDs) > 0 {
		subnetList, _, err := sbDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{SubnetIDs: subnetIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Subnets from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnets from DB by IDs", nil)
		}
		for i := range subnetList {
			subnetIDMap[subnetList[i].ID] = &subnetList[i]
		}
	}

	// Batch fetch VPC Prefixes from DB
	vpcPrefixIDMap := make(map[uuid.UUID]*cdbm.VpcPrefix)
	if len(vpcPrefixIDs) > 0 {
		vpcPrefixList, _, err := vpDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{VpcPrefixIDs: vpcPrefixIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving VPC Prefixes from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Prefixes from DB by IDs", nil)
		}
		for i := range vpcPrefixList {
			vpcPrefixIDMap[vpcPrefixList[i].ID] = &vpcPrefixList[i]
		}
	}

	// Load VPCs only for interfaces using Core-managed prefix selection.
	interfaceVpcIDMap, interfaceVpcErr := loadInstanceInterfaceVpcs(ctx, &logger, uih.dbSession, apiRequest.Interfaces, tenant.ID, site.ID)
	if interfaceVpcErr != nil {
		logger.Warn().Err(interfaceVpcErr).Msg("failed to validate VPCs specified by Instance interfaces")
		return cutil.NewAPIErrorResponse(c, interfaceVpcErr.Code, interfaceVpcErr.Message, interfaceVpcErr.Data)
	}

	existingSubnetIfcMap := map[uuid.UUID]uint64{}
	existingVpcPrefixIfcMap := map[uuid.UUID]uint64{}
	if len(apiRequest.Interfaces) > 0 {
		ifcDAO := cdbm.NewInterfaceDAO(uih.dbSession)
		existingIfcsForCapacity, _, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving existing Interfaces for Instance")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve existing Interfaces for Instance", nil)
		}
		for i := range existingIfcsForCapacity {
			eifc := &existingIfcsForCapacity[i]
			if eifc.Status == cdbm.InterfaceStatusDeleting {
				continue
			}

			if eifc.SubnetID != nil {
				existingSubnetIfcMap[*eifc.SubnetID]++
			}
			if eifc.VpcPrefixID != nil {
				existingVpcPrefixIfcMap[*eifc.VpcPrefixID]++
			}
		}
	}

	// Validate each Interface against fetched data
	dbInterfaces := []cdbm.Interface{}
	isDeviceInfoPresent := false
	pfWithinVPC := []uuid.UUID{}
	primaryPhysicalInterfaceUsesVpcSelection := false
	allFoundVpcIds := goset.NewSet[uuid.UUID]()

	// Prepare the unique set of all VPC IDs for this instance update.
	allRequestedVpcIds := goset.NewSet[uuid.UUID]()
	for _, vpcID := range apiRequest.SecondaryVpcIDs {
		id, err := uuid.Parse(vpcID)
		if err != nil {
			logger.Error().Msgf("invalid VPC ID %v in secondaryVpcIds in request data", vpcID)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid VPC ID `%s` in secondaryVpcIds in request data", vpcID), nil)
		}

		if !allRequestedVpcIds.Add(id) {
			logger.Error().Msgf("duplicate ID %s found in `secondaryVpcIds`", id)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Duplicate ID `%s` found in `secondaryVpcIds`", id), nil)
		}
	}

	// Now add the primary VPC to the list.
	// If add failed, it means the ID already existed, but
	// the primary VPC of the instance shouldn't exist in the secondary list.
	if !allRequestedVpcIds.Add(vpc.ID) {
		logger.Error().Msgf("primary VPC ID: %s for Instance must not be listed in `secondaryVpcIds`", vpc.ID)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Primary VPC ID: %s for Instance must not be listed in `secondaryVpcIds`", vpc.ID), nil)
	}

	subnetsForUsage := make([]*cdbm.Subnet, 0, len(subnetIfcMap))
	for subnetID := range subnetIfcMap {
		if sn, ok := subnetIDMap[subnetID]; ok {
			subnetsForUsage = append(subnetsForUsage, sn)
		}
	}
	subnetUsageMap, usageErr := sbDAO.GetPrefixUsage(ctx, nil, subnetsForUsage...)
	if usageErr != nil {
		logger.Error().Err(usageErr).Msg("error getting prefix usage for Subnets")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to get prefix usage for Subnet", nil)
	}

	vpcPrefixesForUsage := make([]*cdbm.VpcPrefix, 0, len(vpcPrefixIfcMap))
	for vpcPrefixID := range vpcPrefixIfcMap {
		if vp, ok := vpcPrefixIDMap[vpcPrefixID]; ok {
			vpcPrefixesForUsage = append(vpcPrefixesForUsage, vp)
		}
	}
	vpcPrefixUsageMap, vpUsageErr := vpDAO.GetPrefixUsage(ctx, nil, vpcPrefixesForUsage...)
	if vpUsageErr != nil {
		logger.Error().Err(vpUsageErr).Msg("error getting prefix usage for VPC Prefixes")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to get prefix usage for VPC Prefix", nil)
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
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have Ethernet network virtualization type in order to create Subnet based interfaces", instance.VpcID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have Ethernet network virtualization type in order to create Subnet based interfaces", instance.VpcID), nil)
			}

			// Check if Subnet is exhausted
			incomingInterfaceIPs := uint64(0)
			if subnetIfcMap[subnetID] > existingSubnetIfcMap[subnetID] {
				incomingInterfaceIPs = subnetIfcMap[subnetID] - existingSubnetIfcMap[subnetID]
			}

			subnetUsage := subnetUsageMap[subnetID]
			if incomingInterfaceIPs > 0 && subnetUsage != nil && subnetUsage.AvailableIPs > 0 && subnetUsage.AcquiredIPs+incomingInterfaceIPs > subnetUsage.AvailableIPs {
				msg := fmt.Sprintf(
					"Subnet %v does not have enough IP addresses: %d of %d IP addresses remain available, but the %d additional interface(s) in this request require %d IP address(es)",
					subnetID, subnetUsage.AvailableIPs-subnetUsage.AcquiredIPs, subnetUsage.AvailableIPs, incomingInterfaceIPs, incomingInterfaceIPs,
				)
				logger.Warn().Msg(msg)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				SubnetID:           &subnetID,
				Subnet:             subnet,
				IsPhysical:         ifc.IsPhysical,
				RequestedIpAddress: nil, // RequestedIpAddress requires a VPC prefix, and model validation enforces this.
				Status:             cdbm.InterfaceStatusPending,
			})
		}

		if ifc.VpcPrefixID != nil {
			vpcPrefixID := uuid.MustParse(*ifc.VpcPrefixID)

			vpcPrefix, ok := vpcPrefixIDMap[vpcPrefixID]
			if !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find VPC Prefix with ID specified in request data", nil)
			}

			if vpcPrefix.TenantID != tenant.ID {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request is not owned by Tenant", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request is not owned by Tenant", vpcPrefixID), nil)
			}

			if vpcPrefix.Status != cdbm.VpcPrefixStatusReady {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request data is not in Ready state", vpcPrefixID), nil)
			}

			if vpcPrefix.SiteID != site.ID {
				logger.Warn().Msg(fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC Prefix: %v specified in request does not belong to Site", vpcPrefixID), nil)
			}

			// If the interface is associated with a VPC ID that the user
			// didn't expect, reject the request.
			if !allRequestedVpcIds.Contains(vpcPrefix.VpcID) {
				logger.Error().Msgf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix.ID, vpcPrefix.VpcID)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("One or more Interfaces specify VPC Prefix: %s belonging to VPC: %s which is not specified in 'vpcId' or 'secondaryVpcIds'", vpcPrefix.ID, vpcPrefix.VpcID), nil)
			}

			// Collect the VPC IDs actually found based on
			// interface definitions.
			allFoundVpcIds.Add(vpcPrefix.VpcID)

			if ifc.Device != nil && ifc.DeviceInstance != nil {
				isDeviceInfoPresent = true
			}

			// The requirement that the VpcID of a prefix being associated with an interface must match the VPC of the instance
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

			if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", instance.VpcID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefix based interfaces", instance.VpcID), nil)
			}

			// Check if VPC Prefix is exhausted
			incomingInterfaceIPs := uint64(0)
			if vpcPrefixIfcMap[vpcPrefixID] > existingVpcPrefixIfcMap[vpcPrefixID] {
				incomingInterfaceIPs = vpcPrefixIfcMap[vpcPrefixID] - existingVpcPrefixIfcMap[vpcPrefixID]
			}

			vpUsage := vpcPrefixUsageMap[vpcPrefixID]
			if incomingInterfaceIPs > 0 && vpUsage != nil && vpUsage.AvailableIPs > 0 && vpUsage.AcquiredIPs+incomingInterfaceIPs*2 > vpUsage.AvailableIPs {
				msg := fmt.Sprintf(
					"VPC Prefix %v does not have enough IP addresses: %d of %d IP addresses remain available, but the %d additional interface(s) in this request require %d IP addresses",
					vpcPrefixID, vpUsage.AvailableIPs-vpUsage.AcquiredIPs, vpUsage.AvailableIPs, incomingInterfaceIPs, incomingInterfaceIPs*2,
				)
				logger.Warn().Msg(msg)
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
			}

			dbInterfaces = append(dbInterfaces, cdbm.Interface{
				VpcPrefixID:          &vpcPrefixID,
				VpcPrefix:            vpcPrefix, // We attach this here so it can be used when we convert to the API model.
				RequestedIpAddress:   ifc.IPAddress,
				InlineRoutingProfile: ifc.InlineRoutingProfile.ToDB(),
				Device:               ifc.Device,
				DeviceInstance:       ifc.DeviceInstance,
				VirtualFunctionID:    ifc.VirtualFunctionID,
				IsPhysical:           ifc.IsPhysical,
				Status:               cdbm.InterfaceStatusPending})
		}

		if ifc.VpcID != nil {
			interfaceVpcID := uuid.MustParse(*ifc.VpcID)
			interfaceVpc := interfaceVpcIDMap[interfaceVpcID]
			if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
				logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC based interfaces", instance.VpcID))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC based interfaces", instance.VpcID), nil)
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

	// If there are ethernet interfaces for this Instance,
	// validate the network plan.
	if len(dbInterfaces) > 0 &&
		vpc.NetworkVirtualizationType != nil &&
		*vpc.NetworkVirtualizationType == cdbm.VpcFNN {
		if len(pfWithinVPC) == 0 || pfWithinVPC[0] != vpc.ID {
			// Use the VPC-selection response when the primary physical Interface selects a VPC
			// by ID. If no primary physical Interface was found, any Interface selecting a VPC
			// by ID is enough to prefer this response over the legacy VPC Prefix response.
			if primaryPhysicalInterfaceUsesVpcSelection || (len(pfWithinVPC) == 0 && len(interfaceVpcIDMap) > 0) {
				logger.Error().Msg("the physical interface must use the Instance VPC")
				if !isDeviceInfoPresent {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The physical Interface must use the VPC specified in `vpcId`", nil)
				}
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The physical Interface for deviceInstance: 0 must use the VPC specified in `vpcId`", nil)
			}

			logger.Error().Msg("the primary physical interface must use a VPC prefix that matches with Instance VPC")

			if !isDeviceInfoPresent {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The physical Interface must use a VPC Prefix that belongs to VPC specified in `vpcId`", nil)
			} else {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The physical Interface for deviceInstance: 0 must use a VPC Prefix that belongs to VPC specified in `vpcId`", nil)
			}
		}
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

	mcDAO := cdbm.NewMachineCapabilityDAO(uih.dbSession)

	// Validate DPU Interfaces if Instance Type has Network Capability with DPU device type
	if isDeviceInfoPresent {
		// Get Network Capabilities with DPU device type
		var dpuNetworkCaps []cdbm.MachineCapability
		var dpuNetworkCapCount int
		if instance.InstanceTypeID != nil {
			dpuNetworkCaps, dpuNetworkCapCount, err = mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{*instance.InstanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeNetwork), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving Machine Capabilities from DB for Instance Type")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DPU aware Network Capabilities for Instance Type", nil)
			}
		}

		if dpuNetworkCapCount == 0 {
			dpuNetworkCaps, dpuNetworkCapCount, err = mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeNetwork), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeDPU), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving DPU aware Network Capabilities from DB for Machine")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DPU aware Network Capabilities for Machine", nil)
			}
		}

		if dpuNetworkCapCount == 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Device and Device Instance cannot be specified if Instance Type or Machine doesn't have Network Capability with DPU device type", nil)
		}

		// Validate DPU Interfaces if Instance Type DPU capability is present and matches with the request
		err = apiRequest.ValidateMultiEthernetDeviceInterfaces(dpuNetworkCaps, dbInterfaces)
		if err != nil {
			logger.Error().Msgf("Failed to validate configuration for one or more multi-Ethernet device Interfaces: %s", err)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate configuration for one or more multi-Ethernet device Interfaces", err)
		}
	}

	// Collect all InfiniBand Partition IDs for batch query
	ibpIDs := []uuid.UUID{}
	for _, ibic := range apiRequest.InfiniBandInterfaces {
		ibpID, err := uuid.Parse(ibic.InfiniBandPartitionID)
		if err != nil {
			logger.Warn().Err(err).Msg("error parsing infiniband partition id in instance infiniband interface request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Partition ID: %v specified in request data is not valid", ibic.InfiniBandPartitionID), nil)
		}
		ibpIDs = append(ibpIDs, ibpID)
	}

	// Batch fetch InfiniBand Partitions from DB
	ibpIDMap := make(map[uuid.UUID]*cdbm.InfiniBandPartition)
	if len(ibpIDs) > 0 {
		ibpDAO := cdbm.NewInfiniBandPartitionDAO(uih.dbSession)
		ibps, _, err := ibpDAO.GetAll(ctx, nil, cdbm.InfiniBandPartitionFilterInput{InfiniBandPartitionIDs: ibpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving InfiniBand Partitions from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Partitions from DB by IDs", nil)
		}
		for i := range ibps {
			ibpIDMap[ibps[i].ID] = &ibps[i]
		}
	}

	// Validate each InfiniBand Partition
	for _, ibic := range apiRequest.InfiniBandInterfaces {
		ibpID := uuid.MustParse(ibic.InfiniBandPartitionID)

		ibp, ok := ibpIDMap[ibpID]
		if !ok {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find InfiniBand Partition with ID: %v specified in request data", ibic.InfiniBandPartitionID), nil)
		}

		if ibp.SiteID != site.ID {
			logger.Warn().Msg(fmt.Sprintf("InfiniBandPartition: %v specified in request does not match with Instance Site", ibpID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition: %v specified in request does not match with Instance Site", ibpID), nil)
		}

		if ibp.TenantID != tenant.ID {
			logger.Warn().Msg(fmt.Sprintf("InfiniBandPartition: %v specified in request is not owned by Tenant", ibpID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition: %v specified in request is not owned by Tenant", ibpID), nil)
		}

		if ibp.ControllerIBPartitionID == nil || ibp.Status != cdbm.InfiniBandPartitionStatusReady {
			logger.Warn().Msg(fmt.Sprintf("InfiniBandPartition: %v specified in request data is not in Ready state", ibpID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("InfiniBand Partition: %v specified in request data is not in Ready state", ibpID), nil)
		}
	}

	// Get InfiniBand Capabilities
	if len(apiRequest.InfiniBandInterfaces) > 0 {
		var ibCapCount int
		var ibCaps []cdbm.MachineCapability

		if instance.InstanceTypeID != nil {
			ibCaps, ibCapCount, err = mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{*instance.InstanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeInfiniBand), nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving InfiniBand Capabilities from DB for Instance Type")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Capabilities for Instance Type", nil)
			}
		}

		if ibCapCount == 0 {
			ibCaps, ibCapCount, err = mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeInfiniBand), nil, nil, nil, nil, nil, nil, nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving InfiniBand Capabilities from DB for Machine")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Capabilities for Machine", nil)
			}
		}

		if ibCapCount == 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "InfiniBand Interfaces cannot be specified if Instance Type or Machine doesn't have InfiniBand Capability", nil)
		}

		// Validate InfiniBand Interfaces if Instance Type has InfiniBand Capability
		err = apiRequest.ValidateInfiniBandInterfaces(ibCaps)
		if err != nil {
			logger.Error().Err(err).Msg("Failed to validate InfiniBand interfaces in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate InfiniBand Interfaces specified in request data", err)
		}
	}

	// Collect all DPU Extension Service IDs for batch query
	desIDs := []uuid.UUID{}
	for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
		desID, err := uuid.Parse(adesdr.DpuExtensionServiceID)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid DPU Extension Service ID: %s specified in request", adesdr.DpuExtensionServiceID), nil)
		}
		desIDs = append(desIDs, desID)
	}

	// Batch fetch DPU Extension Services from DB
	desDAO := cdbm.NewDpuExtensionServiceDAO(uih.dbSession)
	desIDMap := make(map[uuid.UUID]*cdbm.DpuExtensionService)
	if len(desIDs) > 0 {
		dess, _, err := desDAO.GetAll(ctx, nil, cdbm.DpuExtensionServiceFilterInput{DpuExtensionServiceIDs: desIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving DPU Extension Services from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DPU Extension Services from DB by IDs", nil)
		}
		for i := range dess {
			desIDMap[dess[i].ID] = &dess[i]
		}
	}

	// Validate each DPU Extension Service
	for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
		desID := uuid.MustParse(adesdr.DpuExtensionServiceID)

		des, ok := desIDMap[desID]
		if !ok {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find DPU Extension Service with ID: %s", desID), nil)
		}

		if des.TenantID != tenant.ID {
			logger.Warn().Str("Tenant ID", tenant.ID.String()).Str("DPU Extension Service ID", desID.String()).Msg("DPU Extension Service does not belong to current Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("DPU Extension Service: %s does not belong to current Tenant", desID.String()), nil)
		}

		if des.SiteID != site.ID {
			logger.Warn().Str("Site ID", site.ID.String()).Str("DPU Extension Service ID", desID.String()).Msg("DPU Extension Service does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("DPU Extension Service: %s does not belong to Site where Instance is being created", desID.String()), nil)
		}

		versionFound := false
		for _, version := range des.ActiveVersions {
			if version == adesdr.Version {
				versionFound = true
				break
			}
		}
		if !versionFound {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Version: %s was not found for DPU Extension Service: %s", adesdr.Version, desID.String()), nil)
		}
	}

	if len(apiRequest.NVLinkInterfaces) > 0 {
		nvlIfcDAO := cdbm.NewNVLinkInterfaceDAO(uih.dbSession)
		nvlIfcs, _, err := nvlIfcDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLink Interfaces from DB for Instance")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Interfaces for Instance", nil)
		}

		// Discard if VPC has default NVLink Logical Partition specified and NVLink Interfaces are exists
		if len(nvlIfcs) > 0 && vpc.NVLinkLogicalPartitionID != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot update NVLink Interfaces if VPC has default NVLink Logical Partition and NVLink Interfaces already exist for the Instance", nil)
		}
	}

	// Collect all NVLink Logical Partition IDs for batch query
	nvllpIDs := []uuid.UUID{}
	for _, nvlifc := range apiRequest.NVLinkInterfaces {
		nvllpID, err := uuid.Parse(nvlifc.NVLinkLogicalPartitionID)
		if err != nil {
			logger.Warn().Err(err).Msg("error parsing NVLink Logical Partition id in instance NVLink Interface request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition ID: %v specified in request data is not valid", nvlifc.NVLinkLogicalPartitionID), nil)
		}
		nvllpIDs = append(nvllpIDs, nvllpID)
	}

	// Deduplicate partition IDs before the batch fetch (multiple GPUs may share the same partition)
	nvllpIDs = goset.NewSet(nvllpIDs...).ToSlice()

	// Batch fetch NVLink Logical Partitions from DB
	nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(uih.dbSession)
	nvllpIDMap := make(map[uuid.UUID]*cdbm.NVLinkLogicalPartition)
	if len(nvllpIDs) > 0 {
		nvllps, _, err := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{NVLinkLogicalPartitionIDs: nvllpIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLink Logical Partitions from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions from DB by IDs", nil)
		}
		for i := range nvllps {
			nvllpIDMap[nvllps[i].ID] = &nvllps[i]
		}
	}

	// Validate each NVLink Logical Partition
	dbnvlic := []cdbm.NVLinkInterface{}
	for _, nvlifc := range apiRequest.NVLinkInterfaces {
		nvllpID := uuid.MustParse(nvlifc.NVLinkLogicalPartitionID)

		nvllp, ok := nvllpIDMap[nvllpID]
		if !ok {
			logger.Error().Msg("error retrieving NVLink Logical Partition from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find NVLink Logical Partition with ID: %s specified in request data", nvllpID), nil)
		}

		if nvllp.SiteID != instance.SiteID {
			logger.Warn().Msg(fmt.Sprintf("NVLink Logical Partition: %v specified in request does not match with Instance Site", nvllpID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition: %v specified in request does not match with Instance Site", nvllpID), nil)
		}

		if nvllp.TenantID != instance.TenantID {
			logger.Warn().Msg(fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not owned by Tenant", nvllpID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not owned by Tenant", nvllpID), nil)
		}

		if nvllp.Status != cdbm.NVLinkLogicalPartitionStatusReady {
			logger.Warn().Msg(fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not in Ready state", nvllpID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition: %v specified in request data is not in Ready state", nvllpID), nil)
		}

		dbnvlic = append(dbnvlic, cdbm.NVLinkInterface{NVLinkLogicalPartitionID: nvllp.ID, DeviceInstance: nvlifc.DeviceInstance})
	}

	// Validate NVLink interfaces if Instance Type has NVLink (GPU) Capability
	if len(apiRequest.NVLinkInterfaces) > 0 {
		var nvlCapCount int
		var nvlCaps []cdbm.MachineCapability

		if instance.InstanceTypeID != nil {
			// Try to get GPU capabilities from Instance Type first
			nvlCaps, nvlCapCount, err = mcDAO.GetAll(ctx, nil, nil, []uuid.UUID{*instance.InstanceTypeID}, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeGPU), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving GPU Machine Capabilities from DB for Instance Type")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve GPU Capabilities for Instance Type", nil)
			}
		}

		// If Instance was not created using Instance Type, or Instance Type does not have NVLink  Capability, get capabilities from Machine
		if nvlCapCount == 0 {
			nvlCaps, nvlCapCount, err = mcDAO.GetAll(ctx, nil, []string{machine.ID}, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityTypeGPU), nil, nil, nil, nil, nil, cdb.GetTypedStrPtr(cdbm.MachineCapabilityDeviceTypeNVLink), nil, nil, nil, cutil.GetPtr(cdbp.TotalLimit), nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving GPU Machine Capabilities from DB for Instance's Machine")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve GPU Capabilities for Instance's Machine", nil)
			}
		}

		if nvlCapCount == 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "NVLink interfaces cannot be specified if Instance Type or Machine doesn't have NVLink GPU Capability", nil)
		}

		// Validate NVLink interfaces if Instance Type has GPU Capability
		err = apiRequest.ValidateNVLinkInterfaces(nvlCaps)
		if err != nil {
			logger.Error().Msgf("NVLink interfaces validation failed: %s", err)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate NVLink interfaces specified in request", err)
		}
	}

	// Values populated inside the transaction closure that are needed for the response.
	var ui *cdbm.Instance
	var newdbIfcs []cdbm.Interface
	var newIbIfcs []cdbm.InfiniBandInterface
	var newNvlIfcs []cdbm.NVLinkInterface
	var updateDesds []cdbm.DpuExtensionServiceDeployment
	var existingIfcs []cdbm.Interface
	var existingIbIfcs []cdbm.InfiniBandInterface
	var existingNvlIfcs []cdbm.NVLinkInterface
	var newOrExistingIbIfcs []cdbm.InfiniBandInterface
	var newOrExistingNvlIfcs []cdbm.NVLinkInterface
	var dbskgs []cdbm.SSHKeyGroup
	var ssds []cdbm.StatusDetail
	reqCtx := ctx
	reIssueInfiniBandInterfaces := false
	reIssueNVLinkInterfaces := false

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var dpsProfileRollback func() error

	err = cdb.WithTx(ctx, uih.dbSession, func(tx *cdb.Tx) error {
		if uih.cfg.GetDPSEnabled() && apiRequest.PowerProfile != nil {
			lockErr := powerutil.AcquireVPCPowerLock(ctx, tx, vpc.ID)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to serialize DPS operations for VPC")
				return cutil.NewAPIError(http.StatusConflict, "Another power operation is already in progress for the VPC", nil)
			}
			lockedVPC, lockErr := cdbm.NewVpcDAO(uih.dbSession).GetByID(ctx, tx, vpc.ID, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to reload VPC after acquiring its power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload VPC power configuration", nil)
			}
			vpc = lockedVPC
			if vpc.PowerResourceGroup == nil {
				return cutil.NewAPIError(http.StatusBadRequest, "A power profile update requires the VPC to have a power resource group", nil)
			}
		}

		// Prepare DAOs
		sdDAO := cdbm.NewStatusDetailDAO(uih.dbSession)

		// apiRequest will be mutated for use in UpdateFromParams.
		// osConfig will hold the struct/data for use with Temporal/NICo calls.
		// Errors will be returned already in the form of cutil.NewAPIError
		osConfig, osID, oserr := uih.buildInstanceUpdateRequestOsConfig(c, &logger, &apiRequest, instance, site)
		if oserr != nil {
			// buildInstanceUpdateRequestOsConfig already handles logging,
			// so this is a bit redundant, but this log brings you to the
			// actual call site.  I think buildInstanceUpdateRequestOsConfig
			// would ideally return only `error` and let the logging and
			// and cutil.NewAPIErrorResponse(...) happen here, but we
			// have at least one StatusInternalServerError case that would
			// be hidden if we merge it all under StatusBadRequest here.
			logger.Error().Err(oserr).Msg("error building os config for updating Instance")
			return oserr
		}

		if uih.cfg.GetDPSEnabled() && apiRequest.PowerProfile != nil && vpc.PowerResourceGroup != nil {
			previousProfile := ""
			if instance.PowerProfile != nil {
				previousProfile = *instance.PowerProfile
			}
			if strings.TrimSpace(previousProfile) != strings.TrimSpace(*apiRequest.PowerProfile) {
				var dpsErr error
				dpsProfileRollback, dpsErr = powerutil.UpdateMachinePower(ctx, uih.dps, *vpc.PowerResourceGroup, powerutil.MachinePowerAssignment{
					MachineID:    machine.ID,
					PowerProfile: *apiRequest.PowerProfile,
				}, previousProfile)
				if dpsErr != nil {
					logger.Error().Err(dpsErr).Str("machineID", machine.ID).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("DPS rejected Instance power-profile update")
					return cutil.NewAPIError(http.StatusServiceUnavailable, "DPS rejected Instance power-profile update", nil)
				}
			}
		}

		// Update Instance
		// Once details are fully built, we can just fill out all the columns we have.
		// Postgres either updates a row or it does not.
		// HOT update should not apply here.
		var derr error
		ui, derr = instanceDAO.Update(ctx, tx,
			cdbm.InstanceUpdateInput{
				InstanceID: instanceID,
				InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{
					Name:                     apiRequest.Name,
					Description:              apiRequest.Description,
					OperatingSystemID:        osID,
					IpxeScript:               apiRequest.IpxeScript,
					AlwaysBootWithCustomIpxe: apiRequest.AlwaysBootWithCustomIpxe,
					NetworkSecurityGroupID:   nsgID,
					PhoneHomeEnabled:         apiRequest.PhoneHomeEnabled,
					Status:                   instanceStatusConfiguring,
					UserData:                 apiRequest.UserData,
					AutoNetwork:              apiRequest.AutoNetwork,
					Labels:                   apiRequest.Labels,
					PowerProfile:             apiRequest.PowerProfile,
				},
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Instance", nil)
		}

		clearInput := cdbm.InstanceClearInput{InstanceID: instanceID}
		shouldClear := false
		// If this request is attempting to clear the OS for the instance, set it.
		if apiRequest.OperatingSystemID != nil && *apiRequest.OperatingSystemID == "" {
			clearInput.OperatingSystemID = true
			shouldClear = true
		}

		// If this request is attempting to clear the NSG for the instance, set it.
		if apiRequest.NetworkSecurityGroupID != nil {
			if *apiRequest.NetworkSecurityGroupID == "" {
				clearInput.NetworkSecurityGroupID = true
			}

			// We should always clear details for any NSG change so that users don't see stale
			// status.
			clearInput.NetworkSecurityGroupPropagationDetails = true
			shouldClear = true
		}

		if apiRequest.PowerProfile != nil && *apiRequest.PowerProfile == "" {
			clearInput.PowerProfile = true
			shouldClear = true
		}

		// Clear it in the db if something should be cleared.
		if shouldClear {
			ui, derr = instanceDAO.Clear(ctx, tx, clearInput)
			if derr != nil {
				logger.Error().Err(derr).Msg("error clearing requested Instance properties")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to clear requested Instance properties", nil)
			}
		}

		// Save update status in DB
		// Create status detail for instance based on updates requested
		statusMessage := cutil.GetPtr("received Instance config update request, processing")

		_, serr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ui.ID.String(), Status: *cutil.GetPtr(cdbm.InstanceStatusConfiguring), Message: statusMessage})
		if serr != nil {
			logger.Error().Err(serr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create status detail for Instance update", nil)
		}

		// Get the existing ssh key group instance associations records from the db
		skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(uih.dbSession)
		skgias, _, derr := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
			SiteIDs:     []uuid.UUID{site.ID},
			InstanceIDs: []uuid.UUID{instanceID},
		}, cdbp.PageInput{}, []string{cdbm.SSHKeyGroupRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving ssh key group instance association Details from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve SSH Key Group Instance Association for Instance", nil)
		}

		// We'll need a list of the IDs as a string slice to
		// send along in the config update request to nico.
		var instanceSshKeyGroupIds []string

		if apiRequest.SSHKeyGroupIDs == nil {
			// If no change in keygroups, we just need to build the keygroup and keygroup ID
			// lists from the existing instance data so we can send to nico
			// and return it to the client.
			for _, skgia := range skgias {
				dbskgs = append(dbskgs, *skgia.SSHKeyGroup)
				instanceSshKeyGroupIds = append(instanceSshKeyGroupIds, skgia.SSHKeyGroupID.String())
			}
		} else {
			existingSkgiasBySkg := map[uuid.UUID]*cdbm.SSHKeyGroupInstanceAssociation{}
			for _, skgia := range skgias {
				existingSkgiasBySkg[skgia.SSHKeyGroupID] = &skgia
			}

			skgDAO := cdbm.NewSSHKeyGroupDAO(uih.dbSession)
			skgsaDAO := cdbm.NewSSHKeyGroupSiteAssociationDAO(uih.dbSession)

			incomingSkgMap := map[uuid.UUID]bool{}

			// Determine which SSH Key Group Associations to add.
			for _, skgIDStr := range apiRequest.SSHKeyGroupIDs {
				skgID, err := uuid.Parse(skgIDStr)
				if err != nil {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update Instance, Invalid SSH Key Group ID: %s", skgIDStr), nil)
				}

				// If the user request has a duplicate, we can skip it.
				if incomingSkgMap[skgID] {
					continue
				}

				incomingSkgMap[skgID] = true

				skgia, found := existingSkgiasBySkg[skgID]
				// If the SKG is already associated with the Instance, we can
				// skip any DB work and just add the keygroup to the lists we'll
				// send to nico and back to the client.
				if found {
					dbskgs = append(dbskgs, *skgia.SSHKeyGroup)
					instanceSshKeyGroupIds = append(instanceSshKeyGroupIds, skgID.String())
					continue
				}

				// If the SKG is new and not already associated with the Instance
				// we need to associate the SSH Key Group to the Instance.

				// Validate the SSH Key for which this SSH Key Group is being associated.
				sshkeygroup, serr := skgDAO.GetByID(ctx, tx, skgID, nil)
				if serr != nil {
					if serr == common.ErrInvalidID {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update Instance, Invalid SSH Key Group ID: %s", skgID), nil)
					}
					if serr == cdb.ErrDoesNotExist {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update Instance, Could not find SSH Key Group with ID: %s ", skgID), nil)
					}

					logger.Warn().Err(serr).Str("SSH Key Group ID", skgID.String()).Msg("error retrieving SSH Key Group from DB by ID")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to retrieve SSH Key Group with ID `%s`specified in request, DB error", skgID), nil)
				}

				if sshkeygroup.TenantID != ui.TenantID {
					logger.Warn().Str("Tenant ID", ui.TenantID.String()).Str("SSH Key Group ID", skgID.String()).Msg("SSH Key Group does not belong to current Tenant")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update Instance, SSH Key Group with ID: %s does not belong to Tenant", skgID), nil)
				}

				// Verify if the SSHKeyGroupSiteAssociation exists
				_, serr = skgsaDAO.GetBySSHKeyGroupIDAndSiteID(ctx, nil, sshkeygroup.ID, site.ID, nil)
				if serr != nil {
					if serr == cdb.ErrDoesNotExist {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("SSH Key Group with ID: %s is not associated with the Site where Instance is being updated", skgID), nil)
					}
					logger.Warn().Err(serr).Str("SSH Key Group ID", skgID.String()).Msg("error retrieving SSH Key Group Site Association from DB by SSH Key Group ID & Site ID")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to determine if SSH Key Group: %s is associated with the Site where Instance is being updated, DB error", skgID), nil)
				}

				_, err = skgiaDAO.Create(ctx, tx, cdbm.SSHKeyGroupInstanceAssociationCreateInput{
					SSHKeyGroupID: skgID,
					SiteID:        site.ID,
					InstanceID:    instance.ID,
					CreatedBy:     dbUser.ID,
				})
				if err != nil {
					logger.Error().Err(err).Msg("failed to create the SSH Key Group Instance Association record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to associate one or more SSH Key Group with Instance, DB error", nil)
				}

				dbskgs = append(dbskgs, *sshkeygroup)
				instanceSshKeyGroupIds = append(instanceSshKeyGroupIds, skgID.String())
			}

			// Determine which SSH Key Group Associations to remove
			for skgID := range existingSkgiasBySkg {
				// Ignore anything see in the users's request.
				// We want to keep those.
				if incomingSkgMap[skgID] {
					continue
				}

				// If not found, we need to disassociate the SSH Key Group from the Instance.
				skgia := existingSkgiasBySkg[skgID]
				err := skgiaDAO.Delete(ctx, tx, skgia.ID)
				if err != nil {
					logger.Error().Err(serr).Str("SSHKeyGroupInstanceAssociation", skgia.ID.String()).Msg("error removing SSH Key Group Instance Association from DB by SSH Key Group Instance Association ID")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to update Instance: %s is associated with the Site where Instance is being updated, DB error", skgia.ID), nil)
				}
			}
		}

		// Create new Interface records in the DB if specified in request

		ifcDAO := cdbm.NewInterfaceDAO(uih.dbSession)

		// OrderAscending is our best-effort to make sure we send
		// NICo the interfaces in the order it originally received them
		// so the config doesn't get rejected.
		existingIfcs, _, derr = ifcDAO.GetAll(ctx, tx, cdbm.InterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}}, []string{cdbm.SubnetRelationName, cdbm.VpcRelationName, cdbm.VpcPrefixRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve current Ethernet Interfaces details for Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current Ethernet Interfaces for Instance, DB error", nil)
		}

		// Create new Interface records in the DB if specified in request.
		//
		// Three branches:
		//   - Switching to auto mode (`ui.AutoNetwork && len(apiRequest.Interfaces) == 0`):
		//     mark every prior explicit interface row as Deleting and
		//     return an empty list. Reads after this should reflect the
		//     auto contract (no explicit interfaces) rather than the
		//     stale rows that pre-dated the mode switch.
		//   - Explicit interfaces in the request: reuse matching rows,
		//     create new rows, and mark only removed rows as Deleting.
		//   - Neither (no interface change, not switching to auto):
		//     carry the existing rows forward.
		switch {
		case ui.AutoNetwork && len(apiRequest.Interfaces) == 0:
			for i := range existingIfcs {
				existingIfcs[i].Status = cdbm.InterfaceStatusDeleting
				_, err := ifcDAO.Update(ctx, tx, cdbm.InterfaceUpdateInput{InterfaceID: existingIfcs[i].ID, Status: cutil.GetPtr(cdbm.InterfaceStatusDeleting)})
				if err != nil {
					logger.Error().Err(err).Msg("failed to update Interface record in DB")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Interface for Instance, DB error", nil)
				}
			}
			newdbIfcs = []cdbm.Interface{}
		case len(apiRequest.Interfaces) > 0:
			existingIfcMap := make(map[string][]cdbm.Interface, len(existingIfcs))
			for existingIfcIndex := range existingIfcs {
				if existingIfcs[existingIfcIndex].Status == cdbm.InterfaceStatusDeleting {
					continue
				}

				key := existingIfcs[existingIfcIndex].EthernetInterfaceKey()
				existingIfcMap[key] = append(existingIfcMap[key], existingIfcs[existingIfcIndex])
			}

			reusedIfcIDs := make(map[uuid.UUID]bool)
			for _, dbifc := range dbInterfaces {
				key := dbifc.EthernetInterfaceKey()
				existingIfcsForKey := existingIfcMap[key]

				if len(existingIfcsForKey) > 0 {
					reusedIfc := existingIfcsForKey[0]
					if len(existingIfcsForKey) == 1 {
						delete(existingIfcMap, key)
					} else {
						existingIfcMap[key] = existingIfcsForKey[1:]
					}

					reusedIfcIDs[reusedIfc.ID] = true
					newdbIfcs = append(newdbIfcs, reusedIfc)
					continue
				}

				input := cdbm.InterfaceCreateInput{
					InstanceID:           instance.ID,
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
					Status:               dbifc.Status,
					CreatedBy:            dbUser.ID,
				}

				newDbifc, serr := ifcDAO.Create(ctx, tx, input)
				if serr != nil {
					logger.Error().Err(serr).Msg("error creating Instance Interface DB entry")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Instance Interface entry for Instance, DB error", nil)
				}

				ifc := *newDbifc
				ifc.Vpc = dbifc.Vpc
				ifc.VpcPrefix = dbifc.VpcPrefix // We created the interface in the DB based on the values in dbifc, so we can populate this as well.
				ifc.Subnet = dbifc.Subnet
				// Add the new Interface to the list of new Interfaces
				newdbIfcs = append(newdbIfcs, ifc)
			}

			unmatchedIfcs := make([]cdbm.Interface, 0, len(existingIfcs)-len(reusedIfcIDs))
			for existingIfcIndex := range existingIfcs {
				if reusedIfcIDs[existingIfcs[existingIfcIndex].ID] {
					continue
				}

				if existingIfcs[existingIfcIndex].Status != cdbm.InterfaceStatusDeleting {
					existingIfcs[existingIfcIndex].Status = cdbm.InterfaceStatusDeleting

					// Deleting rows retain their associations and allocated addresses until Site cleanup releases them.
					_, err := ifcDAO.Update(ctx, tx, cdbm.InterfaceUpdateInput{
						InterfaceID: existingIfcs[existingIfcIndex].ID,
						Status:      cutil.GetPtr(cdbm.InterfaceStatusDeleting),
					})
					if err != nil {
						logger.Error().Err(err).Msg("failed to update Interface record in DB")

						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Interface for Instance, DB error", nil)
					}
				}

				unmatchedIfcs = append(unmatchedIfcs, existingIfcs[existingIfcIndex])
			}

			existingIfcs = unmatchedIfcs
		default:
			newdbIfcs = existingIfcs
		}

		// Create new InfiniBand Interface records in the DB if specified in request
		ibiDAO := cdbm.NewInfiniBandInterfaceDAO(uih.dbSession)

		// OrderAscending is our best-effort to make sure we send NICo the interfaces in the order it originally received them. so the config doesn't get rejected
		existingIbIfcs, _, derr = ibiDAO.GetAll(ctx, tx, cdbm.InfiniBandInterfaceFilterInput{InstanceIDs: []uuid.UUID{instanceID}}, cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InfiniBandInterfaceOrderByCreated, Order: cdbp.OrderAscending}}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve InfinibandInterface details for Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Infiniband Interfaces for Instance, DB error", nil)
		}

		if apiRequest.InfiniBandInterfaces != nil {

			// Bucket existing InfiniBand rows by (partition ID, device name, device instance) so we can align with the incoming request.
			existingIbIfcMap := make(map[string][]cdbm.InfiniBandInterface)

			// There can be multiple historical rows per key; Ordering existingIbIfcs by Created ascending makes the slice per key chronological.
			for i := range existingIbIfcs {
				key := fmt.Sprintf("%s:%s:%d", existingIbIfcs[i].InfiniBandPartitionID.String(), existingIbIfcs[i].Device, existingIbIfcs[i].DeviceInstance)
				existingIbIfcMap[key] = append(existingIbIfcMap[key], existingIbIfcs[i])
			}

			existingReadyIbIfcsCount := 0
			existingPendingIbIfcsCount := 0
			existingDeletingIbIfcsCount := 0

			for _, apiIbIfc := range apiRequest.InfiniBandInterfaces {
				key := fmt.Sprintf("%s:%s:%d", apiIbIfc.InfiniBandPartitionID, apiIbIfc.Device, apiIbIfc.DeviceInstance)
				existingIbIfcsForKey := existingIbIfcMap[key]

				// Check the status of the most recent InfiniBand interface for this (partition, device, device instance) key.
				if len(existingIbIfcsForKey) > 0 {
					mostRecentIbIfc := existingIbIfcsForKey[len(existingIbIfcsForKey)-1]
					if mostRecentIbIfc.Status == cdbm.InfiniBandInterfaceStatusReady {
						// This interface is already ready, we don't need to re-issue the InfiniBand interface
						existingReadyIbIfcsCount++
					} else {
						if mostRecentIbIfc.Updated.After(time.Now().Add(-InfiniBandInterfaceStatusSyncGraceWindow)) {
							if mostRecentIbIfc.Status == cdbm.InfiniBandInterfaceStatusPending {
								existingPendingIbIfcsCount++
							} else if mostRecentIbIfc.Status == cdbm.InfiniBandInterfaceStatusDeleting {
								existingDeletingIbIfcsCount++
							} else if mostRecentIbIfc.Status == cdbm.InfiniBandInterfaceStatusError {
								reIssueInfiniBandInterfaces = true
							}
						} else {
							reIssueInfiniBandInterfaces = true
						}
					}
				} else {
					// No existing InfiniBand interface found for this InfiniBand Partition ID, Device and Device Instance
					reIssueInfiniBandInterfaces = true
				}
			}

			// If we're here and we're not re-issuing InfiniBand interfaces, we need to check if the number of existing InfiniBand interfaces in transition is different from the number of InfiniBand interfaces in the request
			// Assumptions:
			// - There can be no more than 4 InfiniBand Interfaces in Ready state
			// - There can be no more than 4 InfiniBand Interfaces in Pending state
			// - There can more than 4 InfiniBand Interfaces in Deleting state, in multiples of 4
			// - There cannot be Ready and Pending InfiniBand Interfaces at the same time
			// - There cannot be Ready and Deleting InfiniBand Interfaces at the same time
			if !reIssueInfiniBandInterfaces {
				if existingReadyIbIfcsCount > 0 && existingReadyIbIfcsCount != len(apiRequest.InfiniBandInterfaces) {
					reIssueInfiniBandInterfaces = true
				} else if existingPendingIbIfcsCount > 0 && existingPendingIbIfcsCount != len(apiRequest.InfiniBandInterfaces) {
					reIssueInfiniBandInterfaces = true
				} else if existingDeletingIbIfcsCount > 0 && existingDeletingIbIfcsCount != len(apiRequest.InfiniBandInterfaces) {
					reIssueInfiniBandInterfaces = true
				}
			}

			if reIssueInfiniBandInterfaces {
				for _, apiibifc := range apiRequest.InfiniBandInterfaces {
					// NOTE: This is redundant due to earlier validation, but we handle it anyway
					ibpID, err := uuid.Parse(apiibifc.InfiniBandPartitionID)
					if err != nil {
						logger.Error().Err(err).Msg("failed to parse InfinibandPartitionID")
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Failed to parse InfiniBand Partition ID specified in request: %s", apiibifc.InfiniBandPartitionID), nil)
					}

					dbibifc, err := ibiDAO.Create(ctx, tx, cdbm.InfiniBandInterfaceCreateInput{
						InstanceID:            instanceID,
						SiteID:                site.ID,
						InfiniBandPartitionID: ibpID,
						Device:                apiibifc.Device,
						Vendor:                apiibifc.Vendor,
						DeviceInstance:        apiibifc.DeviceInstance,
						IsPhysical:            apiibifc.IsPhysical,
						VirtualFunctionID:     apiibifc.VirtualFunctionID,
						Status:                cdbm.InfiniBandInterfaceStatusPending,
						CreatedBy:             dbUser.ID,
					})

					if err != nil {
						logger.Error().Err(err).Msg("failed to create Infiniband Interface record in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Infiniband Interface for Instance, DB error", nil)
					}

					newIbIfcs = append(newIbIfcs, *dbibifc)
				}

				// Update status of existing InfiniBand Interfaces to Deleting
				for i := range existingIbIfcs {
					existingIbIfcs[i].Status = cdbm.InfiniBandInterfaceStatusDeleting
					_, err = ibiDAO.Update(ctx, tx, cdbm.InfiniBandInterfaceUpdateInput{
						InfiniBandInterfaceID: existingIbIfcs[i].ID,
						Status:                cutil.GetPtr(cdbm.InfiniBandInterfaceStatusDeleting),
					})
					if err != nil {
						logger.Error().Err(err).Msg("failed to update Infiniband Interface record in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Infiniband Interface for Instance, DB error", nil)
					}
				}
			}
		}

		// Fetch existing DPU Extension Service Deployments for the Instance
		desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(uih.dbSession)
		existingDesds, _, derr := desdDAO.GetAll(ctx, tx, cdbm.DpuExtensionServiceDeploymentFilterInput{
			InstanceIDs: []uuid.UUID{instance.ID},
		}, cdbp.PageInput{
			OrderBy: &cdbp.OrderBy{Field: cdbm.DpuExtensionServiceDeploymentOrderByDefault, Order: cdbp.OrderAscending},
			Limit:   cutil.GetPtr(cdbp.TotalLimit),
		}, []string{cdbm.DpuExtensionServiceRelationName})
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve DpuExtensionServiceDeployment details for Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve existing DPU Extension Service Deployments for Instance, DB error", nil)
		}

		// Check if any DPU Extension Service Deployments are being requested to be created or removed
		existingDesdMap := map[string]*cdbm.DpuExtensionServiceDeployment{}
		for _, desd := range existingDesds {
			existingDesdMap[fmt.Sprintf("%s:%s", desd.DpuExtensionServiceID.String(), desd.Version)] = &desd
		}

		updatedDesdMap := map[string]*cdbm.DpuExtensionServiceDeployment{}

		// If DPU extension services were omitted from the request, preserve existing
		// services in the config we send to Site Controller.
		if apiRequest.DpuExtensionServiceDeployments == nil {
			updateDesds = append(updateDesds, existingDesds...)
		} else {
			for _, adesdr := range apiRequest.DpuExtensionServiceDeployments {
				desdID, err := uuid.Parse(adesdr.DpuExtensionServiceID)
				if err != nil {
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Invalid DPU Extension Service ID: %s specified in request", adesdr.DpuExtensionServiceID), nil)
				}

				desvID := fmt.Sprintf("%s:%s", desdID.String(), adesdr.Version)

				existingDesd, exists := existingDesdMap[desvID]
				if exists {
					updateDesds = append(updateDesds, *existingDesd)
					updatedDesdMap[desvID] = existingDesd
				} else {
					newDesd, serr := desdDAO.Create(ctx, tx, cdbm.DpuExtensionServiceDeploymentCreateInput{
						SiteID:                site.ID,
						TenantID:              tenant.ID,
						InstanceID:            instance.ID,
						DpuExtensionServiceID: desdID,
						Version:               adesdr.Version,
						Status:                cdbm.DpuExtensionServiceDeploymentStatusPending,
						CreatedBy:             dbUser.ID,
					})
					if serr != nil {
						logger.Error().Err(serr).Msg("error creating Instance DpuExtensionServiceDeployment record in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create DPU Extension Service Deployment for Instance, DB error", nil)
					}
					des, _ := desIDMap[desdID]
					newDesd.DpuExtensionService = des
					updateDesds = append(updateDesds, *newDesd)
					updatedDesdMap[desvID] = newDesd
				}
			}

			for _, existingDesd := range existingDesds {
				desvID := fmt.Sprintf("%s:%s", existingDesd.DpuExtensionServiceID.String(), existingDesd.Version)
				_, exists := updatedDesdMap[desvID]
				if !exists && existingDesd.Status != cdbm.DpuExtensionServiceDeploymentStatusTerminating {
					// The deployment is not present in request sent by user, update status to Terminating if not already in that state
					_, derr = desdDAO.Update(ctx, tx, cdbm.DpuExtensionServiceDeploymentUpdateInput{
						DpuExtensionServiceDeploymentID: existingDesd.ID,
						Status:                          cutil.GetPtr(cdbm.DpuExtensionServiceDeploymentStatusTerminating)})
					if derr != nil {
						logger.Error().Err(derr).Msg("failed to update DpuExtensionServiceDeployment record in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update DPU Extension Service Deployment for Instance, DB error", nil)
					}
				}
			}
		}

		// Create new NVLink Interface records in the DB if specified in request
		nvlIfcDAO := cdbm.NewNVLinkInterfaceDAO(uih.dbSession)

		// OrderAscending is our best-effort to make sure we send NICo the interfaces in the order it originally received them. so the config doesn't get rejected
		existingNvlIfcs, _, derr = nvlIfcDAO.GetAll(ctx, tx, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{instanceID}}, cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.NVLinkInterfaceOrderByCreated, Order: cdbp.OrderAscending}}, nil)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve NVLink Interfaces details for Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve NVLink interfaces for Instance, DB error", nil)
		}

		if apiRequest.NVLinkInterfaces != nil {
			// Count the number of NVLink interfaces by NVLink Logical Partition ID and Device Instance
			existingNvlIfcMap := make(map[string][]cdbm.NVLinkInterface)

			// There can be multiple NVLink interfaces for the same NVLink Logical Partition ID and Device Instance
			// Ensure that existingNvlIfcs is sorted by created timestamp in ascending order
			for i := range existingNvlIfcs {
				key := fmt.Sprintf("%s:%d", existingNvlIfcs[i].NVLinkLogicalPartitionID.String(), existingNvlIfcs[i].DeviceInstance)
				existingNvlIfcMap[key] = append(existingNvlIfcMap[key], existingNvlIfcs[i])
			}

			nvllpIDs := goset.NewSet[uuid.UUID]()

			existingReadyNvlIfcsCount := 0
			existingPendingNvlIfcsCount := 0
			existingDeletingNvlIfcsCount := 0

			for _, apiNvlIfc := range apiRequest.NVLinkInterfaces {
				// NVLink Logical Partition
				nvllPartitionID, err := uuid.Parse(apiNvlIfc.NVLinkLogicalPartitionID)
				if err != nil {
					logger.Warn().Err(err).Msg("error parsing NVLink Logical Partition id in instance NVLink Interface request")
					return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition ID: %v specified in request data is not valid", apiNvlIfc.NVLinkLogicalPartitionID), nil)
				}
				nvllpIDs.Add(nvllPartitionID)

				// If the number of active NVLink interfaces is unchanged, consume matching keys (multiset match).
				key := fmt.Sprintf("%s:%d", nvllPartitionID.String(), apiNvlIfc.DeviceInstance)
				existingNvlIfcsForKey := existingNvlIfcMap[key]

				// Check the status of the most recent NVLink interface for this key
				if len(existingNvlIfcsForKey) > 0 {
					mostRecentNvlIfc := existingNvlIfcsForKey[len(existingNvlIfcsForKey)-1]
					if mostRecentNvlIfc.Status == cdbm.NVLinkInterfaceStatusReady {
						// This interface is already ready, we don't need to re-issue the NVLink interface
						existingReadyNvlIfcsCount++
					} else {
						if mostRecentNvlIfc.Updated.After(time.Now().Add(-NVLinkInterfaceStatusSyncGraceWindow)) {
							if mostRecentNvlIfc.Status == cdbm.NVLinkInterfaceStatusPending {
								existingPendingNvlIfcsCount++
							} else if mostRecentNvlIfc.Status == cdbm.NVLinkInterfaceStatusDeleting {
								existingDeletingNvlIfcsCount++
							} else if mostRecentNvlIfc.Status == cdbm.NVLinkInterfaceStatusError {
								reIssueNVLinkInterfaces = true
							}
						} else {
							reIssueNVLinkInterfaces = true
						}
					}
					// There are no other possible statuses for an NVLink interface, so we can break out of the loop
				} else {
					// No existing NVLink interface found for this NVLink Logical Partition ID and Device Instance
					reIssueNVLinkInterfaces = true
				}
			}

			// If we're here and we're not re-issuing NVLink interfaces, we need to check if the number of existing NVLink interfaces in transition is different from the number of NVLink interfaces in the request
			// Assumptions:
			// - There can be no more than 4 NVLink Interfaces in Ready state
			// - There can be no more than 4 NVLink Interfaces in Pending state
			// - There can more than 4 NVLink Interfaces in Deleting state, in multiples of 4
			// - There cannot be Ready and Pending NVLink Interfaces at the same time
			// - There cannot be Ready and Deleting NVLink Interfaces at the same time
			if !reIssueNVLinkInterfaces {
				if existingReadyNvlIfcsCount > 0 && existingReadyNvlIfcsCount != len(apiRequest.NVLinkInterfaces) {
					reIssueNVLinkInterfaces = true
				} else if existingPendingNvlIfcsCount > 0 && existingPendingNvlIfcsCount != len(apiRequest.NVLinkInterfaces) {
					reIssueNVLinkInterfaces = true
				} else if existingDeletingNvlIfcsCount > 0 && existingDeletingNvlIfcsCount != len(apiRequest.NVLinkInterfaces) {
					reIssueNVLinkInterfaces = true
				}
			}

			if reIssueNVLinkInterfaces {
				nvllpIDMap := make(map[string]*cdbm.NVLinkLogicalPartition)
				if nvllpIDs.Cardinality() > 0 {
					nvllps, _, err := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{NVLinkLogicalPartitionIDs: nvllpIDs.ToSlice()}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
					if err != nil {
						logger.Error().Err(err).Msg("error retrieving NVLink Logical Partitions from DB by IDs")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions specified in request data, DB error", nil)
					}
					for i := range nvllps {
						nvllpIDMap[nvllps[i].ID.String()] = &nvllps[i]
					}
				}
				for _, apiNvlIfc := range apiRequest.NVLinkInterfaces {
					nvllPartition, ok := nvllpIDMap[apiNvlIfc.NVLinkLogicalPartitionID]
					if !ok {
						return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not find NVLink Logical Partition with ID: %v specified in request data", apiNvlIfc.NVLinkLogicalPartitionID), nil)
					}

					newNvlIfc, err := nvlIfcDAO.Create(ctx, tx, cdbm.NVLinkInterfaceCreateInput{
						InstanceID:               instanceID,
						SiteID:                   site.ID,
						NVLinkLogicalPartitionID: nvllPartition.ID,
						DeviceInstance:           apiNvlIfc.DeviceInstance,
						Status:                   cdbm.NVLinkInterfaceStatusPending,
						CreatedBy:                dbUser.ID,
					})

					if err != nil {
						logger.Error().Err(err).Msg("failed to create NVLink Interface record in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create NVLink Interface for Instance, DB error", nil)
					}
					newNvlIfcs = append(newNvlIfcs, *newNvlIfc)
				}
				// Update status of existing NVLink interfaces to Deleting
				if len(existingNvlIfcs) > 0 {
					nvlIfcUpdateInputs := make([]cdbm.NVLinkInterfaceUpdateInput, len(existingNvlIfcs))
					for i := range existingNvlIfcs {
						existingNvlIfcs[i].Status = cdbm.NVLinkInterfaceStatusDeleting
						nvlIfcUpdateInputs[i] = cdbm.NVLinkInterfaceUpdateInput{
							NVLinkInterfaceID: existingNvlIfcs[i].ID,
							Status:            cutil.GetPtr(cdbm.NVLinkInterfaceStatusDeleting),
						}
					}
					_, err := nvlIfcDAO.UpdateMultiple(ctx, tx, nvlIfcUpdateInputs)
					if err != nil {
						logger.Error().Err(err).Msg("failed to update NVLink Interface records in DB")
						return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update NVLink Interfaces for Instance, DB error", nil)
					}
				}
			}
		}

		// Get Status Details
		ssds, _, derr = sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{ui.ID.String()}}, cdbp.PageInput{})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for Instance from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for Instance", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := uih.scp.GetClientByID(site.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		labels := util.ProtobufLabelsFromAPILabels(ui.Labels)

		description := ""
		if ui.Description != nil {
			description = *ui.Description
		}

		interfaceConfigs := make([]*corev1.InstanceInterfaceConfig, len(newdbIfcs))
		for i := range newdbIfcs {
			ifc := &newdbIfcs[i]
			if ifc.Status == cdbm.InterfaceStatusDeleting {
				// NOTE: Don't send any Interfaces that are being deleted
				continue
			}

			interfaceConfig := &corev1.InstanceInterfaceConfig{
				FunctionType: corev1.InterfaceFunctionType_VIRTUAL_FUNCTION,
			}

			if ifc.SubnetID != nil {
				interfaceConfig.NetworkSegmentId = &corev1.NetworkSegmentId{Value: ifc.SubnetID.String()}
				interfaceConfig.NetworkDetails = &corev1.InstanceInterfaceConfig_SegmentId{
					SegmentId: &corev1.NetworkSegmentId{Value: ifc.SubnetID.String()},
				}
			}

			vpcSelection, serr := instanceInterfaceVpcSelection(ifc)
			if serr != nil {
				logger.Error().Err(serr).Msg("failed to build VPC selection for Instance Interface")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to build VPC selection for Instance Interface", nil)
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

			// Assign Device and DeviceInstance in case of Multi DPU Interface
			if ifc.Device != nil && ifc.DeviceInstance != nil {
				interfaceConfig.Device = ifc.Device
				interfaceConfig.DeviceInstance = uint32(*ifc.DeviceInstance)
			}

			if !ifc.IsPhysical {
				if ifc.VirtualFunctionID != nil {
					vfID := uint32(*ifc.VirtualFunctionID)
					interfaceConfig.VirtualFunctionId = &vfID
				}
			}

			if ifc.RequestedIpAddress != nil {
				interfaceConfig.IpAddress = ifc.RequestedIpAddress
			}
			if ifc.InlineRoutingProfile != nil {
				interfaceConfig.RoutingProfile = ifc.InlineRoutingProfile.ToProto()
			}

			interfaceConfigs[i] = interfaceConfig
		}

		// Populate InfiniBand Interface details for Site Controller request
		// This loop accommodates both cases where InfiniBand Interfaces for updated or no update was requested
		// IF there are any new InfiniBand Interfaces, that means all existing InfiniBand Interfaces will be in Deleting state
		ibInterfaceConfigs := []*corev1.InstanceIBInterfaceConfig{}
		newOrExistingIbIfcs = append(newIbIfcs, existingIbIfcs...)
		for _, newIbIfc := range newOrExistingIbIfcs {
			if newIbIfc.Status == cdbm.InfiniBandInterfaceStatusDeleting {
				// NOTE: Don't send any InfiniBand Interfaces that are being deleted
				continue
			}

			ibInterfaceConfig := &corev1.InstanceIBInterfaceConfig{
				Device:         newIbIfc.Device,
				Vendor:         newIbIfc.Vendor,
				DeviceInstance: uint32(newIbIfc.DeviceInstance),
				FunctionType:   corev1.InterfaceFunctionType_PHYSICAL_FUNCTION,
				IbPartitionId:  &corev1.IBPartitionId{Value: newIbIfc.InfiniBandPartitionID.String()},
			}

			// NOTE: Not supported yet, but ensures future compatibility
			if !newIbIfc.IsPhysical {
				ibInterfaceConfig.FunctionType = corev1.InterfaceFunctionType_VIRTUAL_FUNCTION

				if newIbIfc.VirtualFunctionID != nil {
					vfID := uint32(*newIbIfc.VirtualFunctionID)
					ibInterfaceConfig.VirtualFunctionId = &vfID
				}
			}

			ibInterfaceConfigs = append(ibInterfaceConfigs, ibInterfaceConfig)
		}

		// Populate DPU Extension Service Deployment details for Site Controller request
		desdConfigs := []*corev1.InstanceDpuExtensionServiceConfig{}
		for _, desd := range updateDesds {
			// Skip deployments that are being deleted
			if desd.Status == cdbm.DpuExtensionServiceDeploymentStatusTerminating {
				continue
			}

			desdConfig := &corev1.InstanceDpuExtensionServiceConfig{
				ServiceId: desd.DpuExtensionServiceID.String(),
				Version:   desd.Version,
			}

			desdConfigs = append(desdConfigs, desdConfig)
		}

		// Populate NVLink Interface details for Site Controller request
		// IF there are any new NVLink Interfaces, that means all existing NVLink Interfaces will be in Deleting state
		// This loop accommodates both cases where NVLink Interfaces for updated or no update was requested
		nvlInterfaceConfigs := []*corev1.InstanceNVLinkGpuConfig{}
		newOrExistingNvlIfcs = append(newNvlIfcs, existingNvlIfcs...)
		for _, newNvlIfc := range newOrExistingNvlIfcs {
			if newNvlIfc.Status == cdbm.NVLinkInterfaceStatusDeleting {
				// NOTE: Don't send any NVLink interfaces that are being deleted
				continue
			}

			nvlInterfaceConfig := &corev1.InstanceNVLinkGpuConfig{
				DeviceInstance:     uint32(newNvlIfc.DeviceInstance),
				LogicalPartitionId: &corev1.NVLinkLogicalPartitionId{Value: newNvlIfc.NVLinkLogicalPartitionID.String()},
			}
			nvlInterfaceConfigs = append(nvlInterfaceConfigs, nvlInterfaceConfig)
		}

		// Prepare the config update request workflow object
		updateInstanceRequest := &corev1.InstanceConfigUpdateRequest{
			InstanceId: &corev1.InstanceId{Value: instance.GetSiteID().String()},
			Metadata: &corev1.Metadata{
				Name:        ui.Name,
				Description: description,
				Labels:      labels,
			},
			Config: &corev1.InstanceConfig{
				NetworkSecurityGroupId: ui.NetworkSecurityGroupID,
				PowerProfile:           ui.PowerProfile,
				Tenant: &corev1.TenantConfig{
					TenantOrganizationId: tenant.Org,
					TenantKeysetIds:      instanceSshKeyGroupIds,
				},
				Os:      osConfig,
				Network: buildInstanceNetworkConfig(ui.AutoNetwork, interfaceConfigs, vpc.ControllerVpcID),
				Infiniband: &corev1.InstanceInfinibandConfig{
					IbInterfaces: ibInterfaceConfigs,
				},
				DpuExtensionServices: &corev1.InstanceDpuExtensionServicesConfig{
					ServiceConfigs: desdConfigs,
				},
				Nvlink: &corev1.InstanceNVLinkConfig{
					GpuConfigs: nvlInterfaceConfigs,
				},
			},
		}

		if apiRequest.SpectrumXAttachments != nil {
			spectrumXAttachmentConfigs := make([]*corev1.InstanceSpxAttachment, 0, len(apiRequest.SpectrumXAttachments))
			for _, sac := range apiRequest.SpectrumXAttachments {
				spectrumXAttachmentConfigs = append(spectrumXAttachmentConfigs, sac.ToProto())
			}
			updateInstanceRequest.Config.Spxconfig = &corev1.InstanceSpxConfig{SpxAttachments: spectrumXAttachmentConfigs}
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-update-" + instance.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering instance update workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow to update instance
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateInstance", updateInstanceRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to update Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update Instance on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update Instance workflow")

		// Execute the workflow synchronously
		wferr = we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to update Instance, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "Instance", "UpdateInstance")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Instance update workflow timed out", nil)
			}

			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to update Instance")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update Instance on Site: %s", unwrapped), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update Instance workflow")

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			if dpsProfileRollback != nil {
				rollbackErr := dpsProfileRollback()
				if rollbackErr != nil {
					logger.Error().Err(rollbackErr).Str("machineID", machine.ID).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("failed to compensate DPS after Instance update failure")
				}
			}
			return common.HandleTxError(c, logger, err, "Failed to update Instance, DB transaction error")
		}
	}
	if timeoutResp != nil {
		responseErr := timeoutResp()
		if dpsProfileRollback != nil {
			rollbackErr := dpsProfileRollback()
			if rollbackErr != nil {
				logger.Error().Err(rollbackErr).Str("machineID", machine.ID).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("failed to compensate DPS after Instance update failure")
			}
		}
		return responseErr
	}

	// If existing Interfaces were updated, add them to the response
	if len(existingIfcs) > 0 {
		// Add the existing Interfaces to the response
		newdbIfcs = append(newdbIfcs, existingIfcs...)
	}

	// Create response
	apiInstance := model.NewAPIInstance(ui, site, newdbIfcs, newOrExistingIbIfcs, updateDesds, newOrExistingNvlIfcs, dbskgs, ssds)

	// If the instance has no NSG ID, then we need to check if its parent VPC does.
	// We'll need to pull that separately because the user might not have asked for
	// the VPC relation, so we can't assume that it's there.

	if ui.NetworkSecurityGroupID == nil {
		err = AttachVpcNsgPropagationDetailsToApiInstance(c, reqCtx, &logger, uih.dbSession, ui, newdbIfcs, apiInstance)
		if err != nil {
			return err
		}
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInstance)
}

// AttachVpcNsgPropagationDetailsToApiInstance attaches NSG propagation details to an APIInstance.
// It returns NewAPIErrorResponse directly.
func AttachVpcNsgPropagationDetailsToApiInstance(c echo.Context, ctx context.Context, logger *zerolog.Logger, dbSession *cdb.Session, instance *cdbm.Instance, interfaces []cdbm.Interface, apiInstance *model.APIInstance) error {

	// If there are no ethernet interfaces, there is no VPC attachment to check for NSG
	// propagation.
	if len(interfaces) == 0 {
		return nil
	}

	// Get _all_ the VPCs with which this Instance is associated.
	// Only an instance in a primary VPC that uses FNN virtualization
	// can span multiple VPCs, and the primary interface would still
	// be within the primary VPC of the instance, so we can init
	// the list with that.
	vpcIDs := goset.NewSet[uuid.UUID]()
	for _, ifc := range interfaces {
		if ifc.VpcID != nil {
			vpcIDs.Add(*ifc.VpcID)
		} else if ifc.VpcPrefix != nil {
			vpcIDs.Add(ifc.VpcPrefix.VpcID)
		}

		if ifc.Subnet != nil {
			vpcIDs.Add(ifc.Subnet.VpcID)
		}
	}

	vpcDAO := cdbm.NewVpcDAO(dbSession)

	vpcs, _, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{VpcIDs: vpcIDs.ToSlice()}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC for Instance", nil)
	}

	// If we somehow found no VPCs, that's simply incorrect.
	if len(vpcs) == 0 {
		logger.Error().Err(err).Msg("no VPC found for Instance based on Interfaces")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to find any VPC for Instance based on Interfaces", nil)
	}

	vpcCountWithNsgTotal := 0
	vpcCountWithNsgPropagated := 0

	for _, vpc := range vpcs {

		// If we inherited an NSG from this VPC, then see if we're propagated.
		if vpc.NetworkSecurityGroupID != nil {

			// We found an attached VPC that has an NSG attached,
			// so we'll need to track propagation.
			vpcCountWithNsgTotal++

			if vpc.NetworkSecurityGroupPropagationDetails != nil {
				// If the instance wasn't found in the list of unpropagated instances, then we're propagated.
				if !slices.Contains(vpc.NetworkSecurityGroupPropagationDetails.GetUnpropagatedInstanceIds(), instance.ID.String()) {
					// We're propagated, so count it.
					vpcCountWithNsgPropagated++
				}
			}
		}
	}

	if vpcCountWithNsgTotal > 0 {
		// We've only inherited if our NSG ID is null _and_ the parent
		// NSG ID is not null. I.e., at least one VPC is using an NSG.
		apiInstance.NetworkSecurityGroupInherited = true

		switch {
		case vpcCountWithNsgPropagated == 0:
			apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
				ObjectID:       instance.ID.String(),
				DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusNone,
				Status:         model.APINetworkSecurityGroupPropagationStatusSynchronizing,
			}
		case vpcCountWithNsgPropagated < vpcCountWithNsgTotal:
			apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
				ObjectID:       instance.ID.String(),
				DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusPartial,
				Status:         model.APINetworkSecurityGroupPropagationStatusSynchronizing,
			}
		case vpcCountWithNsgPropagated == vpcCountWithNsgTotal:
			apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
				ObjectID:       instance.ID.String(),
				DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusFull,
				Status:         model.APINetworkSecurityGroupPropagationStatusSynchronized,
			}
		default:
			apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
				ObjectID:       instance.ID.String(),
				DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusUnknown,
				Status:         model.APINetworkSecurityGroupPropagationStatusError,
			}

		}

	}

	return nil
}

// ~~~~~ Get Handler ~~~~~ //

// GetInstanceHandler is the API Handler for getting an Instance
type GetInstanceHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetInstanceHandler initializes and returns a new handler for getting Instance
func NewGetInstanceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetInstanceHandler {
	return GetInstanceHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get an Instance
// @Description Get an Instance for the org
// @Tags Instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Instance"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Success 200 {object} model.APIInstance
// @Router /v2/org/{org}/nico/instance/{id} [get]
func (gih GetInstanceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Instance", "Get", c, gih.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
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

	// Validate role, only Tenant Admins are allowed to retrieve Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InstanceRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Instance ID from URL param
	instanceStrID := c.Param("id")
	instanceID, err := uuid.Parse(instanceStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance ID in URL", nil)
	}

	gih.tracerSpan.SetAttribute(handlerSpan, attribute.String("instance_id", instanceStrID), logger)

	// Get Instance
	instanceDAO := cdbm.NewInstanceDAO(gih.dbSession)

	instance, err := instanceDAO.GetByID(ctx, nil, instanceID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Instance with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gih.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]

	// Check if Instance belongs to Tenant
	if instance.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance does not belong to current Tenant", nil)
	}

	// Get Site for this Instance
	siteDAO := cdbm.NewSiteDAO(gih.dbSession)
	site, err := siteDAO.GetByID(ctx, nil, instance.SiteID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site for Instance", nil)
	}

	// Get the instance Interfaces record from the db
	ifcDAO := cdbm.NewInterfaceDAO(gih.dbSession)
	ifcs, _, err := ifcDAO.GetAll(
		ctx,
		nil,
		cdbm.InterfaceFilterInput{
			InstanceIDs: []uuid.UUID{instance.ID},
		},
		cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InterfaceOrderByCreated, Order: cdbp.OrderAscending}, Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.SubnetRelationName, cdbm.VpcPrefixRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instance Interfaces Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance Interfaces for Instance", nil)
	}

	// Get the instance infiniband interface record from the db
	ibifcDAO := cdbm.NewInfiniBandInterfaceDAO(gih.dbSession)
	ibIfcs, _, err := ibifcDAO.GetAll(
		ctx,
		nil,
		cdbm.InfiniBandInterfaceFilterInput{
			InstanceIDs: []uuid.UUID{instanceID},
		},
		cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.InfiniBandInterfaceOrderByCreated, Order: cdbp.OrderAscending}, Limit: cutil.GetPtr(cdbp.TotalLimit)},
		[]string{cdbm.InfiniBandPartitionRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instance InfiniBand Interfaces Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance InfiniBand Interfaces for Instance", nil)
	}

	// Get the instance NVLink Interface record from the db
	nvlDAO := cdbm.NewNVLinkInterfaceDAO(gih.dbSession)
	nvlIfcs, _, err := nvlDAO.GetAll(
		ctx,
		nil,
		cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{instanceID}},
		cdbp.PageInput{OrderBy: &cdbp.OrderBy{Field: cdbm.NVLinkInterfaceOrderByCreated, Order: cdbp.OrderAscending}, Limit: cutil.GetPtr(cdbp.TotalLimit)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instance NVLink interfaces Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance NVLink interfaces for Instance", nil)
	}

	// Get DPU Extension Service Deployments for the instance
	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(gih.dbSession)
	desds, _, err := desdDAO.GetAll(
		ctx,
		nil,
		cdbm.DpuExtensionServiceDeploymentFilterInput{
			InstanceIDs: []uuid.UUID{instanceID},
		},
		cdbp.PageInput{
			OrderBy: &cdbp.OrderBy{Field: cdbm.DpuExtensionServiceDeploymentOrderByDefault, Order: cdbp.OrderAscending},
			Limit:   cutil.GetPtr(cdbp.TotalLimit),
		},
		[]string{cdbm.DpuExtensionServiceRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving DPU Extension Service Deployments for instance from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DPU Extension Service Deployments for instance", nil)
	}

	// Get the ssh key group instance associations record from the db
	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(gih.dbSession)
	var dbskgs []cdbm.SSHKeyGroup
	skgias, _, err := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
		SiteIDs:     []uuid.UUID{site.ID},
		InstanceIDs: []uuid.UUID{instanceID},
	}, cdbp.PageInput{}, []string{cdbm.SSHKeyGroupRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving ssh key group instance association Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Group Instance Association for Instance", nil)
	}

	for _, skgia := range skgias {
		dbskgs = append(dbskgs, *skgia.SSHKeyGroup)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gih.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{instanceID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for instance from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for instance", nil)
	}

	// Create response
	ins := model.NewAPIInstance(instance, site, ifcs, ibIfcs, desds, nvlIfcs, dbskgs, ssds)

	// If the instance has no NSG ID, then we need to check if any parent VPC does.
	if instance.NetworkSecurityGroupID == nil {
		err = AttachVpcNsgPropagationDetailsToApiInstance(c, ctx, &logger, gih.dbSession, instance, ifcs, ins)
		if err != nil {
			return err
		}
	}

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, ins)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllInstanceHandler is the API Handler for retrieving all Instances
type GetAllInstanceHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllInstanceHandler initializes and returns a new handler for retreiving all Instances
func NewGetAllInstanceHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInstanceHandler {
	return GetAllInstanceHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all Instances
// @Description Get all Instances for the org
// @Tags Instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param infrastructureProviderId query string false "Deprecated: Instances will no longer be filtered by Infrastructure Provider; results are scoped to the org's Tenant. Use siteId to scope results to a specific Infrastructure Provider's Sites."
// @Param siteId query string true "ID of Site"
// @Param vpcId query string true "ID of Vpc"
// @Param instanceTypeId query string false "ID of Instance Type"
// @Param operatingSystemId query string false "ID of Operating System"
// @Param name query string false "Filter by Instance name"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param ipAddress query string false "Filter by IP address. Can be specified multiple times to filter on more than one IP address."
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} []model.APIInstance
// @Router /v2/org/{org}/nico/instance [get]
func (gaih GetAllInstanceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Instance", "GetAll", c, gaih.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
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

	// Validate role, only Tenant Admins are allowed to retrieve Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination request attributes
	err = pageRequest.Validate(cdbm.InstanceOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest,
			"Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InstanceRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	filter := cdbm.InstanceFilterInput{}
	// Get infrastructure ID from query param if specified
	infrastructureProviderIDStr := c.QueryParam("infrastructureProviderId")
	if infrastructureProviderIDStr != "" {
		parsedID, serr := uuid.Parse(infrastructureProviderIDStr)
		if serr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Infrastructure Provider ID in query", nil)
		}

		// Check for Provider existence
		ifpDAO := cdbm.NewInfrastructureProviderDAO(gaih.dbSession)
		_, verr := ifpDAO.GetByID(ctx, nil, parsedID, nil)
		if verr != nil {
			logger.Warn().Err(verr).Msg("error retrieving Infrastructure Provider from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not retrieve Infrastructure Provider with ID specified in query", nil)
		}
		filter.InfrastructureProviderIDs = []uuid.UUID{parsedID}
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gaih.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]
	filter.TenantIDs = append(filter.TenantIDs, tenant.ID)

	// Get site IDs from query param and validate
	stDAO := cdbm.NewSiteDAO(gaih.dbSession)

	var siteIDs []uuid.UUID
	sitesByID := map[uuid.UUID]*cdbm.Site{}
	siteIDStrs := qParams["siteId"]

	for _, siteIDStr := range siteIDStrs {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("siteId", siteIDStrs), logger)
		parsedID, err := uuid.Parse(siteIDStr)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid site ID %v in query", siteIDStr), nil)
		}
		siteIDs = append(siteIDs, parsedID)
	}

	if siteIDs != nil {
		siteIDs = goset.NewSet(siteIDs...).ToSlice()

		sites, _, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{SiteIDs: siteIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Sites from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites s", nil)
		}
		for _, site := range sites {
			sitesByID[site.ID] = &site
		}

		if len(sites) != len(siteIDs) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find one or more Sites specified in query", nil)
		}
	} else {
		tsDAO := cdbm.NewTenantSiteDAO(gaih.dbSession)
		tss, _, err := tsDAO.GetAll(ctx, nil, cdbm.TenantSiteFilterInput{TenantIDs: []uuid.UUID{tenant.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.SiteRelationName})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Sites for Tenant from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites for Tenant, DB error", nil)
		}
		for _, ts := range tss {
			// Check if Site relation was loaded successfully
			if ts.Site != nil {
				sitesByID[ts.Site.ID] = ts.Site
			}
		}
	}

	// Check TenantSite entry
	if len(siteIDs) > 0 {
		tsDAO := cdbm.NewTenantSiteDAO(gaih.dbSession)
		_, count, err := tsDAO.GetAll(
			ctx,
			nil,
			cdbm.TenantSiteFilterInput{
				TenantIDs: []uuid.UUID{tenant.ID},
				SiteIDs:   siteIDs,
			},
			cdbp.PageInput{},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving TenantSite entry")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant's association with Site", nil)
		}

		// We've ensured that the set of siteIDs is unique earlier,
		// so if the counts don't match, then something wasn't found.
		if count != len(siteIDs) {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
				"Tenant is not associated with one or more of the Sites specified in query", nil)
		}
	}

	filter.SiteIDs = siteIDs

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		filter.SearchQuery = searchQuery
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	// Get status from query param
	if statusStrings := qParams["status"]; len(statusStrings) != 0 {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("status", statusStrings), logger)
		for _, status := range statusStrings {
			gaih.tracerSpan.SetAttribute(handlerSpan, attribute.String("status", status), logger)
			_, ok := cdbm.InstanceStatusMap[status]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", status))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
			}
			filter.Statuses = append(filter.Statuses, status)
		}
	}

	// Get VPC IDs from query param
	if vpcIDStrs := qParams["vpcId"]; len(vpcIDStrs) != 0 {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("vpcId", vpcIDStrs), logger)
		for _, vpcIDStr := range vpcIDStrs {
			// Check for Vpc existence
			vpc, verr := common.GetVpcFromIDString(ctx, nil, vpcIDStr, nil, gaih.dbSession)
			if verr != nil {
				if verr == common.ErrInvalidID {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid VPC ID %v in query", vpcIDStr), nil)
				}
				if verr == cdb.ErrDoesNotExist {
					return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find VPC with ID %v specified in query", vpcIDStr), nil)
				}
				logger.Error().Err(verr).Msg("error retrieving Vpc from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve VPC with ID %v specified in query", vpcIDStr), nil)
			}
			filter.VpcIDs = append(filter.VpcIDs, vpc.ID)
		}
	}

	// Get instance type IDs from query param
	if instanceTypeIDStrs := qParams["instanceTypeId"]; len(instanceTypeIDStrs) != 0 {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("instanceTypeId", instanceTypeIDStrs), logger)
		for _, instanceTypeStr := range instanceTypeIDStrs {
			// Check for instance type existence
			instanceType, verr := common.GetInstanceTypeFromIDString(ctx, nil, instanceTypeStr, gaih.dbSession)
			if verr != nil {
				if verr == common.ErrInvalidID {
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid instance type ID %v in query", instanceTypeStr), nil)
				}
				if verr == cdb.ErrDoesNotExist {
					return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find instance type with ID %v specified in query", instanceTypeStr), nil)
				}
				logger.Error().Err(verr).Msg("error retrieving instance type from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve instance type with ID %v specified in query", instanceTypeStr), nil)
			}
			filter.InstanceTypeIDs = append(filter.InstanceTypeIDs, instanceType.ID)
		}
	}

	// Get network security group IDs from query param
	if len(qParams["networkSecurityGroupId"]) > 0 {
		filter.NetworkSecurityGroupIDs = qParams["networkSecurityGroupId"]
	}

	// Get operating system IDs from query param
	if operatingSystemIDStrs := qParams["operatingSystemId"]; len(operatingSystemIDStrs) != 0 {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("operatingSystemId", operatingSystemIDStrs), logger)
		operatingSystemDAO := cdbm.NewOperatingSystemDAO(gaih.dbSession)
		for _, operatingSystemStr := range operatingSystemIDStrs {
			parsedID, err := uuid.Parse(operatingSystemStr)
			if err != nil {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid operating system ID %v in query", operatingSystemStr), nil)
			}

			// Check for operating system existence
			_, verr := operatingSystemDAO.GetByID(ctx, nil, parsedID, nil)
			if verr != nil {
				if verr == cdb.ErrDoesNotExist {
					return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find operating system with ID %v specified in query", operatingSystemStr), nil)
				}
				logger.Error().Err(err).Msg("error retrieving operating system from DB")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve operating system with ID %v specified in query", operatingSystemStr), nil)
			}
			filter.OperatingSystemIDs = append(filter.OperatingSystemIDs, parsedID)
		}
	}

	// Get machine IDs from query param
	if machineIDs := qParams["machineId"]; len(machineIDs) != 0 {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("machineId", machineIDs), logger)
		machineDAO := cdbm.NewMachineDAO(gaih.dbSession)
		machines, _, err := machineDAO.GetAll(ctx, nil, cdbm.MachineFilterInput{MachineIDs: machineIDs, ExcludeMetadata: true}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving machines from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to retrieve machines with IDs %v specified in query", strings.Join(machineIDs, ", ")), nil)
		}
		machineMap := map[string]bool{}
		for _, machine := range machines {
			machineMap[machine.ID] = true
		}
		hasValidMachineID := false
		for _, machineID := range machineIDs {
			_, ok := machineMap[machineID]
			if ok {
				filter.MachineIDs = append(filter.MachineIDs, machineID)
				hasValidMachineID = true
			}
		}
		if !hasValidMachineID {
			// Create pagination response header
			pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, 0, pageRequest.OrderByStr)
			pageHeader, err := json.Marshal(pageReponse)
			if err != nil {
				logger.Error().Err(err).Msg("error marshaling pagination response")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
			}
			c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))
			return c.JSON(http.StatusOK, []model.APIInstance{})
		}
	}

	// Get instance name from query param
	if name := c.QueryParam("name"); name != "" {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.String("name", name), logger)
		filter.Names = []string{name}
	}

	// Get IP addresses from query param and filter by interface IPs
	if ipAddresses := qParams["ipAddress"]; len(ipAddresses) != 0 {
		gaih.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("ipAddress", ipAddresses), logger)

		// GetAll interfaces matching specified IP addresses
		ifcDAO := cdbm.NewInterfaceDAO(gaih.dbSession)
		matchingIfcs, _, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{IPAddresses: ipAddresses}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Interfaces for IP filtering")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Interfaces for IP filtering", nil)
		}

		// Collect Instance ID attribute of matching Interfaces
		instanceIDsWithMatchingIPs := goset.NewSet[uuid.UUID]()
		for _, ifc := range matchingIfcs {
			instanceIDsWithMatchingIPs.Add(ifc.InstanceID)
		}

		// Add InstanceIDs to filter object
		if instanceIDsWithMatchingIPs.Cardinality() > 0 {
			filter.InstanceIDs = instanceIDsWithMatchingIPs.ToSlice()
		} else {
			// No instances match the IP filter, set empty list to get no results
			filter.InstanceIDs = []uuid.UUID{}
		}
	}

	// Get all Instances by Tenant, and Site, if specified
	instanceDAO := cdbm.NewInstanceDAO(gaih.dbSession)

	dbInstances, total, serr := instanceDAO.GetAll(ctx, nil,
		filter,
		cdbp.PageInput{
			Limit:   pageRequest.Limit,
			Offset:  pageRequest.Offset,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Instances for this Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instances for Site", nil)
	}

	// Make a map for instanceID -> ref Instance
	// for some easy look ups later when we need
	// to compile info for NSGs.
	dbInstancesByID := map[uuid.UUID]*cdbm.Instance{}
	for idx := range dbInstances {
		ins := &dbInstances[idx]
		dbInstancesByID[ins.ID] = ins
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gaih.dbSession)

	sdEntityIDs := []string{}
	insIDs := []uuid.UUID{}
	for _, ins := range dbInstances {
		sdEntityIDs = append(sdEntityIDs, ins.ID.String())
		insIDs = append(insIDs, ins.ID)
	}

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Instances from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Instances", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	ifcDAO := cdbm.NewInterfaceDAO(gaih.dbSession)
	ibifcDAO := cdbm.NewInfiniBandInterfaceDAO(gaih.dbSession)
	nvlDAO := cdbm.NewNVLinkInterfaceDAO(gaih.dbSession)
	skgiaDAO := cdbm.NewSSHKeyGroupInstanceAssociationDAO(gaih.dbSession)

	// Create a map for instanceID -> set of VPCs
	vpcsByInstance := map[uuid.UUID]goset.Set[uuid.UUID]{}

	ifcs, _, serr := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{InstanceIDs: insIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.SubnetRelationName, cdbm.VpcPrefixRelationName})
	if serr != nil {
		// Log error and continue
		logger.Error().Err(serr).Msg("error retrieving Instance Subnets for Instance from DB")
	}

	// We'll need to pull the VPC details for any instances that aren't setting NSG ID
	// to decide if they're inheriting one from their parent VPC, and then figure out
	// their propagation status.  This needs to done separately because we can't assume
	// that the user requested the VPC relation.

	inheritVpcIDs := goset.NewSet[uuid.UUID]()

	ifcMap := map[uuid.UUID][]cdbm.Interface{}
	for _, ifc := range ifcs {
		cifc := ifc
		ifcMap[ifc.InstanceID] = append(ifcMap[ifc.InstanceID], cifc)

		ins := dbInstancesByID[ifc.InstanceID]
		// We're only concerned about VPCs for instances with no
		// NSG attached since they'd inherit from their associated
		// VPCs.
		if ins != nil && ins.NetworkSecurityGroupID == nil {
			if vpcsByInstance[ifc.InstanceID] == nil {
				vpcsByInstance[ifc.InstanceID] = goset.NewSet[uuid.UUID]()
			}

			// Collect the sets of _all_ VPC IDs for the instances so we can use
			// it later for determining NSG propagation.
			if ifc.VpcID != nil {
				vpcsByInstance[ifc.InstanceID].Add(*ifc.VpcID)
				inheritVpcIDs.Add(*ifc.VpcID)
			} else if ifc.VpcPrefix != nil {
				vpcsByInstance[ifc.InstanceID].Add(ifc.VpcPrefix.VpcID)
				inheritVpcIDs.Add(ifc.VpcPrefix.VpcID)
			}

			if ifc.Subnet != nil {
				vpcsByInstance[ifc.InstanceID].Add(ifc.Subnet.VpcID)
				inheritVpcIDs.Add(ifc.Subnet.VpcID)

			}

		}

	}

	// Get the instance infiniband interface record from the db
	ibifcs, _, serr := ibifcDAO.GetAll(
		ctx,
		nil,
		cdbm.InfiniBandInterfaceFilterInput{
			InstanceIDs: insIDs,
			SiteIDs:     siteIDs,
		},
		cdbp.PageInput{
			Limit: cutil.GetPtr(cdbp.TotalLimit),
		},
		[]string{cdbm.InfiniBandPartitionRelationName},
	)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving instance InfiniBand Interfaces Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance InfiniBand Interfaces for Instance", nil)
	}
	ibifcMap := map[uuid.UUID][]cdbm.InfiniBandInterface{}
	for _, ibifc := range ibifcs {
		cibifc := ibifc
		ibifcMap[ibifc.InstanceID] = append(ibifcMap[ibifc.InstanceID], cibifc)
	}

	// Get the instance NVLink Interface record from the db
	retnvlifc, _, serr := nvlDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: insIDs}, cdbp.PageInput{}, nil)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving instance NVLink interfaces Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance NVLink interfaces for Instance", nil)
	}

	// Get the instance NVLink Interface record from the db
	nvlifcMap := map[uuid.UUID][]cdbm.NVLinkInterface{}
	for _, nvlifc := range retnvlifc {
		cnvlifc := nvlifc
		nvlifcMap[nvlifc.InstanceID] = append(nvlifcMap[nvlifc.InstanceID], cnvlifc)
	}

	// Get DPU Extension Service Deployments for all instances
	desdDAO := cdbm.NewDpuExtensionServiceDeploymentDAO(gaih.dbSession)
	desds, _, err := desdDAO.GetAll(
		ctx,
		nil,
		cdbm.DpuExtensionServiceDeploymentFilterInput{
			InstanceIDs: insIDs,
		},
		cdbp.PageInput{
			Limit: cutil.GetPtr(cdbp.TotalLimit),
		},
		[]string{cdbm.DpuExtensionServiceRelationName},
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving DPU Extension Service Deployments for instances from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve DPU Extension Service Deployments for Instances", nil)
	}
	desdsMap := map[uuid.UUID][]cdbm.DpuExtensionServiceDeployment{}
	for _, desd := range desds {
		cdesd := desd
		desdsMap[desd.InstanceID] = append(desdsMap[desd.InstanceID], cdesd)
	}

	// Get SSH Key Group Instance Associations for all Instances
	skgias, _, err := skgiaDAO.GetAll(ctx, nil, cdbm.SSHKeyGroupInstanceAssociationFilterInput{
		SiteIDs:     siteIDs,
		InstanceIDs: insIDs,
	}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, []string{cdbm.SSHKeyGroupRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving ssh key group instance association Details from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SSH Key Group Instance Association for Instance", nil)
	}
	skgiasMap := map[uuid.UUID][]cdbm.SSHKeyGroup{}
	for _, skgia := range skgias {
		cskgia := skgia
		skgiasMap[skgia.InstanceID] = append(skgiasMap[skgia.InstanceID], *cskgia.SSHKeyGroup)
	}

	vpcs := map[uuid.UUID]*cdbm.Vpc{}

	// Only if there's at least one possible case
	// of inheritence.
	if !inheritVpcIDs.IsEmpty() {

		vpcDAO := cdbm.NewVpcDAO(gaih.dbSession)

		vpcFilter := cdbm.VpcFilterInput{
			VpcIDs: inheritVpcIDs.ToSlice(),
		}

		dbVpcs, _, err := vpcDAO.GetAll(ctx, nil, vpcFilter, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving VPCs DB entities")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs for Instances", nil)
		}

		for _, vpc := range dbVpcs {
			vpcs[vpc.ID] = &vpc
		}
	}

	// Advertise the deprecated infrastructureProviderId query param this endpoint still accepts.
	queryParamDeprecations := model.InstanceListQueryParamDeprecations()

	apiInstances := []model.APIInstance{}
	for _, ins := range dbInstances {
		// Create response
		dbInstance := ins
		apiInstance := model.NewAPIInstance(&dbInstance, sitesByID[dbInstance.SiteID], ifcMap[dbInstance.ID], ibifcMap[dbInstance.ID], desdsMap[dbInstance.ID], nvlifcMap[dbInstance.ID], skgiasMap[dbInstance.ID], ssdMap[ins.ID.String()])
		apiInstance.Deprecations = queryParamDeprecations

		// If the instance has no NSG applied directly, and there
		// were ethernet interfaces attached to VPCs (vpcsByInstance),
		// then we'll need to check if we're inheriting from any VPCs
		// and check propagation if so.
		if ins.NetworkSecurityGroupID == nil && vpcsByInstance[ins.ID] != nil {

			vpcCountWithNsgTotal := 0
			vpcCountWithNsgPropagated := 0

			for _, vpcID := range vpcsByInstance[ins.ID].ToSlice() {
				vpc, exists := vpcs[vpcID]

				if exists {
					// If we inherited an NSG from this VPC, then see if we're propagated.
					if vpc.NetworkSecurityGroupID != nil {

						// We found an attached VPC that has an NSG attached,
						// so we'll need to track propagation.
						vpcCountWithNsgTotal++

						if vpc.NetworkSecurityGroupPropagationDetails != nil {
							// If the instance wasn't found in the list of unpropagated instances, then we're propagated.
							if !slices.Contains(vpc.NetworkSecurityGroupPropagationDetails.GetUnpropagatedInstanceIds(), ins.ID.String()) {
								// We're propagated, so count it.
								vpcCountWithNsgPropagated++
							}
						}
					}

				}

			}

			if vpcCountWithNsgTotal > 0 {
				// We've only inherited if our NSG ID is null _and_ the parent
				// NSG ID is not null. I.e., at least one VPC is using an NSG.
				apiInstance.NetworkSecurityGroupInherited = true

				// NOTE: We could track the specific list of VPCs still waiting for us to synchronize,
				//       but that information can also be derived by looking at VPC details to see
				//       which instances are pending, so we mayb not need it here.

				switch {
				case vpcCountWithNsgPropagated == 0:
					apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
						ObjectID:       ins.ID.String(),
						DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusNone,
						Status:         model.APINetworkSecurityGroupPropagationStatusSynchronizing,
					}
				case vpcCountWithNsgPropagated < vpcCountWithNsgTotal:
					apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
						ObjectID:       ins.ID.String(),
						DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusPartial,
						Status:         model.APINetworkSecurityGroupPropagationStatusSynchronizing,
					}
				case vpcCountWithNsgPropagated == vpcCountWithNsgTotal:
					apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
						ObjectID:       ins.ID.String(),
						DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusFull,
						Status:         model.APINetworkSecurityGroupPropagationStatusSynchronized,
					}
				default:
					apiInstance.NetworkSecurityGroupPropagationDetails = &model.APINetworkSecurityGroupPropagationDetails{
						ObjectID:       ins.ID.String(),
						DetailedStatus: model.APINetworkSecurityGroupPropagationDetailedStatusUnknown,
						Status:         model.APINetworkSecurityGroupPropagationStatusError,
					}
				}
			}

		}
		apiInstances = append(apiInstances, *apiInstance)
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiInstances)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteInstanceHandler is the API Handler for deleting an Instance
type DeleteInstanceHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// NewDeleteInstanceHandler initializes and r`eturns a new handler for deleting an Instance
func NewDeleteInstanceHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) DeleteInstanceHandler {
	return DeleteInstanceHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		dps:        dps,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an Instance
// @Description Delete an Instance fro the org
// @Tags Instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Instance"
// @Success 202
// @Router /v2/org/{org}/nico/instance/{id} [delete]
func (dih DeleteInstanceHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Instance", "Delete", c, dih.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
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

	// Validate role, only Tenant Admins are allowed to delete Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get Instance ID from URL param
	instanceStrID := c.Param("id")
	instanceID, err := uuid.Parse(instanceStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance ID in URL", nil)
	}

	dih.tracerSpan.SetAttribute(handlerSpan, attribute.String("instance_id", instanceStrID), logger)

	// Get Instance
	instanceDAO := cdbm.NewInstanceDAO(dih.dbSession)

	instance, err := instanceDAO.GetByID(ctx, nil, instanceID, []string{cdbm.SiteRelationName, cdbm.TenantRelationName, cdbm.VpcRelationName})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Instance with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance", nil)
	}

	if instance.Tenant == nil {
		logger.Error().Err(err).Msg("error retrieving Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant details", nil)
	}

	// Confirm that the Instance org (via the Tenant org)
	// matches the org sent in the request.
	if instance.Tenant.Org != org {
		logger.Error().Msg("org specified in request does not match org of Tenant associated with Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org specified in request does not match Org of Tenant associated with Instance", nil)
	}

	// Verify that the instance is associated with a site and then that the site is
	// in a valid state.
	if instance.Site == nil {
		logger.Error().Msg("failed to pull site data for Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Instance", nil)
	}

	if instance.Site.Status != cdbm.SiteStatusRegistered {
		logger.Error().Msg("site not in registered state - cannot delete Instance")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Site is not in Registered state - cannot delete Instance", nil)
	}

	// Bind request data to API model
	apiRequest := model.APIInstanceDeleteRequest{}
	if c.Request().Body != http.NoBody {
		if err := c.Bind(&apiRequest); err != nil {
			logger.Warn().Err(err).Msg("error binding request data into API model")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
		}
	}

	// Validate request attributes
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating Instance deletion request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Instance deletion request data", verr)
	}

	// Authorization stays in the handler: setting `IsRepairTenant` requires the
	// tenant to carry the TargetedInstanceCreation capability. Validate before
	// opening the transaction so no writes or locks happen for an unauthorized
	// request. By the time `ToProto` runs the request is safe to trust.
	if apiRequest.IsRepairTenant != nil && *apiRequest.IsRepairTenant {
		enabledForSite, derr := common.TenantHasTargetedInstanceCreation(ctx, nil, dih.dbSession, instance.Tenant, &common.TenantPrivilegeScope{
			SiteID: &instance.SiteID,
		})
		if derr != nil {
			logger.Error().Err(derr).Msg("error checking effective targeted instance creation for Instance's Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify capability for Instance's Site", nil)
		}
		if !enabledForSite {
			logger.Warn().Msg("tenant does not have capability to set IsRepairTenant for the Instance's Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have capability to set IsRepairTenant", nil)
		}
	}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dih.dbSession, func(tx *cdb.Tx) error {
		if dih.cfg.GetDPSEnabled() && instance.Vpc != nil {
			lockErr := powerutil.AcquireVPCPowerLock(ctx, tx, instance.Vpc.ID)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", instance.Vpc.ID.String()).Msg("failed to serialize DPS operations for VPC")
				return cutil.NewAPIError(http.StatusConflict, "Another power operation is already in progress for the VPC", nil)
			}
			lockedVPC, lockErr := cdbm.NewVpcDAO(dih.dbSession).GetByID(ctx, tx, instance.Vpc.ID, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", instance.Vpc.ID.String()).Msg("failed to reload VPC after acquiring its power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload VPC power configuration", nil)
			}
			instance.Vpc = lockedVPC
		}

		// Update Instance to set status to Deleting
		_, derr := instanceDAO.Update(ctx, tx, cdbm.InstanceUpdateInput{InstanceID: instance.ID, InstanceUpdateCommonInput: cdbm.InstanceUpdateCommonInput{Status: cutil.GetPtr(cdbm.InstanceStatusTerminating)}})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Instance in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Instance", nil)
		}

		// Create status detail
		sdDAO := cdbm.NewStatusDetailDAO(dih.dbSession)
		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: instance.ID.String(), Status: *cutil.GetPtr(cdbm.InstanceStatusTerminating), Message: cutil.GetPtr("Instance deletion successfully initiated on Site")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dih.scp.GetClientByID(instance.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Prepare the delete/release request workflow object
		releaseInstanceRequest := apiRequest.ToProto(instance, dbUser)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "instance-delete-" + instance.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering instance delete workflow")

		// Add context deadline.
		// The client (NGC or its downstream client) could cancel the parent deadline
		// at any time by closing the connection, HTTP2 reset, etc.  So, the real
		// deadline could be shorter than WorkflowContextTimeout.  We're only
		// enforcing an upper limit here.
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow to update instance
		// TODO: Once Site Agent offers DeleteInstanceV2 re-registered as DeleteInstance then update workflow name here
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteInstanceV2", releaseInstanceRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to delete Instance")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to delete Instance on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete Instance workflow")

		// Execute the workflow synchronously
		wferr = we.Get(wfCtx, nil)

		// Handle skippable errors
		if wferr != nil {
			// If this was a 404 back from NICo, we can treat the object as already having been deleted and allow things to proceed.
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.ObjectNotFoundErrTypes(), applicationErr.Type()) {
				logger.Warn().Msg(swe.ErrTypeNICoObjectNotFound + " received from Site")
				// Reset error to nil
				wferr = nil
			}
		}

		// Check if err is still nil now that we've handled any skippable errors.
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to delete Instance, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "Instance", "DeleteInstanceV2")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Instance delete workflow timed out", nil)
			}

			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to delete Instance")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete Instance on Site: %s", unwrapped), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete Instance workflow")

		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete Instance, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}
	if dih.cfg.GetDPSEnabled() && dih.dps != nil && instance.Vpc != nil && instance.Vpc.PowerResourceGroup != nil && instance.MachineID != nil {
		cleanupErr := dih.dps.RemoveMachine(context.WithoutCancel(ctx), *instance.Vpc.PowerResourceGroup, *instance.MachineID)
		if cleanupErr != nil {
			logger.Error().Err(cleanupErr).Str("machineID", *instance.MachineID).Str("powerResourceGroup", *instance.Vpc.PowerResourceGroup).Msg("failed to remove released machine from DPS; external reconciliation is required")
		}
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}

// GetInstanceStatusDetailsHandler is the API Handler for getting Instance StatusDetail records
type GetInstanceStatusDetailsHandler struct {
	dbSession  *cdb.Session
	tracerSpan *cutil.TracerSpan
}

// NewGetInstanceStatusDetailsHandler initializes and returns a new handler to retrieve Instance StatusDetail records
func NewGetInstanceStatusDetailsHandler(dbSession *cdb.Session) GetInstanceStatusDetailsHandler {
	return GetInstanceStatusDetailsHandler{
		dbSession:  dbSession,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get Instance StatusDetails
// @Description Get all StatusDetails for Instance
// @Tags instance
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Instance"
// @Success 200 {object} []model.APIStatusDetail
// @Router /v2/org/{org}/nico/instance/{id}/status-history [get]
func (gisdh GetInstanceStatusDetailsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Instance", "Get", c, gisdh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
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

	// Validate role, only Tenant Admins are allowed to retrieve Instances
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InstanceRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get Instance ID from URL param
	instanceStrID := c.Param("id")
	instanceID, err := uuid.Parse(instanceStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Instance ID in URL", nil)
	}

	gisdh.tracerSpan.SetAttribute(handlerSpan, attribute.String("instance_id", instanceStrID), logger)

	// Get Instance
	instanceDAO := cdbm.NewInstanceDAO(gisdh.dbSession)
	instance, err := instanceDAO.GetByID(ctx, nil, instanceID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Instance with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Instance from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instance", nil)
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gisdh.dbSession)
	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]

	// Check if Instance belongs to Tenant
	if instance.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Instance does not belong to current Tenant", nil)
	}

	// handle retrieving and building status details response
	apiSds, err := handleEntityStatusDetails(ctx, c, gisdh.dbSession, instanceID.String(), logger)
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiSds)
}
