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
	"strconv"

	"github.com/labstack/echo/v4"

	cip "github.com/NVIDIA/infra-controller/rest-api/ipam"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

const DefaultReservedIPCount = 2

// ~~~~~ Create Handler ~~~~~ //

// CreateSubnetHandler creates IPv4 Subnets for Ethernet virtualizer VPCs.
// FNN VPCs use the separate VPC Prefix resource.
type CreateSubnetHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateSubnetHandler initializes and returns a new handler for creating Subnet
func NewCreateSubnetHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateSubnetHandler {
	return CreateSubnetHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create Subnet
// @Description Creates an IPv4 Subnet in an Ethernet virtualizer VPC. FNN VPCs use VPC Prefixes.
// @Tags Subnet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APISubnetCreateRequest true "Subnet creation request"
// @Success 201 {object} model.APISubnet
// @Router /v2/org/{org}/nico/subnet [post]
func (csh CreateSubnetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Subnet", "Create", c, csh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Subnet endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISubnetCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Subnet creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Subnet creation request data", verr)
	}

	// Validate the tenant for which this Subnet is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, csh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}
	// Verify vpc in request
	vpc, err := common.GetVpcFromIDString(ctx, nil, apiRequest.VpcID, nil, csh.dbSession)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting vpc in request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find VPC specified in request", nil)
	}
	if vpc.TenantID != tenant.ID {
		logger.Warn().Msg("tenant in vpc does not belong to tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for VPC in request does not match tenant in org", nil)
	}

	// A nil `NetworkVirtualizationType` represents legacy Ethernet virtualizer VPCs.
	if vpc.NetworkVirtualizationType != nil && *vpc.NetworkVirtualizationType != cdbm.VpcEthernetVirtualizer {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v must be an Ethernet virtualizer VPC for REST Subnet creation", vpc.ID), nil)
	}

	// Verify if vpc is ready
	if vpc.ControllerVpcID == nil || vpc.Status != cdbm.VpcStatusReady {
		logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request data must be in Ready state in order to create Subnet", apiRequest.VpcID))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC specified in request data must be in Ready state in order to create Subnet", nil)
	}

	// Verify if site is ready
	stDAO := cdbm.NewSiteDAO(csh.dbSession)
	site, err := stDAO.GetByID(ctx, nil, vpc.SiteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site associated with Subnet", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site with ID from VPC", nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("The Site: %v where the Subnet is being created must be in Registered state in order to proceed", vpc.SiteID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where the Subnet is being created must be in Registered state in order to proceed", nil)
	}

	// Resolve the public REST Domain ID to the Site-facing Core Domain ID.
	// The local ID remains the persisted Subnet relation.
	var domain *cdbm.Domain
	if apiRequest.SubdomainID != nil {
		domainID, derr := uuid.Parse(*apiRequest.SubdomainID)
		if derr != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid subdomainId in request data", nil)
		}
		domain, derr = cdbm.NewDomainDAO(csh.dbSession).GetByID(ctx, nil, domainID, nil)
		if derr != nil {
			if derr == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Domain specified by subdomainId", nil)
			}
			logger.Error().Err(derr).Msg("error retrieving Domain specified by subdomainId")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Domain specified by subdomainId", nil)
		}
		if domain.TenantID == nil || *domain.TenantID != tenant.ID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Domain specified by subdomainId does not belong to the Tenant", nil)
		}
		if domain.SiteID == nil || *domain.SiteID != site.ID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Domain specified by subdomainId does not belong to the VPC Site", nil)
		}
		if domain.Status != cdbm.DomainStatusReady || domain.ControllerDomainID == nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Domain specified by subdomainId must be in Ready state", nil)
		}
		apiRequest.ControllerDomainID = domain.ControllerDomainID
	}

	// Validate the Ready, tenant-allocated IPv4 IP block for this Subnet.
	// Model validation ensures IPv4BlockID is non-nil.
	ipBlockFilter := cdbm.IPBlockFilterInput{}
	ipBlockFilter.TenantAllocated(tenant.ID)
	ipBlockFilter.Statuses = []string{cdbm.IPBlockStatusReady}
	ipv4Block, err := common.GetIPBlockFromIDString(ctx, nil, *apiRequest.IPv4BlockID, ipBlockFilter, csh.dbSession)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting IPv4 IPBlock in request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find a Ready, tenant-allocated IPv4 IP block specified by ipv4BlockId", nil)
	}
	if ipv4Block.ProtocolVersion != cdbm.IPBlockProtocolVersionV4 {
		logger.Warn().Msg("ipv4BlockId must reference an IPv4 IP block for Subnet creation")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "ipv4BlockId must reference an IPv4 IP block for Subnet creation", nil)
	}
	if vpc.SiteID != ipv4Block.SiteID {
		logger.Warn().Msg("IPv4 Block specified in request and VPC do not belong to the same Site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "IPv4 Block specified in request and VPC do not belong to the same Site", nil)
	}
	// A REST Subnet uses one IPv4 block, so its routing type comes from that block.
	routingType := ipv4Block.RoutingType

	// Check for name uniqueness for the tenant, ie, Tenant cannot have another Subnet with same name at the Site
	// TODO consider doing this with an advisory lock for correctness
	sDAO := cdbm.NewSubnetDAO(csh.dbSession)
	sbs, tot, err := sDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{Names: []string{apiRequest.Name}, SiteIDs: []uuid.UUID{vpc.SiteID}, TenantIDs: []uuid.UUID{tenant.ID}}, paginator.PageInput{}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant subnet")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Subnet due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.Name).Msg("subnet with same name already exists for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "A Subnet with specified name already exists for Tenant at this Site", validation.Errors{
			"id": errors.New(sbs[0].ID.String()),
		})
	}

	sdDAO := cdbm.NewStatusDetailDAO(csh.dbSession)

	var subnet *cdbm.Subnet
	var ssd *cdbm.StatusDetail
	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, csh.dbSession, func(tx *cdb.Tx) error {
		// acquire an advisory lock on the parent IP block ID on which there could be contention
		// this lock is released when the transaction commits or rollsback
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(fmt.Sprintf("%s-%s", tenant.ID.String(), ipv4Block.ID.String())), nil)
		if derr != nil {
			// TODO add a retry here
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on ipblock")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error creating Subnet, detected multiple parallel request on IP Block by Tenant", nil)
		}

		// create an IPAM allocation for the subnet
		// allocate a child prefix in ipam
		ipamStorage := ipam.NewIpamStorage(csh.dbSession.DB, tx.GetBunTx())
		childPrefix, derr := ipam.CreateChildIpamEntryForIPBlock(ctx, tx, csh.dbSession, ipamStorage, ipv4Block, apiRequest.PrefixLength)
		if derr != nil {
			// printing parent prefix usage to debug the child prefix failure
			parentPrefix, serr := ipamStorage.ReadPrefix(ctx, ipv4Block.Prefix, ipam.GetIpamNamespaceForIPBlock(ctx, ipv4Block.RoutingType, ipv4Block.InfrastructureProviderID.String(), ipv4Block.SiteID.String()))
			if serr == nil {
				logger.Info().Str("IP Block ID", ipv4Block.ID.String()).Str("IP Block Prefix", ipv4Block.Prefix).Msgf("%+v\n", parentPrefix.Usage())
			}

			logger.Warn().Err(derr).Msg("failed to create IPAM entry for subnet")
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not create IPAM entry for Subnet. Details: %s", derr.Error()), nil)
		}
		logger.Info().Str("childCidr", childPrefix.Cidr).Msg("created child cidr for subnet")

		// get the prefix and gateway IP addresses
		ipv4Prefix, _, derr := ipam.ParseCidrIntoPrefixAndBlockSize(childPrefix.Cidr)
		if derr != nil {
			logger.Warn().Err(derr).Msg("unable to parse cidr")
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not create IPAM entry for Subnet. Details: %s", derr.Error()), nil)
		}

		ipv4Gateway, derr := ipam.GetFirstIPFromCidr(childPrefix.Cidr)
		if derr != nil {
			logger.Warn().Err(derr).Msg("unable to get first ip in cidr")
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not create IPAM entry for Subnet. Details: %s", derr.Error()), nil)
		}

		var domainID *uuid.UUID
		if domain != nil {
			domainID = &domain.ID
		}

		// Create Subnet in DB
		subnet, derr = sDAO.Create(
			ctx, tx, cdbm.SubnetCreateInput{
				Name:         apiRequest.Name,
				Description:  apiRequest.Description,
				Org:          org,
				SiteID:       site.ID,
				VpcID:        vpc.ID,
				DomainID:     domainID,
				TenantID:     tenant.ID,
				RoutingType:  &routingType,
				IPv4Prefix:   &ipv4Prefix,
				IPv4Gateway:  &ipv4Gateway,
				IPv4BlockID:  &ipv4Block.ID,
				PrefixLength: apiRequest.PrefixLength,
				Status:       cdbm.SubnetStatusPending,
				CreatedBy:    dbUser.ID,
			})
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create Subnet record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating subnet record", nil)
		}

		// Update the controller ID for the subnet.
		// We need this to match the subnet ID.  This was previously handled
		// by the async cloud workflow after successful creation on site.
		subnet, derr = sDAO.Update(ctx, tx, cdbm.SubnetUpdateInput{SubnetId: subnet.ID, ControllerNetworkSegmentID: cutil.GetPtr(subnet.ID)})
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to update Subnet record controllerNetworkSegmentId")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed updating new subnet record", nil)
		}

		// create the status detail record
		ssd, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: subnet.ID.String(), Status: *cutil.GetPtr(cdbm.SubnetStatusPending), Message: cutil.GetPtr("received subnet creation request, pending")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Subnet", nil)
		}
		if ssd == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Subnet", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := csh.scp.GetClientByID(subnet.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		createSubnetRequest := apiRequest.ToProto(subnet, vpc, DefaultReservedIPCount)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "subnet-create-" + subnet.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering Subnet create workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateSubnetV2", createSubnetRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to create Subnet")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to create Subnet on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create Subnet workflow")

		// Block until the workflow has completed and returned success/error.
		wferr = we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to create Subnet, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "Subnet", "CreateSubnetV2")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Subnet create workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to create Subnet")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create Subnet on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create Subnet workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create Subnet, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// create response
	apiInstance := model.NewAPISubnet(subnet, []cdbm.StatusDetail{*ssd}, nil)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiInstance)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllSubnetHandler is the API Handler for getting all Subnets
type GetAllSubnetHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllSubnetHandler initializes and returns a new handler for getting all Subnets
func NewGetAllSubnetHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllSubnetHandler {
	return GetAllSubnetHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all Subnets
// @Description Get all Subnets
// @Tags Subnet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "Site ID"
// @Param vpcId query string true "ID of Vpc"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Vpc', 'Tenant', 'IPv4Block', 'IPv6Block'"
// @Param includeUsageStats query boolean false "Subnet IPv4 usage (interface/instance-derived; same shape as IP Block usage)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APISubnet
// @Router /v2/org/{org}/nico/subnet [get]
func (gash GetAllSubnetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Subnet", "GetAll", c, gash.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Subnet endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate paginantion request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate request attributes
	err = pageRequest.Validate(cdbm.SubnetOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}
	subnetFilter := cdbm.SubnetFilterInput{}

	// Validate the tenant for which this Subnet is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, gash.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}
	subnetFilter.TenantIDs = []uuid.UUID{tenant.ID}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SubnetRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	includeUsageStats := false
	qius := c.QueryParam("includeUsageStats")
	if qius != "" {
		includeUsageStats, err = strconv.ParseBool(qius)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeUsageStats` query param", nil)
		}
	}
	queryIncludeRelations := slices.Clone(qIncludeRelations)
	if includeUsageStats && !slices.Contains(queryIncludeRelations, cdbm.IPv4BlockRelationName) {
		queryIncludeRelations = append(queryIncludeRelations, cdbm.IPv4BlockRelationName)
	}

	// Get site ID from query param
	tsDAO := cdbm.NewTenantSiteDAO(gash.dbSession)
	var siteID *uuid.UUID
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gash.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting site in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query param, invalid ID or DB error", nil)
		}
		siteID = &site.ID

		// Check Site association with Tenant
		_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to this Site", nil)
			}
			logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
		}
		subnetFilter.SiteIDs = []uuid.UUID{*siteID}
	}

	// verify vpc if specified in query string
	qVpcID := c.QueryParam("vpcId")
	var vpcID *uuid.UUID
	if qVpcID != "" {
		vpc, err := common.GetVpcFromIDString(ctx, nil, qVpcID, nil, gash.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting vpc in request")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find VPC specified in request", nil)
		}
		if vpc.TenantID != tenant.ID {
			logger.Warn().Msg("tenant in vpc does not belong to tenant in org")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for VPC in request does not match tenant in org", nil)
		}
		vpcID = &vpc.ID
		subnetFilter.VpcIDs = []uuid.UUID{*vpcID}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gash.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
		subnetFilter.SearchQuery = searchQuery
	}

	// Get status from query param
	var status *string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		gash.tracerSpan.SetAttribute(handlerSpan, attribute.String("status", statusQuery), logger)
		_, ok := cdbm.SubnetStatusMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		status = &statusQuery
		subnetFilter.Statuses = []string{*status}
	}

	// Create response
	sDAO := cdbm.NewSubnetDAO(gash.dbSession)
	subnets, total, err := sDAO.GetAll(ctx, nil, subnetFilter, paginator.PageInput{
		Limit:   pageRequest.Limit,
		Offset:  pageRequest.Offset,
		OrderBy: pageRequest.OrderBy,
	}, queryIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error getting subnets from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnets", nil)
	}

	sbusageMap := map[uuid.UUID]*cip.Usage{}
	if includeUsageStats {
		subnetsForUsage := make([]*cdbm.Subnet, 0, len(subnets))
		for i := range subnets {
			sn := &subnets[i]
			if sn.IPv4Block == nil {
				logger.Error().Str("subnetId", sn.ID.String()).Msg("Subnet missing IPv4 Block relation for usage stats")
				continue
			}
			subnetsForUsage = append(subnetsForUsage, sn)
		}
		if len(subnetsForUsage) > 0 {
			prefixUsageMap, serr := sDAO.GetPrefixUsage(ctx, nil, subnetsForUsage...)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving usage stats for Subnets")
			} else {
				for id, usage := range prefixUsageMap {
					sbusageMap[id] = usage
				}
			}
		}
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gash.dbSession)

	sdEntityIDs := []string{}
	for _, sn := range subnets {
		sdEntityIDs = append(sdEntityIDs, sn.ID.String())
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for Subnets from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Subnets", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiSubnets := []*model.APISubnet{}

	// get status details
	for _, sn := range subnets {
		cursn := sn
		snusage := sbusageMap[sn.ID]
		apiSubnet := model.NewAPISubnet(&cursn, ssdMap[sn.ID.String()], snusage)
		apiSubnets = append(apiSubnets, apiSubnet)
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

	return c.JSON(http.StatusOK, apiSubnets)
}

// ~~~~~ Get Handler ~~~~~ //

// GetSubnetHandler is the API Handler for retrieving Subnet
type GetSubnetHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetSubnetHandler initializes and returns a new handler to retrieve Subnet
func NewGetSubnetHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetSubnetHandler {
	return GetSubnetHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the Subnet
// @Description Retrieve the Subnet
// @Tags Subnet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Subnet"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Vpc', 'Tenant', 'IPv4Block', 'IPv6Block'"
// @Param includeUsageStats query boolean false "Subnet IPv4 usage (interface/instance-derived; same shape as IP Block usage)"
// @Success 200 {object} model.APISubnet
// @Router /v2/org/{org}/nico/subnet/{id} [get]
func (gsh GetSubnetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Subnet", "Get", c, gsh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Subnet endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.SubnetRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	includeUsageStats := false
	qius := c.QueryParam("includeUsageStats")
	if qius != "" {
		includeUsageStats, err = strconv.ParseBool(qius)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeUsageStats` query param", nil)
		}
	}

	queryIncludeRelations := slices.Clone(qIncludeRelations)
	if includeUsageStats && !slices.Contains(queryIncludeRelations, cdbm.IPv4BlockRelationName) {
		queryIncludeRelations = append(queryIncludeRelations, cdbm.IPv4BlockRelationName)
	}

	// Get subnet ID from URL param
	sStrID := c.Param("id")

	gsh.tracerSpan.SetAttribute(handlerSpan, attribute.String("subnet_id", sStrID), logger)

	sID, err := uuid.Parse(sStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Subnet ID in URL", nil)
	}

	sDAO := cdbm.NewSubnetDAO(gsh.dbSession)

	// Validate the tenant for which this Subnet is being updated
	tenant, err := common.GetTenantForOrg(ctx, nil, gsh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}

	// Check that subnet exists
	subnet, err := sDAO.GetByID(ctx, nil, sID, queryIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Subnet DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Subnet to update", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve Subnet to update", nil)
	}

	// verify tenant matches
	if tenant.ID != subnet.TenantID {
		logger.Warn().Msg("tenant in subnet does not belong to tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for subnet in request does not match tenant in org", nil)
	}

	// get status details for the response
	sdDAO := cdbm.NewStatusDetailDAO(gsh.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{subnet.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for subnet from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for subnet", nil)
	}

	var sbusage *cip.Usage
	if includeUsageStats {
		if subnet.IPv4Block == nil {
			logger.Error().Str("subnetId", subnet.ID.String()).Msg("Subnet missing IPv4 Block relation for usage stats")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats for Subnet", nil)
		}
		prefixUsageMap, err := sDAO.GetPrefixUsage(ctx, nil, subnet)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving usage stats for Subnet")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats for Subnet", nil)
		}
		var ok bool
		sbusage, ok = prefixUsageMap[subnet.ID]
		if !ok {
			logger.Error().Str("subnetId", subnet.ID.String()).Msg("Subnet missing IPv4 prefix for usage stats")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats for Subnet", nil)
		}
	}

	// Send response
	apiInstance := model.NewAPISubnet(subnet, ssds, sbusage)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateSubnetHandler is the API Handler for updating a Subnet
type UpdateSubnetHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateSubnetHandler initializes and returns a new handler for updating Subnet
func NewUpdateSubnetHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) UpdateSubnetHandler {
	return UpdateSubnetHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing Subnet
// @Description Update an existing Subnet
// @Tags Subnet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Subnet"
// @Param message body model.APISubnetUpdateRequest true "Subnet update request"
// @Success 200 {object} model.APISubnet
// @Router /v2/org/{org}/nico/subnet/{id} [patch]
func (ush UpdateSubnetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Subnet", "Update", c, ush.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Subnet endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get subnet ID from URL param
	sStrID := c.Param("id")

	ush.tracerSpan.SetAttribute(handlerSpan, attribute.String("subnet_id", sStrID), logger)

	sID, err := uuid.Parse(sStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Subnet ID in URL", nil)
	}

	sDAO := cdbm.NewSubnetDAO(ush.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APISubnetUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Subnet update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Subnet update request data", verr)
	}

	// Validate the tenant for which this Subnet is being updated
	tenant, err := common.GetTenantForOrg(ctx, nil, ush.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}

	// Check that subnet exists
	subnet, err := sDAO.GetByID(ctx, nil, sID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Subnet DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve Subnet to update", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve Subnet to update", nil)
	}

	// verify tenant matches
	if tenant.ID != subnet.TenantID {
		logger.Warn().Msg("tenant in subnet does not belong to tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for subnet in request does not match tenant in org", nil)
	}

	if apiRequest.Name != nil && *apiRequest.Name != subnet.Name {
		sbs, tot, serr := sDAO.GetAll(ctx, nil, cdbm.SubnetFilterInput{Names: []string{*apiRequest.Name}, SiteIDs: []uuid.UUID{subnet.SiteID}, TenantIDs: []uuid.UUID{tenant.ID}}, paginator.PageInput{}, []string{})
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant subnet")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create Subnet due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another Subnet with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(sbs[0].ID.String()),
			})
		}
	}

	// The Update is the sole write — perform it via WithTxResult so the
	// updated Subnet drops out cleanly. Status detail fetch is a pure read
	// that doesn't depend on the Update's mutations (Update doesn't touch
	// status detail rows), so it stays outside the tx per AGENTS.md rule 3.
	subnet, err = cdb.WithTxResult(ctx, ush.dbSession, func(tx *cdb.Tx) (*cdbm.Subnet, error) {
		usubnet, derr := sDAO.Update(ctx, tx, cdbm.SubnetUpdateInput{SubnetId: sID, Name: apiRequest.Name, Description: apiRequest.Description})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating Subnet in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Subnet", nil)
		}
		return usubnet, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Subnet, DB transaction error")
	}

	// get status details for the response — best-effort, the PATCH has already
	// committed so a transient read failure here must not surface as 500.
	sdDAO := cdbm.NewStatusDetailDAO(ush.dbSession)
	ssds, _, err := sdDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{subnet.ID.String()}}, paginator.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Status Details for subnet after update commit")
		ssds = nil
	}

	// Send response
	apiInstance := model.NewAPISubnet(subnet, ssds, nil)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInstance)
}

