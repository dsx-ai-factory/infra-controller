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

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/labstack/echo/v4"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

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

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateVPCHandler is the API Handler for creating new VPC
type CreateVPCHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// NewCreateVPCHandler initializes and returns a new handler for creating Tenant
func NewCreateVPCHandler(dbSession *cdb.Session, tc temporalClient.Client, sc *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) CreateVPCHandler {
	return CreateVPCHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        sc,
		cfg:        cfg,
		dps:        dps,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a VPC
// @Description Create a VPC for the org.
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIVpcCreateRequest true "VPC create request"
// @Success 201 {object} model.APIVpc
// @Router /v2/org/{org}/nico/vpc [post]
func (cvh CreateVPCHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC", "Create", c, cvh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIVpcCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC creation request data", verr)
	}

	// Validate the site for which this VPC is being created
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cvh.dbSession)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site with ID specified in request data", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in request data", nil)
	}

	// Verify if site is ready
	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("Site: %v specified in request data must be in Registered state in order to proceed", site.ID))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data must be in Registered state in order to proceed", nil)
	}
	// Get Tenant for this org
	tenant, err := common.GetTenantForOrg(ctx, nil, cvh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	// Ensure that Tenant has an Allocation with specified Site
	aDAO := cdbm.NewAllocationDAO(cvh.dbSession)
	allocationFilter := cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{site.ID}}
	aCount, err := aDAO.GetCount(ctx, nil, allocationFilter)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocations count from DB for Tenant and Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site Allocations count for Tenant", nil)
	}

	if aCount == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
			"Tenant does not have any Allocations with Site specified in request data", nil)
	}

	vpcDAO := cdbm.NewVpcDAO(cvh.dbSession)
	if apiRequest.ID != nil {
		_, total, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{VpcIDs: []uuid.UUID{*apiRequest.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(paginator.DefaultLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("db error checking for ID uniqueness of tenant vpc")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Vpc due to DB error", nil)
		}
		if total > 0 {
			logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.ID.String()).Msg("vpc with same ID already exists")
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "A Vpc with specified ID already exists", validation.Errors{
				"id": errors.New(apiRequest.ID.String()),
			})
		}
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another vpc with same name at the site
	// TODO consider doing this with an advisory lock for correctness
	vpcs, tot, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{Name: &apiRequest.Name, InfrastructureProviderID: cutil.GetPtr(site.InfrastructureProviderID), TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant vpc")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Vpc due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.Name).Msg("vpc with same name already exists for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "A Vpc with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(vpcs[0].ID.String()),
		})
	}

	// If an NSG was requested, validate it.
	if apiRequest.NetworkSecurityGroupID != nil {
		nsgDAO := cdbm.NewNetworkSecurityGroupDAO(cvh.dbSession)

		nsg, err := nsgDAO.GetByID(ctx, nil, *apiRequest.NetworkSecurityGroupID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				logger.Error().Err(err).Msg("could not find NetworkSecurityGroup with ID specified in request data")
				// Should probably be using StatusPreconditionFailed here, and maybe for all of these NSG errors.
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
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}
	if apiErr := util.ValidateSitePowerManagement(siteConfig, apiRequest.PowerResourceGroup); apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	// Network Virtualization type support
	networkVirtualizationType := apiRequest.NetworkVirtualizationType
	if networkVirtualizationType == nil {
		// Default to `EthernetVirtualizer`
		networkVirtualizationType = cutil.GetPtr(cdbm.VpcEthernetVirtualizer)

		// If site has native networking enabled, use FNN
		if siteConfig.NativeNetworking {
			networkVirtualizationType = cutil.GetPtr(cdbm.VpcFNN)
		}
	}

	slaacEnabled := apiRequest.SlaacEnabled != nil && *apiRequest.SlaacEnabled
	// Verify if site has been enabled for FNN type
	if *networkVirtualizationType == cdbm.VpcFNN {
		if !siteConfig.NativeNetworking {
			logger.Warn().Msg(fmt.Sprintf("Site: %v specified in request data must have native networking enabled in order to create FNN VPCs", site.ID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data must have native networking enabled in order to create FNN VPCs", nil)
		}
	}
	if slaacEnabled && *networkVirtualizationType != cdbm.VpcFNN {
		logger.Warn().Str("networkVirtualizationType", *networkVirtualizationType).Msg("SLAAC is not supported for network virtualization type")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "`slaacEnabled` is only supported when network virtualization type is `FNN`", nil)
	}
	if slaacEnabled && !siteConfig.VpcSlaac {
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not advertise support for SLAAC-enabled VPCs", nil)
	}

	// TargetedInstanceCreation supplies both policies, but keep write authorization
	// separate from effective-profile response visibility.
	tenantCanSetRoutingProfile := false
	tenantCanViewEffectiveRoutingProfile := false
	if cdbm.VpcTypeSupportsRoutingProfile(networkVirtualizationType) || apiRequest.RoutingProfile != nil {
		tenantHasTargetedInstanceCreation, err := common.TenantHasTargetedInstanceCreation(ctx, nil, cvh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
		if err != nil {
			logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to verify privileges for Site", nil)
		}
		tenantCanSetRoutingProfile = tenantHasTargetedInstanceCreation
		tenantCanViewEffectiveRoutingProfile = tenantHasTargetedInstanceCreation
	}

	if apiRequest.RoutingProfileOverrides != nil && !cdbm.VpcTypeSupportsRoutingProfile(networkVirtualizationType) {
		logger.Warn().Str("networkVirtualizationType", *networkVirtualizationType).Msg("routing profile overrides are not supported for network virtualization type")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Routing profile overrides are not supported for network virtualization type: %s", *networkVirtualizationType), nil)
	}

	if apiRequest.RoutingProfileOverrides != nil && !tenantCanSetRoutingProfile {
		logger.Warn().Msg("tenant does not have sufficient privileges to set `routingProfileOverrides`")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have sufficient privileges to set `routingProfileOverrides`", nil)
	}

	var routingProfile *string
	if apiRequest.RoutingProfile != nil {
		// Routing-profile writes require TargetedInstanceCreation for the VPC's Site.
		if !tenantCanSetRoutingProfile {
			logger.Warn().Msg("tenant does not have sufficient privileges to set `routingProfile`")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have sufficient privileges to set `routingProfile`", nil)
		}

		// The request-struct-only case (both RoutingProfile and
		// NetworkVirtualizationType supplied) is caught by Validate.
		// The implicit case below covers callers that supplied
		// `routingProfile` only and let the handler default
		// `networkVirtualizationType` from site config; Validate
		// cannot see that default, so the check lives here.
		if !cdbm.VpcTypeSupportsRoutingProfile(networkVirtualizationType) {
			logger.Warn().Str("routingProfile", *apiRequest.RoutingProfile).Msg("`routingProfile` can only be specified if network virtualization type is set to `FNN`, or Site has native networking enabled and no network virtualization type is specified")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "`routingProfile` can only be specified if network virtualization type is set to `FNN`, or Site has native networking enabled and no network virtualization type is specified", nil)
		}

		// Normalize the API routing profile before sending it to the site controller.
		routingProfile = cutil.GetPtr(model.NormalizeAPIVpcRoutingProfileForSite(*apiRequest.RoutingProfile))
	}

	var defaultNvllPartitionId *uuid.UUID
	if apiRequest.NVLinkLogicalPartitionID != nil {
		if !siteConfig.NVLinkPartition {
			logger.Warn().Msg(fmt.Sprintf("Site: %v specified in request data must have NVLink Partition enabled in order to create VPC with default NVLink Partition", site.ID))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data must have NVLink Partition enabled in order to create VPC with default NVLink Partition", nil)
		}

		nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(cvh.dbSession)
		nvllpID, err := uuid.Parse(*apiRequest.NVLinkLogicalPartitionID)
		if err != nil {
			logger.Error().Err(err).Msg("error parsing NVLink Logical Partition ID specified in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid NVLink Logical Partition ID specified in request data", nil)
		}

		nvllPartition, err := nvllpDAO.GetByID(ctx, nil, nvllpID, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLink Logical Partition with ID specified in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partition with ID specified in request data", nil)
		}

		if nvllPartition.SiteID != site.ID {
			logger.Error().Msg("NVLink Logical Partition in request does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition with ID specified in request data does not belong to Site", nil)
		}

		if nvllPartition.Status != cdbm.NVLinkLogicalPartitionStatusReady {
			logger.Error().Msg("NVLink Logical Partition in request is not in Ready state")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition with ID specified in request data is not in Ready state", nil)
		}

		// Verify that the NVLink Logical Partition is associated with the VPC's Tenant
		if nvllPartition.TenantID != tenant.ID {
			logger.Error().Msg("NVLink Logical Partition in request does not belong to Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition with ID specified in request data does not belong to Tenant", nil)
		}
		defaultNvllPartitionId = &nvllpID
	}

	// Labels support
	var labels map[string]string
	if apiRequest.Labels != nil {
		labels = apiRequest.Labels
	}

	dpsGroupCreated := false
	if cvh.cfg.GetDPSEnabled() && apiRequest.PowerResourceGroup != nil {
		if cvh.dps == nil {
			return cutil.NewAPIErrorResponse(c, http.StatusServiceUnavailable, "DPS power provisioning is unavailable", nil)
		}
		if apiRequest.ID == nil {
			apiRequest.ID = cutil.GetPtr(uuid.New())
		}
		externalID, derr := dpsclient.ExternalIDFromVPCID(apiRequest.ID.String())
		if derr != nil {
			logger.Error().Err(derr).Str("vpcID", apiRequest.ID.String()).Msg("failed to derive DPS resource group ID")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to derive DPS resource group ID", nil)
		}
		derr = cvh.dps.CreateResourceGroup(ctx, *apiRequest.PowerResourceGroup, externalID)
		if derr != nil {
			logger.Error().Err(derr).Str("powerResourceGroup", *apiRequest.PowerResourceGroup).Msg("failed to create DPS resource group")
			apiErr := powerResourceGroupAPIError(derr, "Failed to create DPS resource group")
			return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
		}
		dpsGroupCreated = true
	}

	sdDAO := cdbm.NewStatusDetailDAO(cvh.dbSession)

	var vpc *cdbm.Vpc
	var ssd *cdbm.StatusDetail
	controllerVpc := &corev1.Vpc{}
	controllerVpcModel := &cdbm.Vpc{}

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, cvh.dbSession, func(tx *cdb.Tx) error {
		// Create VPC
		vpcInput := cdbm.VpcCreateInput{
			ID:                        apiRequest.ID,
			Name:                      apiRequest.Name,
			Description:               apiRequest.Description,
			Org:                       org,
			InfrastructureProviderID:  site.InfrastructureProviderID,
			NetworkSecurityGroupID:    apiRequest.NetworkSecurityGroupID,
			TenantID:                  tenant.ID,
			SiteID:                    site.ID,
			NetworkVirtualizationType: networkVirtualizationType,
			SlaacEnabled:              slaacEnabled,
			RoutingProfile:            routingProfile,
			PowerResourceGroup:        apiRequest.PowerResourceGroup,
			RoutingProfileOverrides:   apiRequest.RoutingProfileOverrides.ToDB(),
			NVLinkLogicalPartitionID:  defaultNvllPartitionId,
			Labels:                    labels,
			Status:                    cdbm.VpcStatusProvisioning,
			CreatedBy:                 *dbUser,
			Vni:                       apiRequest.Vni,
		}

		createdVpc, derr := vpcDAO.Create(ctx, tx, vpcInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating VPC DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating new VPC record, DB error", nil)
		}

		// Update the controller ID
		// We need this to match the VPC ID.  This was previously handled
		// by the async cloud workflow after successful creation on site.
		uvpcInput := cdbm.VpcUpdateInput{
			VpcID:           createdVpc.ID,
			ControllerVpcID: cutil.GetPtr(createdVpc.ID),
		}
		updatedVpc, derr := vpcDAO.Update(ctx, tx, uvpcInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating VPC DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed updating new VPC record, DB error", nil)
		}
		vpc = updatedVpc

		// Create status detail
		createdSsd, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpc.ID.String(), Status: cdbm.VpcStatusProvisioning, Message: cutil.GetPtr("VPC provisioning has been initiated on Site")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for VPC", nil)
		}
		if createdSsd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for VPC", nil)
		}
		ssd = createdSsd

		stc, clientErr := cvh.scp.GetClientByID(vpc.SiteID)
		if clientErr != nil {
			logger.Error().Err(clientErr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		createVpcRequest := apiRequest.ToProto(vpc)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "vpc-create-" + vpc.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering VPC create workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateVPCV2", createVpcRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to create VPC")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to create VPC on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create VPC workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, controllerVpc)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to create VPC, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPC", "Create")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC create workflow timed out", nil)
			}

			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to create VPC")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create VPC on Site: %s", unwrapped), nil)
		}

		controllerVpcModel.FromProto(controllerVpc)
		if controllerVpcModel.RoutingProfile != nil {
			updatedVpc, derr := vpcDAO.Update(ctx, tx, cdbm.VpcUpdateInput{
				VpcID:          vpc.ID,
				RoutingProfile: controllerVpcModel.RoutingProfile,
			})
			if derr != nil {
				logger.Error().Err(derr).Msg("error persisting Core-resolved VPC routing profile")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to persist Core-resolved VPC routing profile", nil)
			}
			vpc = updatedVpc
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create VPC workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			if dpsGroupCreated {
				cleanupErr := cvh.dps.DeleteResourceGroup(context.WithoutCancel(ctx), *apiRequest.PowerResourceGroup)
				if cleanupErr != nil {
					logger.Error().Err(cleanupErr).Str("powerResourceGroup", *apiRequest.PowerResourceGroup).Msg("failed to compensate DPS resource group after VPC creation failure")
				}
			}
			return common.HandleTxError(c, logger, err, "Failed to create VPC due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		responseErr := timeoutResp()
		if dpsGroupCreated {
			cleanupErr := cvh.dps.DeleteResourceGroup(context.WithoutCancel(ctx), *apiRequest.PowerResourceGroup)
			if cleanupErr != nil {
				logger.Error().Err(cleanupErr).Str("powerResourceGroup", *apiRequest.PowerResourceGroup).Msg("failed to compensate DPS resource group after VPC creation failure")
			}
		}
		return responseErr
	}

	statusDetails := []cdbm.StatusDetail{*ssd}

	// Make a best-effort attempt to cache the remaining controller-reported
	// state for the response. The resolved routing profile was committed above.
	activeVni := controllerVpcModel.ActiveVni
	effectiveRoutingProfile := controllerVpcModel.EffectiveRoutingProfile
	if activeVni != nil || effectiveRoutingProfile != nil {
		uvpcInput := cdbm.VpcUpdateInput{
			VpcID:                   vpc.ID,
			ActiveVni:               activeVni,
			EffectiveRoutingProfile: effectiveRoutingProfile,
		}
		if activeVni != nil {
			uvpcInput.Status = cutil.GetPtr(cdbm.VpcStatusReady)
		}
		updatedVpc, err := vpcDAO.Update(ctx, nil, uvpcInput)
		if err != nil {
			logger.Error().Err(err).Msg("error while caching controller-reported VPC VNI and effective routing profile")
		} else {
			// Update the vpc being returned if all went well.
			vpc = updatedVpc

			if activeVni != nil {
				// Best effort create status detail
				ssd, err = sdDAO.Create(ctx, nil, cdbm.StatusDetailCreateInput{EntityID: vpc.ID.String(), Status: cdbm.VpcStatusReady, Message: cutil.GetPtr("VPC is ready for use")})
				if err != nil {
					logger.Error().Err(err).Msg("error creating Status Detail DB entry")
				} else if ssd == nil {
					logger.Error().Err(err).Msg("unexpected nil Status Detail returned from DB")
				} else {
					statusDetails = append(statusDetails, *ssd)
				}
			}
		}
	}

	// Create response
	apiVpc := model.NewAPIVpc(*vpc, statusDetails, tenantCanViewEffectiveRoutingProfile)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusCreated, apiVpc)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateVPCHandler is the API Handler for updating a VPC
type UpdateVPCHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// NewUpdateVPCHandler initializes and returns a new handler for updating VPC
func NewUpdateVPCHandler(dbSession *cdb.Session, tc temporalClient.Client, sc *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) UpdateVPCHandler {
	return UpdateVPCHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        sc,
		cfg:        cfg,
		dps:        dps,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing VPC
// @Description Update an existing VPC for the org
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Vpc"
// @Param message body model.APIVpcUpdateRequest true "VPC update request"
// @Success 200 {object} model.APIVpc
// @Router /v2/org/{org}/nico/vpc/{id} [patch]
func (uvh UpdateVPCHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC", "Update", c, uvh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get vpc instance ID from URL param
	vpcStrID := c.Param("id")

	uvh.tracerSpan.SetAttribute(handlerSpan, attribute.String("vpc_id", vpcStrID), logger)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIVpcUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC update request data", verr)
	}

	// Check that VPC exists
	vpc, err := common.GetVpcFromIDString(ctx, nil, vpcStrID, []string{cdbm.SiteRelationName}, uvh.dbSession)
	if err != nil {
		// Check if it's a UUID parsing error (happens before DB call)
		if err == common.ErrInvalidID {
			logger.Warn().Err(err).Msg("invalid VPC ID in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC ID in request", nil)
		}
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("VPC not found")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve VPC to update", nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC", nil)
	}
	// Get Tenant for this org
	tenant, err := common.GetTenantForOrg(ctx, nil, uvh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	// Check that VPC belongs to the Tenant
	if vpc.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "VPC does not belong to current Tenant", nil)
	}

	// TargetedInstanceCreation supplies both policies, but keep write authorization
	// separate from effective-profile response visibility.
	tenantCanSetRoutingProfile := false
	tenantCanViewEffectiveRoutingProfile := false
	if cdbm.VpcTypeSupportsRoutingProfile(vpc.NetworkVirtualizationType) {
		tenantHasTargetedInstanceCreation, err := common.TenantHasTargetedInstanceCreation(ctx, nil, uvh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &vpc.SiteID})
		if err != nil {
			logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
		}
		tenantCanSetRoutingProfile = tenantHasTargetedInstanceCreation
		tenantCanViewEffectiveRoutingProfile = tenantHasTargetedInstanceCreation
	}

	if apiRequest.RoutingProfileOverrides != nil && !cdbm.VpcTypeSupportsRoutingProfile(vpc.NetworkVirtualizationType) {
		networkVirtualizationType := "<unspecified>"
		if vpc.NetworkVirtualizationType != nil {
			networkVirtualizationType = *vpc.NetworkVirtualizationType
		}
		logger.Warn().Str("networkVirtualizationType", networkVirtualizationType).Msg("routing profile overrides are not supported for network virtualization type")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Routing profile overrides are not supported for network virtualization type: %s", networkVirtualizationType), nil)
	}

	if apiRequest.RoutingProfileOverrides != nil && !tenantCanSetRoutingProfile {
		logger.Warn().Msg("tenant does not have sufficient privileges to set `routingProfileOverrides`")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have sufficient privileges to set `routingProfileOverrides`", nil)
	}

	// Ensure that Tenant has an Allocation with specified Site
	aDAO := cdbm.NewAllocationDAO(uvh.dbSession)
	allocationFilter := cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{vpc.SiteID}}
	aCount, err := aDAO.GetCount(ctx, nil, allocationFilter)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Allocations count from DB for Tenant and Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site Allocations count for Tenant", nil)
	}

	if aCount == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
			"Tenant does not have any Allocations with Site specified in request data", nil)
	}

	vpcDAO := cdbm.NewVpcDAO(uvh.dbSession)
	// check for name uniqueness for the tenant, ie, tenant cannot have another vpc with same name at the site
	if apiRequest.Name != nil && *apiRequest.Name != vpc.Name {
		vpcs, tot, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{Name: apiRequest.Name, InfrastructureProviderID: &vpc.InfrastructureProviderID, TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{vpc.SiteID}}, cdbp.PageInput{}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant vpc")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Vpc due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another VPC with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(vpcs[0].ID.String()),
			})
		}
	}

	var nsgID *string
	// If an NSG was requested, validate it.
	if apiRequest.NetworkSecurityGroupID != nil && *apiRequest.NetworkSecurityGroupID != "" {
		nsgDAO := cdbm.NewNetworkSecurityGroupDAO(uvh.dbSession)

		nsg, err := nsgDAO.GetByID(ctx, nil, *apiRequest.NetworkSecurityGroupID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				logger.Error().Err(err).Msg("could not find NetworkSecurityGroup with ID specified in request data")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find NetworkSecurityGroup with ID specified in request data", nil)
			}

			logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroup with ID specified in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NetworkSecurityGroup with ID specified in request data", nil)
		}

		if nsg.SiteID != vpc.SiteID {
			logger.Error().Msg("NetworkSecurityGroup in request does not belong to Site")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NetworkSecurityGroup with ID specified in request data does not belong to Site", nil)
		}

		if nsg.TenantID != tenant.ID {
			logger.Error().Msg("NetworkSecurityGroup in request does not belong to Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NetworkSecurityGroup with ID specified in request data does not belong to Tenant", nil)
		}

		nsgID = cutil.GetPtr(nsg.ID)
	}

	// Labels support
	var labels map[string]string
	if apiRequest.Labels != nil {
		labels = apiRequest.Labels
	}

	siteConfig := &cdbm.SiteConfig{}
	if vpc.Site != nil && vpc.Site.Config != nil {
		siteConfig = vpc.Site.Config
	}
	if apiErr := util.ValidateSitePowerManagement(siteConfig, apiRequest.PowerResourceGroup); apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}

	var defaultNvllPartitionId *uuid.UUID
	if apiRequest.NVLinkLogicalPartitionID != nil {
		if !siteConfig.NVLinkPartition {
			logger.Warn().Msg(fmt.Sprintf("Site: %v specified in request data must have NVLink Partition enabled in order to update VPC with default NVLink Partition", vpc.SiteID.String()))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Site: %v specified in request data must have NVLink Partition enabled in order to update VPC with default NVLink Partition", vpc.SiteID.String()), nil)
		}

		// Verify that the existing default NVLink Logical Partition is not being used by any Instance from the VPC
		instanceDAO := cdbm.NewInstanceDAO(uvh.dbSession)
		instances, _, err := instanceDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Instances from DB for VPC")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instances for VPC", nil)
		}

		nvlIfcDAO := cdbm.NewNVLinkInterfaceDAO(uvh.dbSession)
		for _, instance := range instances {
			// Get NVLink Interfaces for the Instance
			nvlIfcs, _, err := nvlIfcDAO.GetAll(ctx, nil, cdbm.NVLinkInterfaceFilterInput{InstanceIDs: []uuid.UUID{instance.ID}}, cdbp.PageInput{}, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving NVLink Interfaces from DB for Instance")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Interfaces for Instance", nil)
			}

			for _, nvlIfc := range nvlIfcs {
				if nvlIfc.NVLinkLogicalPartitionID == *vpc.NVLinkLogicalPartitionID {
					logger.Error().Msg("Existing default NVLink Logical Partition is already being used by an Instance from the VPC")
					return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Existing default NVLink Logical Partition is already being used by an Instance from the VPC", nil)
				}
			}
		}

		// If a new NVLink Logical Partition ID is specified, validate it.
		// if it is empty then user wants
		if *apiRequest.NVLinkLogicalPartitionID != "" {
			nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(uvh.dbSession)
			nvllpID, err := uuid.Parse(*apiRequest.NVLinkLogicalPartitionID)
			if err != nil {
				logger.Error().Err(err).Msg("error parsing NVLink Logical Partition ID specified in request data")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid NVLink Logical Partition ID specified in request data", nil)
			}

			nvllPartition, err := nvllpDAO.GetByID(ctx, nil, nvllpID, nil)
			if err != nil {
				logger.Error().Err(err).Msg("error retrieving NVLink Logical Partition with ID specified in request data")
				return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partition with ID specified in request data", nil)
			}

			// Verify that the NVLink Logical Partition is associated with the VPC's Site
			if nvllPartition.SiteID != vpc.SiteID {
				logger.Error().Msg("NVLink Logical Partition in request does not belong to Site")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition with ID specified in request data does not belong to Site", nil)
			}

			// Verify that the NVLink Logical Partition is in the Ready state
			if nvllPartition.Status != cdbm.NVLinkLogicalPartitionStatusReady {
				logger.Error().Msg("NVLink Logical Partition in request is not in Ready state")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition with ID specified in request data is not in Ready state", nil)
			}

			// Verify that the NVLink Logical Partition is associated with the VPC's Tenant
			if nvllPartition.TenantID != tenant.ID {
				logger.Error().Msg("NVLink Logical Partition in request does not belong to Tenant")
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "NVLink Logical Partition with ID specified in request data does not belong to Tenant", nil)
			}

			defaultNvllPartitionId = &nvllpID
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(uvh.dbSession)
	var ssds []cdbm.StatusDetail

	var dpsChange *powerutil.PowerChange
	var dpsOldGroup, dpsNewGroup string

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, uvh.dbSession, func(tx *cdb.Tx) error {
		if uvh.cfg.GetDPSEnabled() && apiRequest.PowerResourceGroup != nil {
			lockErr := powerutil.AcquireVPCPowerLock(ctx, tx, vpc.ID)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to serialize DPS operations for VPC")
				return cutil.NewAPIError(http.StatusConflict, "Another power operation is already in progress for the VPC", nil)
			}
			lockedVPC, lockErr := vpcDAO.GetByID(ctx, tx, vpc.ID, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to reload VPC after acquiring its power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload VPC power configuration", nil)
			}
			vpc = lockedVPC

			if vpc.PowerResourceGroup != nil {
				dpsOldGroup = *vpc.PowerResourceGroup
			}
			dpsNewGroup = *apiRequest.PowerResourceGroup
			if strings.TrimSpace(dpsOldGroup) != strings.TrimSpace(dpsNewGroup) {
				instances, _, derr := cdbm.NewInstanceDAO(uvh.dbSession).GetAll(ctx, tx, cdbm.InstanceFilterInput{VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
				if derr != nil {
					logger.Error().Err(derr).Str("vpcID", vpc.ID.String()).Msg("failed to retrieve VPC instances for DPS migration")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve VPC instances for DPS migration", nil)
				}
				assignments := make([]powerutil.MachinePowerAssignment, 0, len(instances))
				for _, instance := range instances {
					if instance.MachineID == nil {
						return cutil.NewAPIError(http.StatusConflict, "Cannot change the power resource group while a VPC instance has no machine", nil)
					}
					assignment := powerutil.MachinePowerAssignment{MachineID: *instance.MachineID}
					if instance.PowerProfile != nil {
						assignment.PowerProfile = *instance.PowerProfile
					}
					assignments = append(assignments, assignment)
				}
				externalID, derr := dpsclient.ExternalIDFromVPCID(vpc.ID.String())
				if derr != nil {
					logger.Error().Err(derr).Str("vpcID", vpc.ID.String()).Msg("failed to derive DPS resource group ID")
					return cutil.NewAPIError(http.StatusInternalServerError, "Failed to derive DPS resource group ID", nil)
				}
				dpsChange, derr = powerutil.PreparePowerResourceGroupChange(ctx, uvh.dps, externalID, dpsOldGroup, dpsNewGroup, assignments)
				if derr != nil {
					logger.Error().Err(derr).Str("oldPowerResourceGroup", dpsOldGroup).Str("newPowerResourceGroup", dpsNewGroup).Msg("failed to prepare DPS resource group migration")
					return powerResourceGroupAPIError(derr, "Failed to change DPS resource group")
				}
			}
		}

		// Update VPC
		uvpcInput := cdbm.VpcUpdateInput{
			VpcID:                   vpc.ID,
			Name:                    apiRequest.Name,
			Description:             apiRequest.Description,
			Labels:                  labels,
			NetworkSecurityGroupID:  nsgID,
			RoutingProfileOverrides: apiRequest.RoutingProfileOverrides.ToDB(),
			PowerResourceGroup:      apiRequest.PowerResourceGroup,
		}

		if defaultNvllPartitionId != nil {
			uvpcInput.NVLinkLogicalPartitionID = defaultNvllPartitionId
		}

		updatedVpc, derr := vpcDAO.Update(ctx, tx, uvpcInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating VPC")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update VPC", nil)
		}
		vpc = updatedVpc

		clearInput := cdbm.VpcClearInput{VpcID: vpc.ID}
		shouldClear := false

		// NSG updates clear propagation details so users do not see stale status.
		// An empty ID also clears the VPC association itself.
		if apiRequest.NetworkSecurityGroupID != nil {
			if *apiRequest.NetworkSecurityGroupID == "" {
				clearInput.NetworkSecurityGroupID = true
			}

			// We should always clear details for any NSG change so that users don't see stale
			// status.
			clearInput.NetworkSecurityGroupPropagationDetails = true
			shouldClear = true
		}

		// If this request is attempting to clear the NVLink Logical Partition ID, set it.
		if apiRequest.NVLinkLogicalPartitionID != nil && *apiRequest.NVLinkLogicalPartitionID == "" {
			clearInput.NVLinkLogicalPartitionID = true
			shouldClear = true
		}

		// A changed desired definition invalidates the last controller-resolved value.
		if apiRequest.RoutingProfileOverrides != nil {
			clearInput.EffectiveRoutingProfile = true
			shouldClear = true
		}

		if apiRequest.PowerResourceGroup != nil && *apiRequest.PowerResourceGroup == "" {
			clearInput.PowerResourceGroup = true
			shouldClear = true
		}

		// Clear it in the db if something should be cleared.
		if shouldClear {
			clearedVpc, derr := vpcDAO.Clear(ctx, tx, clearInput)
			if derr != nil {
				logger.Error().Err(derr).Msg("error clearing requested VPC properties")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to clear requested VPC properties", nil)
			}
			vpc = clearedVpc
		}

		// Get status details
		fetchedSsds, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{vpc.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for VPC from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for VPC", nil)
		}
		ssds = fetchedSsds

		// Get the temporal client for the site we are working with.
		stc, derr := uvh.scp.GetClientByID(vpc.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		updateVpcRequest := apiRequest.ToProto(vpc)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "vpc-update-" + vpc.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering VPC update workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateVPC", updateVpcRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to update VPC")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update VPC on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update VPC workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to update VPC, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPC", "Update")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC update workflow timed out", nil)
			}

			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to update VPC")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update VPC on Site: %s", unwrapped), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update VPC workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			if dpsChange != nil {
				rollbackErr := dpsChange.Rollback()
				if rollbackErr != nil {
					logger.Error().Err(rollbackErr).Str("oldPowerResourceGroup", dpsOldGroup).Str("newPowerResourceGroup", dpsNewGroup).Msg("failed to compensate DPS resource group migration")
				}
			}
			return common.HandleTxError(c, logger, err, "Failed to update VPC due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		responseErr := timeoutResp()
		if dpsChange != nil {
			rollbackErr := dpsChange.Rollback()
			if rollbackErr != nil {
				logger.Error().Err(rollbackErr).Str("oldPowerResourceGroup", dpsOldGroup).Str("newPowerResourceGroup", dpsNewGroup).Msg("failed to compensate DPS resource group migration")
			}
		}
		return responseErr
	}
	if dpsChange != nil {
		cleanupErr := dpsChange.Complete()
		if cleanupErr != nil {
			logger.Error().Err(cleanupErr).Msg("failed to delete previous DPS resource group; external reconciliation is required")
		}
	}

	// Create response
	apiVpc := model.NewAPIVpc(*vpc, ssds, tenantCanViewEffectiveRoutingProfile)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiVpc)
}

func powerResourceGroupAPIError(err error, fallbackMessage string) *cutil.APIError {
	if errors.Is(err, dpsclient.ErrResourceGroupAlreadyExists) {
		return cutil.NewAPIError(http.StatusConflict, "Power resource group already exists", nil)
	}
	return cutil.NewAPIError(http.StatusServiceUnavailable, fallbackMessage, nil)
}

// ~~~~~ Update Virtualization Handler ~~~~~ //

// UpdateVPCVirtualizationHandler is the API Handler for updating virtualization of a VPC
type UpdateVPCVirtualizationHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateVPCVirtualizationHandler initializes and returns a new handler for updating virtualization of a VPC
func NewUpdateVPCVirtualizationHandler(dbSession *cdb.Session, tc temporalClient.Client, sc *sc.ClientPool, cfg *config.Config) UpdateVPCVirtualizationHandler {
	return UpdateVPCVirtualizationHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        sc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update virtualization of a VPC
// @Description Update the network virtualization of a VPC
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Vpc"
// @Param message body model.APIVpcVirtualizationUpdateRequest true "VPC virtualization update request"
// @Success 200 {object} model.APIVpc
// @Router /v2/org/{org}/nico/vpc/{id}/virtualization [patch]
func (uvvh UpdateVPCVirtualizationHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC", "Update Virtualization", c, uvvh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get vpc instance ID from URL param
	vpcStrID := c.Param("id")

	uvvh.tracerSpan.SetAttribute(handlerSpan, attribute.String("vpc_id", vpcStrID), logger)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIVpcVirtualizationUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Check that VPC exists (load Site for status and native networking / FNN eligibility checks)
	vpc, err := common.GetVpcFromIDString(ctx, nil, vpcStrID, []string{cdbm.SiteRelationName}, uvvh.dbSession)
	if err != nil {
		// Check if it's a UUID parsing error (happens before DB call)
		if err == common.ErrInvalidID {
			logger.Warn().Err(err).Msg("invalid VPC ID in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC ID in request", nil)
		}
		if err == cdb.ErrDoesNotExist {
			logger.Warn().Err(err).Msg("VPC not found")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve VPC to update", nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC DB entity")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate(vpc)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC virtualization update request data", verr)
	}

	// Get Tenant for this org
	tenant, err := common.GetTenantForOrg(ctx, nil, uvvh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	// Check that VPC belongs to the Tenant
	if vpc.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "VPC does not belong to current Tenant", nil)
	}

	tenantCanViewEffectiveRoutingProfile, err := common.TenantHasTargetedInstanceCreation(ctx, nil, uvvh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &vpc.SiteID})
	if err != nil {
		logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
	}

	// Ensure that Tenant has access to Site
	tsDAO := cdbm.NewTenantSiteDAO(uvvh.dbSession)
	_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, vpc.SiteID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to Site, VPC cannot be updated", nil)
		}

		logger.Error().Err(err).Msg("error retrieving Tenant Site association")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant/Site association, DB error", nil)
	}

	// Verify that the VPC Site is in a valid state
	if vpc.Site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site that VPC belongs to must be in Registered state in order to update virtualization type", nil)
	}

	// Get site config
	siteConfig := &cdbm.SiteConfig{}
	if vpc.Site.Config != nil {
		siteConfig = vpc.Site.Config
	}

	// Verify if site has been enabled for FNN type
	// No need to check for FNN type, as the request validator guarantees that
	if !siteConfig.NativeNetworking {
		logger.Warn().Msg(fmt.Sprintf("Site: %v that VPC belongs to does not have native networking enabled, unable to update virtualization type to FNN", vpc.SiteID))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site that VPC belongs to does not have native networking enabled, unable to update virtualization type to FNN", nil)
	}

	subnetDAO := cdbm.NewSubnetDAO(uvvh.dbSession)
	_, subnetCount, err := subnetDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Subnets count from DB for VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnets count for VPC", nil)
	}

	instanceDAO := cdbm.NewInstanceDAO(uvvh.dbSession)
	instanceCount, err := instanceDAO.GetCount(ctx, nil, cdbm.InstanceFilterInput{VpcIDs: []uuid.UUID{vpc.ID}})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Instances count from DB for VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Instances count for VPC", nil)
	}

	if subnetCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Virtualization Type cannot be changed while VPC contains one or more Subnets", nil)
	}

	if instanceCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Virtualization Type cannot be changed while VPC contains one or more Instances", nil)
	}

	vpcDAO := cdbm.NewVpcDAO(uvvh.dbSession)
	sdDAO := cdbm.NewStatusDetailDAO(uvvh.dbSession)

	var uv *cdbm.Vpc
	var ssds []cdbm.StatusDetail
	var timeoutResp func() error

	err = cdb.WithTx(ctx, uvvh.dbSession, func(tx *cdb.Tx) error {
		// Update VPC
		uvpcInput := cdbm.VpcUpdateInput{
			VpcID:                     vpc.ID,
			NetworkVirtualizationType: &apiRequest.NetworkVirtualizationType,
		}
		updatedVpc, derr := vpcDAO.Update(ctx, tx, uvpcInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating VPC")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update VPC virtualization, DB error", nil)
		}
		uv = updatedVpc

		// Get status details
		fetchedSsds, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{uv.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for VPC from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve status history for VPC", nil)
		}
		ssds = fetchedSsds

		// Get the temporal client for the site we are working with.
		stc, derr := uvvh.scp.GetClientByID(vpc.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// VPC virtualization type can only be updated to FNN, the request validator guarantees that
		siteVirtualizationType := corev1.VpcVirtualizationType_FNN
		siteRequest := &corev1.VpcUpdateVirtualizationRequest{
			Id:                        &corev1.VpcId{Value: vpc.GetSiteID().String()},
			NetworkVirtualizationType: &siteVirtualizationType,
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "vpc-update-virtualzation-" + uv.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering VPC virtualization update workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateVPCVirtualization", siteRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to update VPC virtualization")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update VPC on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update VPC virtualization workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || errors.Is(wferr, context.DeadlineExceeded) || wfCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPC", "UpdateVirtualization")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC virtualization update workflow timed out", nil)
			}
			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to update VPC virtualization")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update VPC virtualization on Site: %s", unwrapped), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update VPC workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update VPC virtualization due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	apiVpc := model.NewAPIVpc(*uv, ssds, tenantCanViewEffectiveRoutingProfile)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiVpc)
}