// ~~~~~ Attach VPC Handler ~~~~~ //

// AttachSubnetVpcHandler reassigns an unallocated tenant Subnet to another Ethernet virtualizer VPC.
type AttachSubnetVpcHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewAttachSubnetVpcHandler initializes and returns a Subnet VPC attachment handler.
func NewAttachSubnetVpcHandler(dbSession *cdb.Session, scp *sc.ClientPool) AttachSubnetVpcHandler {
	return AttachSubnetVpcHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Attach an existing Subnet to a VPC
// @Description Reassigns an unallocated tenant Subnet between Ethernet virtualizer VPCs at the same Site.
// @Tags Subnet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param subnetId path string true "ID of Subnet"
// @Param message body model.APISubnetAttachVpcRequest true "Subnet attach VPC request"
// @Success 200 {object} model.APISubnet
// @Router /v2/org/{org}/nico/subnet/{subnetId}/attach-vpc [post]
func (asvh AttachSubnetVpcHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Subnet", "AttachVpc", c, asvh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}
	if !auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole) {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	apiRequest := model.APISubnetAttachVpcRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Subnet attach VPC request data", err)
	}

	subnetID, err := uuid.Parse(c.Param("subnetId"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Subnet ID in URL", nil)
	}
	tenant, err := common.GetTenantForOrg(ctx, nil, asvh.dbSession, org)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}

	subnetDAO := cdbm.NewSubnetDAO(asvh.dbSession)
	subnet, err := subnetDAO.GetByID(ctx, nil, subnetID, []string{cdbm.VpcRelationName})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Subnet to attach to VPC", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Subnet to attach to VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Subnet to attach to VPC", nil)
	}
	if subnet.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for Subnet does not match tenant in org", nil)
	}
	if subnet.ControllerNetworkSegmentID == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet must have a Controller Network Segment ID to attach to a VPC", nil)
	}
	if subnet.Vpc == nil || subnet.Vpc.TenantID != tenant.ID || subnet.Vpc.SiteID != subnet.SiteID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet must belong to a valid VPC at its Site", nil)
	}
	if subnet.Vpc.ControllerVpcID == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet source VPC must have a Controller VPC ID", nil)
	}
	// A nil type is the legacy representation of an Ethernet virtualizer VPC.
	if subnet.Vpc.NetworkVirtualizationType != nil &&
		*subnet.Vpc.NetworkVirtualizationType != cdbm.VpcEthernetVirtualizer &&
		*subnet.Vpc.NetworkVirtualizationType != cdbm.VpcEthernetVirtualizerWithNVUE {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet must belong to an Ethernet virtualizer VPC", nil)
	}

	targetVpc, err := common.GetVpcFromIDString(ctx, nil, apiRequest.VpcID, nil, asvh.dbSession)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find VPC specified in request", nil)
	}
	if targetVpc.TenantID != tenant.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Target VPC does not belong to the Tenant", nil)
	}
	if targetVpc.SiteID != subnet.SiteID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Target VPC and Subnet must belong to the same Site", nil)
	}
	if targetVpc.NetworkVirtualizationType != nil &&
		*targetVpc.NetworkVirtualizationType != cdbm.VpcEthernetVirtualizer &&
		*targetVpc.NetworkVirtualizationType != cdbm.VpcEthernetVirtualizerWithNVUE {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Target VPC must be an Ethernet virtualizer VPC", nil)
	}
	sourceVirtualizationType := cdbm.VpcEthernetVirtualizer
	if subnet.Vpc.NetworkVirtualizationType != nil {
		sourceVirtualizationType = *subnet.Vpc.NetworkVirtualizationType
	}
	targetVirtualizationType := cdbm.VpcEthernetVirtualizer
	if targetVpc.NetworkVirtualizationType != nil {
		targetVirtualizationType = *targetVpc.NetworkVirtualizationType
	}
	if sourceVirtualizationType != targetVirtualizationType {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Target VPC must use the same network virtualization type as the Subnet source VPC", nil)
	}
	if targetVpc.ControllerVpcID == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Target VPC must have a Controller VPC ID", nil)
	}
	_, interfaceCount, err := cdbm.NewInterfaceDAO(asvh.dbSession).GetAll(
		ctx,
		nil,
		cdbm.InterfaceFilterInput{SubnetID: &subnet.ID},
		paginator.PageInput{Limit: cutil.GetPtr(0)},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Interfaces for Subnet")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Interfaces for Subnet", nil)
	}
	if interfaceCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet is being used by one or more Instances and cannot be attached to another VPC", nil)
	}

	stc, err := asvh.scp.GetClientByID(subnet.SiteID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}
	apiRequest.ControllerNetworkSegmentID = *subnet.ControllerNetworkSegmentID
	apiRequest.ControllerVpcID = *targetVpc.ControllerVpcID
	coreResponse := &corev1.NetworkSegment{}
	apiErr := common.ExecuteCoreGRPC(
		ctx,
		stc,
		corev1.Forge_AttachNetworkSegmentToVpc_FullMethodName,
		apiRequest.ToProto(),
		coreResponse,
		subnet.SiteID.String(),
	)
	if apiErr != nil {
		logAPIError(logger, apiErr, "failed to attach Subnet to VPC")
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	if coreResponse.GetId().GetValue() != subnet.ControllerNetworkSegmentID.String() ||
		coreResponse.GetConfig().GetSegmentType() != corev1.NetworkSegmentType_TENANT ||
		coreResponse.GetConfig().GetVpcId().GetValue() != targetVpc.ControllerVpcID.String() {
		logger.Error().Msg("Core returned an unexpected Network Segment after attaching Subnet to VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Core returned an unexpected Subnet attachment response", nil)
	}

	updatedSubnet, err := cdb.WithTxResult(ctx, asvh.dbSession, func(tx *cdb.Tx) (*cdbm.Subnet, error) {
		result, derr := subnetDAO.Update(ctx, tx, cdbm.SubnetUpdateInput{SubnetId: subnet.ID, VpcID: &targetVpc.ID})
		if derr != nil {
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Subnet VPC", nil)
		}
		return result, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Subnet VPC, DB transaction error")
	}
	updatedSubnet.Vpc = targetVpc

	statusDetailDAO := cdbm.NewStatusDetailDAO(asvh.dbSession)
	statusDetails, _, err := statusDetailDAO.GetAll(ctx, nil, cdbm.StatusDetailFilterInput{EntityIDs: []string{subnet.ID.String()}}, paginator.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
	if err != nil {
		logger.Warn().Err(err).Msg("error retrieving Status Details for Subnet after VPC attachment")
		statusDetails = nil
	}

	return c.JSON(http.StatusOK, model.NewAPISubnet(updatedSubnet, statusDetails, nil))
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteSubnetHandler is the API Handler for deleting a Subnet
type DeleteSubnetHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteSubnetHandler initializes and returns a new handler for deleting Subnet
func NewDeleteSubnetHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteSubnetHandler {
	return DeleteSubnetHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an existing Subnet
// @Description Delete an existing Subnet
// @Tags Subnet
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Subnet"
// @Success 202
// @Router /v2/org/{org}/nico/subnet/{id} [delete]
func (dsh DeleteSubnetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Subnet", "Delete", c, dsh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with Subnet endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get subnet ID from URL param
	sStrID := c.Param("id")

	dsh.tracerSpan.SetAttribute(handlerSpan, attribute.String("subnet_id", sStrID), logger)

	sID, err := uuid.Parse(sStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Subnet ID in URL", nil)
	}

	// Check that subnet exists
	sDAO := cdbm.NewSubnetDAO(dsh.dbSession)
	subnet, err := sDAO.GetByID(ctx, nil, sID, []string{"IPv4Block", cdbm.TenantRelationName, cdbm.SiteRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Subnet DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Subnet to delete", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed not retrieve Subnet for deletion, DB error", nil)
	}

	if subnet.Tenant == nil {
		logger.Warn().Err(err).Msg("failed to retrieve Tenant details")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant details", nil)
	}

	// Validate the tenant for which this Subnet is being updated
	if subnet.Tenant.Org != org {
		logger.Warn().Msg("org specified in request does not match org of Tenant associated with Subnet")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org specified in request does not match org of Tenant associated with Subnet", nil)
	}

	// Verify that the Subnet is associated with a site and then that the site is
	// in a valid state.
	if subnet.Site == nil {
		logger.Error().Msg("failed to pull site data for Subnet")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Subnet", nil)
	}

	// Verify if site is ready
	if subnet.Site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Str("Site ID", subnet.SiteID.String()).Msg("Site associated with Subnet must be in Registered state in order to proceed")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site associated with Subnet must be in Registered state in order to proceed", nil)
	}

	// Verify no instances are using the Subnet
	isDAO := cdbm.NewInterfaceDAO(dsh.dbSession)

	filterInput := cdbm.InterfaceFilterInput{
		SubnetID: &subnet.ID,
	}

	pageInput := paginator.PageInput{
		Offset:  nil,
		Limit:   cutil.GetPtr(0),
		OrderBy: nil,
	}

	_, ifcCount, err := isDAO.GetAll(ctx, nil, filterInput, pageInput, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Interfaces for Subnet from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Interfaces for Subnet, DB error", nil)
	}

	if ifcCount > 0 {
		logger.Warn().Msg("Interfaces exist for Subnet, cannot delete it")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Subnet is being used by one or more Instances and cannot be deleted", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(dsh.dbSession)

	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dsh.dbSession, func(tx *cdb.Tx) error {
		// acquire an advisory lock on the parent IP block ID on which there could be contention
		// this lock is released when the transaction commits or rollsback
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(fmt.Sprintf("%s-%s", subnet.TenantID.String(), subnet.IPv4BlockID.String())), nil)
		if derr != nil {
			// TODO: Add a retry here
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on ipblock")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Subnet, DB lock error", nil)
		}

		// Set Subnet status to Deleting
		status := cdbm.SubnetStatusDeleting
		statusMsg := "Subnet deletion successfully initiated on Site"
		_, derr = sDAO.Update(ctx, tx, cdbm.SubnetUpdateInput{SubnetId: subnet.ID, Status: &status})
		if derr != nil {
			logger.Error().Err(derr).Msg("error setting Subnet status to deleting")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Subnet status, DB error", nil)
		}

		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: subnet.ID.String(), Status: status, Message: &statusMsg})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail for Subnet")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Subnet status detail, DB error", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dsh.scp.GetClientByID(subnet.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Prepare the delete/release request workflow object
		deleteSubnetRequest := &corev1.NetworkSegmentDeletionRequest{
			Id: &corev1.NetworkSegmentId{Value: subnet.GetSiteID().String()},
		}

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:        "subnet-delete-" + subnet.ID.String(),
			TaskQueue: queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering Subnet delete workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow to delete subnet Subnet
		// TODO: Once Site Agent offers DeleteSubnetV2 re-registered as SubnetSubnet then update workflow name here
		we, wferr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteSubnetV2", deleteSubnetRequest)
		if wferr != nil {
			logger.Error().Err(wferr).Msg("failed to synchronously start Temporal workflow to delete Subnet")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to delete Subnet on Site: %s", wferr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete Subnet workflow")

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
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to delete Subnet, timeout occurred executing workflow on Site.")
				timeoutCause := wferr
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, timeoutCause, "Subnet", "DeleteSubnetV2")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Subnet delete workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to delete Subnet")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete Subnet on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete Subnet workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete Subnet, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