// ~~~~~ Get Handler ~~~~~ //

// GetVPCHandler is the API Handler for getting a VPC
type GetVPCHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetVPCHandler initializes and returns a new handler for getting VPC
func NewGetVPCHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetVPCHandler {
	return GetVPCHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a VPC
// @Description Get a VPC for the org
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Vpc"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Site', 'Tenant'"
// @Success 200 {object} model.APIVpc
// @Router /v2/org/{org}/nico/vpc/{id} [get]
func (gvh GetVPCHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC", "Get", c, gvh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.VpcRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get VPC ID from URL param
	vpcIDStr := c.Param("id")

	gvh.tracerSpan.SetAttribute(handlerSpan, attribute.String("vpc_id", vpcIDStr), logger)

	// Get VPC
	vpcDAO := cdbm.NewVpcDAO(gvh.dbSession)

	vpcID, err := uuid.Parse(vpcIDStr)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC ID in URL", nil)
	}

	vpc, err := vpcDAO.GetByID(ctx, nil, vpcID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find VPC with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC", nil)
	}

	// Get Tenant for this org
	tenant, err := common.GetTenantForOrg(ctx, nil, gvh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant", nil)
	}

	// Check if VPC belongs to Tenant
	if vpc.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "VPC does not belong to current Tenant", nil)
	}

	tenantCanViewEffectiveRoutingProfile := false
	// Existing non-FNN rows can carry cached effective state, so gate that state
	// instead of hiding it solely based on VPC type.
	if cdbm.VpcTypeSupportsRoutingProfile(vpc.NetworkVirtualizationType) || vpc.EffectiveRoutingProfile != nil {
		tenantCanViewEffectiveRoutingProfile, err = common.TenantHasTargetedInstanceCreation(ctx, nil, gvh.dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &vpc.SiteID})
		if err != nil {
			logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
		}
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gvh.dbSession)

	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{vpcID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for VPC from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for VPC", nil)
	}

	// Create response
	vc := model.NewAPIVpc(*vpc, ssds, tenantCanViewEffectiveRoutingProfile)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, vc)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllVPCHandler is the API Handler for retrieving all VPCs
type GetAllVPCHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllVPCHandler initializes and returns a new handler for retreiving all VPCs
func NewGetAllVPCHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllVPCHandler {
	return GetAllVPCHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all VPCs
// @Description Get all VPCs for the org
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "Site ID"
// @Param nvLinkLogicalPartitionId query string false "NVLink Logical Partition ID"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Site', 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} []model.APIVpc
// @Router /v2/org/{org}/nico/vpc [get]
func (gavh GetAllVPCHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC", "GetAll", c, gavh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.VpcOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	var queryTenantID *uuid.UUID
	if tenantID := c.QueryParam("tenantId"); tenantID != "" {
		parsedTenantID, parseErr := uuid.Parse(tenantID)
		if parseErr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Tenant ID specified in query", nil)
		}
		queryTenantID = &parsedTenantID
	}
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.VpcRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get site IDs from query param
	var siteIDs []uuid.UUID

	siteIDStrs := qParams["siteId"]
	if len(siteIDStrs) > 0 {
		gavh.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("siteId", siteIDStrs), logger)
		for _, idStr := range siteIDStrs {
			parsedID, serr := uuid.Parse(idStr)
			if serr != nil {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid Site ID in query: %s", idStr), nil)
			}
			siteIDs = append(siteIDs, parsedID)
		}

		// Check for Site existence
		stDAO := cdbm.NewSiteDAO(gavh.dbSession)
		sites, _, err := stDAO.GetAll(ctx, nil, cdbm.SiteFilterInput{SiteIDs: siteIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Warn().Err(err).Msg("error retrieving Sites from DB by IDs")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve Sites with IDs specified in query", nil)
		}
		// Build a set of site IDs for efficient lookup
		sitesByID := make(map[uuid.UUID]struct{}, len(sites))
		for _, site := range sites {
			sitesByID[site.ID] = struct{}{}
		}
		// For each siteIDStr, check if the corresponding site exists
		for _, siteID := range siteIDs {
			if _, ok := sitesByID[siteID]; !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Could not find Site with ID specified in query: %s", siteID.String()), nil)
			}
		}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gavh.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	// Get status from query param
	var statuses []string
	if statusStrings := qParams["status"]; len(statusStrings) != 0 {
		gavh.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("status", statusStrings), logger)
		for _, status := range statusStrings {
			_, ok := cdbm.VpcStatusMap[status]
			if !ok {
				logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", status))
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
			}
			statuses = append(statuses, status)
		}
	}

	// Get Tenant for this org
	tnDAO := cdbm.NewTenantDAO(gavh.dbSession)

	tenants, _, err := tnDAO.GetAll(ctx, nil, cdbm.TenantFilterInput{Orgs: []string{org}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Tenant for this org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	if len(tenants) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org does not have a Tenant associated", nil)
	}
	tenant := tenants[0]
	if queryTenantID != nil && *queryTenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant ID specified in query param is not associated with current org", nil)
	}

	privilegedSiteIDs, err := common.GetPrivilegedAccessSiteIDsForTenant(ctx, nil, gavh.dbSession, &tenant)
	if err != nil {
		logger.Error().Err(err).Msg("error resolving privileged Site access for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability, DB error", nil)
	}
	privilegedSites := make(map[uuid.UUID]bool, len(privilegedSiteIDs))
	for _, siteID := range privilegedSiteIDs {
		privilegedSites[siteID] = true
	}

	// Get all VPCs by Tenant, and Site, if specified
	vpcDAO := cdbm.NewVpcDAO(gavh.dbSession)

	vpcFilter := cdbm.VpcFilterInput{
		Org:         &org,
		SearchQuery: searchQuery,
		TenantIDs:   []uuid.UUID{tenant.ID},
	}

	if len(siteIDs) > 0 {
		vpcFilter.SiteIDs = siteIDs
	}

	if len(statuses) > 0 {
		vpcFilter.Statuses = statuses
	}

	// Get network security group IDs from query param
	networkSecurityGroupIDs := qParams["networkSecurityGroupId"]
	if len(networkSecurityGroupIDs) > 0 {
		gavh.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("networkSecurityGroupId", networkSecurityGroupIDs), logger)
		networkSecurityGroupDAO := cdbm.NewNetworkSecurityGroupDAO(gavh.dbSession)

		networkSecurityGroups, _, err := networkSecurityGroupDAO.GetAll(
			ctx,
			nil,
			cdbm.NetworkSecurityGroupFilterInput{NetworkSecurityGroupIDs: networkSecurityGroupIDs, TenantIDs: []uuid.UUID{tenant.ID}},
			cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Network Security Groups from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Network Security Groups specified in query", nil)
		}
		networkSecurityGroupIDsMap := make(map[string]struct{}, len(networkSecurityGroups))
		for _, networkSecurityGroup := range networkSecurityGroups {
			networkSecurityGroupIDsMap[networkSecurityGroup.ID] = struct{}{}
		}
		for _, nsgID := range networkSecurityGroupIDs {
			if _, ok := networkSecurityGroupIDsMap[nsgID]; !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Network Security Group ID: %s specified in query does not exist for current Tenant", nsgID), nil)
			}
		}
		vpcFilter.NetworkSecurityGroupIDs = networkSecurityGroupIDs
	}

	qNvLinkLogicalPartitionIDStrs := qParams["nvLinkLogicalPartitionId"]
	if len(qNvLinkLogicalPartitionIDStrs) > 0 {
		gavh.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("nvLinkLogicalPartitionId", qNvLinkLogicalPartitionIDStrs), logger)
		nvllpDAO := cdbm.NewNVLinkLogicalPartitionDAO(gavh.dbSession)
		nvLinkLogicalPartitionIDs := make([]uuid.UUID, 0, len(qNvLinkLogicalPartitionIDStrs))
		for _, nvLinkLogicalPartitionIDStr := range qNvLinkLogicalPartitionIDStrs {
			nvLinkLogicalPartitionID, err := uuid.Parse(nvLinkLogicalPartitionIDStr)
			if err != nil {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Invalid NVLink Logical Partition ID: %s in query", nvLinkLogicalPartitionIDStr), nil)
			}
			nvLinkLogicalPartitionIDs = append(nvLinkLogicalPartitionIDs, nvLinkLogicalPartitionID)
		}
		nvLinkLogicalPartitions, _, err := nvllpDAO.GetAll(ctx, nil, cdbm.NVLinkLogicalPartitionFilterInput{NVLinkLogicalPartitionIDs: nvLinkLogicalPartitionIDs, TenantIDs: []uuid.UUID{tenant.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving NVLink Logical Partitions from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve NVLink Logical Partitions specified in query", nil)
		}
		nvLinkLogicalPartitionIDsMap := make(map[uuid.UUID]struct{}, len(nvLinkLogicalPartitions))
		for _, nvLinkLogicalPartition := range nvLinkLogicalPartitions {
			nvLinkLogicalPartitionIDsMap[nvLinkLogicalPartition.ID] = struct{}{}
		}
		for _, nvLinkLogicalPartitionID := range nvLinkLogicalPartitionIDs {
			if _, ok := nvLinkLogicalPartitionIDsMap[nvLinkLogicalPartitionID]; !ok {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("NVLink Logical Partition ID: %s specified in query does not exist for current Tenant", nvLinkLogicalPartitionID.String()), nil)
			}
		}
		vpcFilter.NVLinkLogicalPartitionIDs = nvLinkLogicalPartitionIDs
	}

	vpcPageInput := cdbp.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}

	vpcs, total, serr := vpcDAO.GetAll(
		ctx,
		nil,
		vpcFilter,
		vpcPageInput,
		qIncludeRelations,
	)

	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving VPCs for this Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPCs for Site", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gavh.dbSession)

	sdEntityIDs := []string{}
	for _, vpc := range vpcs {
		sdEntityIDs = append(sdEntityIDs, vpc.ID.String())
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for VPCs from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for VPCs", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiVpcs := []model.APIVpc{}

	for _, vpc := range vpcs {
		apiVpc := model.NewAPIVpc(vpc, ssdMap[vpc.ID.String()], privilegedSites[vpc.SiteID])
		apiVpcs = append(apiVpcs, apiVpc)
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

	return c.JSON(http.StatusOK, apiVpcs)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteVPCHandler is the API Handler for deleting a VPC
type DeleteVPCHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	dps        dpsclient.PowerProvisioner
	tracerSpan *cutil.TracerSpan
}

// NewDeleteVPCHandler initializes and returns a new handler for deleting VPC
func NewDeleteVPCHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config, dps dpsclient.PowerProvisioner) DeleteVPCHandler {
	return DeleteVPCHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		dps:        dps,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete a VPC
// @Description Delete a VPC fro the org
// @Tags vpc
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of VPC"
// @Success 202
// @Router /v2/org/{org}/nico/vpc/{id} [delete]
func (dvh DeleteVPCHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC", "Delete", c, dvh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get VPC ID from URL param
	vpcStrID := c.Param("id")
	vpcID, err := uuid.Parse(vpcStrID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC ID in URL", nil)
	}

	dvh.tracerSpan.SetAttribute(handlerSpan, attribute.String("vpc_id", vpcStrID), logger)

	// Get VPC from DB
	vpcDAO := cdbm.NewVpcDAO(dvh.dbSession)
	vpc, err := vpcDAO.GetByID(ctx, nil, vpcID, []string{
		cdbm.SiteRelationName,
		cdbm.TenantRelationName,
	})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find VPC with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving VPC from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC with specified ID", nil)
	}

	if vpc.Tenant == nil {
		logger.Warn().Err(err).Msg("failed to retrieve Tenant details")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant details", nil)
	}

	// Validate the tenant for which this VPC is being deleted
	if vpc.Tenant.Org != org {
		logger.Warn().Msg("org specified in request does not match org of Tenant associated with VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org specified in request does not match org of Tenant associated with VPC", nil)
	}

	// Verify that the VPC is associated with a site and then that the site is
	// in a valid state.
	if vpc.Site == nil {
		logger.Error().Msg("failed to pull site data for VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for VPC", nil)
	}

	// Verify if site is ready
	if vpc.Site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Str("Site ID", vpc.SiteID.String()).Msg("Site associated with VPC must be in Registered state in order to proceed")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site associated with VPC must be in Registered state in order to proceed", nil)
	}

	// Check if VPC has any resources subnet attached
	// Check if VPC has subnet attached
	sbDAO := cdbm.NewSubnetDAO(dvh.dbSession)
	subnets, _, err := sbDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{TenantIDs: []uuid.UUID{vpc.TenantID}, VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Subnet for this VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnets for this VPC", nil)
	}
	if len(subnets) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot delete VPC, one or more Subnets exist for this VPC", nil)
	}

	// Check if VPC has VPC prefix attached
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(dvh.dbSession)
	vpcPrefixes, _, err := vpcPrefixDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{TenantIDs: []uuid.UUID{vpc.TenantID}, VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC prefixes for this VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC prefixes for this VPC", nil)
	}
	if len(vpcPrefixes) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot delete VPC, one or more VPC prefixes exist for this VPC", nil)
	}

	// Check if VPC has instance
	insDAO := cdbm.NewInstanceDAO(dvh.dbSession)
	instances, _, err := insDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{TenantIDs: []uuid.UUID{vpc.TenantID}, VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving instances for this VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve instances for this VPC", nil)
	}
	if len(instances) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Cannot delete VPC, one or more instances for this VPC", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(dvh.dbSession)

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dvh.dbSession, func(tx *cdb.Tx) error {
		if dvh.cfg.GetDPSEnabled() {
			lockErr := powerutil.AcquireVPCPowerLock(ctx, tx, vpc.ID)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to serialize DPS operations for VPC")
				return cutil.NewAPIError(http.StatusConflict, "Another power operation is already in progress for the VPC", nil)
			}
			lockedVPC, lockErr := vpcDAO.GetByID(ctx, tx, vpc.ID, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to reload VPC after acquiring its power lock")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to reload VPC power configuration", nil)
			}
			vpc = lockedVPC
			lockedInstances, _, lockErr := insDAO.GetAll(ctx, tx, cdbm.InstanceFilterInput{TenantIDs: []uuid.UUID{vpc.TenantID}, VpcIDs: []uuid.UUID{vpc.ID}}, cdbp.PageInput{}, nil)
			if lockErr != nil {
				logger.Error().Err(lockErr).Str("vpcID", vpc.ID.String()).Msg("failed to retrieve instances for VPC deletion")
				return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve instances for this VPC", nil)
			}
			if len(lockedInstances) > 0 {
				return cutil.NewAPIError(http.StatusConflict, "Cannot delete VPC, one or more instances for this VPC", nil)
			}
		}

		// Update VPC to set status to Deleting
		uvpcInput := cdbm.VpcUpdateInput{
			VpcID:  vpc.ID,
			Status: cutil.GetPtr(cdbm.VpcStatusDeleting),
		}
		if _, derr := vpcDAO.Update(ctx, tx, uvpcInput); derr != nil {
			logger.Error().Err(derr).Msg("error updating VPC in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete VPC", nil)
		}

		// Create status detail (best-effort: original code only logs on error)
		if _, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpc.ID.String(), Status: *cutil.GetPtr(cdbm.VpcStatusDeleting), Message: cutil.GetPtr("received request for deletion, pending processing")}); derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dvh.scp.GetClientByID(vpc.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		deleteVpcRequest := &corev1.VpcDeletionRequest{
			Id: &corev1.VpcId{Value: vpc.GetSiteID().String()},
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "vpc-delete-" + vpc.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering VPC delete workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteVPCV2", deleteVpcRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to delete VPC")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to delete VPC on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete VPC workflow")

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

		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to delete VPC, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPC", "Delete")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC delete workflow timed out", nil)
			}

			code, unwrapped := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(unwrapped).Msg("failed to synchronously execute Temporal workflow to delete VPC")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete VPC on Site: %s", unwrapped), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete VPC workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete VPC due to DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}
	if dvh.cfg.GetDPSEnabled() && vpc.PowerResourceGroup != nil && dvh.dps != nil {
		cleanupErr := dvh.dps.DeleteResourceGroup(context.WithoutCancel(ctx), *vpc.PowerResourceGroup)
		if cleanupErr != nil {
			logger.Error().Err(cleanupErr).Str("powerResourceGroup", *vpc.PowerResourceGroup).Msg("failed to delete DPS resource group; external reconciliation is required")
		}
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
