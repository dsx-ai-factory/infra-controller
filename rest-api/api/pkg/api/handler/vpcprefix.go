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

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"

	cip "github.com/NVIDIA/infra-controller/rest-api/ipam"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/vpcprefix"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/ipam"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateVpcPrefixHandler is the API Handler for creating new VPC prefix
type CreateVpcPrefixHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateVpcPrefixHandler initializes and returns a new handler for creating VPC prefix
func NewCreateVpcPrefixHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateVpcPrefixHandler {
	return CreateVpcPrefixHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a VPC prefix
// @Description Create a VPC prefix
// @Tags vpcprefix
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIVpcPrefixCreateRequest true "VPC prefix creation request"
// @Success 201 {object} model.APIVpcPrefix
// @Router /v2/org/{org}/nico/vpcprefix [post]
func (csh CreateVpcPrefixHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC prefix", "Create", c, csh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC prefix endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIVpcPrefixCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC prefix creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC prefix creation request data", verr)
	}

	// Validate the tenant for which this VPC prefix is being created
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

	if vpc.NetworkVirtualizationType == nil || *vpc.NetworkVirtualizationType != cdbm.VpcFNN {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("VPC: %v specified in request must have FNN network virtualization type in order to create VPC Prefixes", vpc.ID), nil)
	}

	// Verify if vpc is ready
	if vpc.ControllerVpcID == nil || vpc.Status != cdbm.VpcStatusReady {
		logger.Warn().Msg(fmt.Sprintf("VPC: %v specified in request data must be in Ready state in order to create VPC prefix", apiRequest.VpcID))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC specified in request data must be in Ready state in order to create VPC prefix", nil)
	}

	// Verify if site is ready
	stDAO := cdbm.NewSiteDAO(csh.dbSession)
	site, err := stDAO.GetByID(ctx, nil, vpc.SiteID, nil, false)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find Site associated with VPC prefix", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site with ID from VPC", nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("The Site: %v where the VPC prefix is being created must be in Registered state in order to proceed", vpc.SiteID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where the VPC prefix is being created must be in Registered state in order to proceed", nil)
	}

	// Model validation ensures `IPBlockID` is not nil. Only a Ready tenant IP
	// Block can provide addresses for a new VPC Prefix.
	ipBlockFilter := cdbm.IPBlockFilterInput{Statuses: []string{cdbm.IPBlockStatusReady}}
	ipBlockFilter.TenantAllocated(tenant.ID)
	ipBlock, err := common.GetIPBlockFromIDString(ctx, nil, *apiRequest.IPBlockID, ipBlockFilter, csh.dbSession)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting IP Block in request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not find a Ready tenant IP Block specified by ipBlockId", nil)
	}
	if vpc.SiteID != ipBlock.SiteID {
		logger.Warn().Msg("IP Block specified in request and VPC do not belong to the same Site")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "IP Block specified in request and VPC do not belong to the same Site", nil)
	}

	family := vpcprefix.IPFamily(ipBlock.ProtocolVersion)
	maxPrefixLength, knownFamily := family.MaximumPrefixLength(vpc.SlaacEnabled)
	if !knownFamily {
		logger.Error().Str("protocolVersion", ipBlock.ProtocolVersion).Msg("IP Block has unsupported protocol version")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not determine VPC Prefix length limit", nil)
	}

	verr = apiRequest.ValidatePrefixLength(maxPrefixLength)
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC prefix length for IP Block and VPC")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC prefix creation request data", verr)
	}

	// Check for name uniqueness for the tenant, ie, Tenant cannot have another VPC prefix with same name at the Site
	// TODO consider doing this with an advisory lock for correctness
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(csh.dbSession)
	vps, tot, err := vpcPrefixDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{Names: []string{apiRequest.Name}, SiteIDs: []uuid.UUID{vpc.SiteID}, TenantIDs: []uuid.UUID{tenant.ID}}, cdbp.PageInput{}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant VPC prefix")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create VPC prefix due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", tenant.ID.String()).Str("name", apiRequest.Name).Msg("VPC prefix with same name already exists for tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "A VPC prefix with specified name already exists for Tenant at this Site", validation.Errors{
			"id": errors.New(vps[0].ID.String()),
		})
	}

	sdDAO := cdbm.NewStatusDetailDAO(csh.dbSession)

	var ssd *cdbm.StatusDetail
	var createdVpcPrefix *cdbm.VpcPrefix
	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	err = cdb.WithTx(ctx, csh.dbSession, func(tx *cdb.Tx) error {
		// acquire an advisory lock on the parent IP block ID on which there could be contention
		// this lock is released when the transaction commits or rollsback
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(fmt.Sprintf("%s-%s", tenant.ID.String(), ipBlock.ID.String())), nil)
		if derr != nil {
			// TODO add a retry here
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on ipblock")
			return cutil.NewAPIError(http.StatusInternalServerError, "Error creating VPC prefix, detected multiple parallel request on IP Block by Tenant", nil)
		}

		// create an IPAM allocation for the VPC prefix
		// allocate a child prefix in ipam
		ipamStorage := ipam.NewIpamStorage(csh.dbSession.DB, tx.GetBunTx())
		childPrefix, derr := ipam.CreateChildIpamEntryForIPBlock(ctx, tx, csh.dbSession, ipamStorage, ipBlock, apiRequest.PrefixLength)
		if derr != nil {
			// printing parent prefix usage to debug the child prefix failure
			parentPrefix, serr := ipamStorage.ReadPrefix(ctx, ipBlock.Prefix, ipam.GetIpamNamespaceForIPBlock(ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String()))
			if serr == nil {
				logger.Info().Str("IP Block ID", ipBlock.ID.String()).Str("IP Block Prefix", ipBlock.Prefix).Msgf("%+v\n", parentPrefix.Usage())
			}

			logger.Warn().Err(derr).Msg("failed to create IPAM entry for VPC prefix")
			return cutil.NewAPIError(http.StatusBadRequest, fmt.Sprintf("Could not create IPAM entry for VPC prefix. Details: %s", derr.Error()), nil)
		}
		logger.Info().Str("childCidr", childPrefix.Cidr).Msg("created child cidr for VPC prefix")

		// Create VPC prefix in DB with the initial Core lifecycle state.
		status := cdbm.VpcPrefixStatusProvisioning
		statusMsg := "VPC Prefix is being provisioned on Site"
		vpcPrefix, derr := vpcPrefixDAO.Create(ctx, tx, cdbm.VpcPrefixCreateInput{Name: apiRequest.Name, TenantOrg: org, SiteID: site.ID, VpcID: vpc.ID, TenantID: tenant.ID, IpBlockID: &ipBlock.ID, Prefix: childPrefix.Cidr, PrefixLength: apiRequest.PrefixLength, Status: status, CreatedBy: dbUser.ID})
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create VPC prefix record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating VPC prefix record", nil)
		}

		// create the status detail record
		createdSSD, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpcPrefix.ID.String(), Status: *cutil.GetPtr(status), Message: cutil.GetPtr(statusMsg)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for VPC prefix", nil)
		}
		if createdSSD == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for VPC prefix", nil)
		}
		ssd = createdSSD

		// Get the temporal client for the site we are working with.
		stc, derr := csh.scp.GetClientByID(vpcPrefix.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		createVpcPrefixRequest := apiRequest.ToProto(vpcPrefix, vpc)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "vpc-prefix-create-" + vpcPrefix.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering VPC prefix create workflow")

		// Add context deadlines
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "CreateVpcPrefix", createVpcPrefixRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to create VPC prefix")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to create VPC prefix on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create VPC prefix workflow")

		// Block until the workflow has completed and returned success/error.
		wferr := we.Get(workflowCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to create VPC prefix, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPCPrefix", "Create")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC Prefix create workflow timed out", nil)
			}

			code, uerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uerr).Msg("failed to synchronously execute Temporal workflow to create VPC prefix")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create VPC prefix on Site: %s", uerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create VPC prefix workflow")
		createdVpcPrefix = vpcPrefix
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create VPC Prefix, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// create response
	apiVpcPrefix := model.NewAPIVpcPrefix(createdVpcPrefix, []cdbm.StatusDetail{*ssd}, nil)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiVpcPrefix)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllVpcPrefixHandler is the API Handler for getting all VpcPrefixs
type GetAllVpcPrefixHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllVpcPrefixHandler initializes and returns a new handler for getting all VpcPrefixs
func NewGetAllVpcPrefixHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllVpcPrefixHandler {
	return GetAllVpcPrefixHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all VpcPrefixs
// @Description Get all VpcPrefixs
// @Tags vpcprefix
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "Site ID"
// @Param vpcId query string true "ID of Vpc"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Vpc', 'IPBlock'"
// @Param includeUsageStats query boolean false "IPv4 usage (interface/instance-derived; same shape as IP Block usage)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIVpcPrefix
// @Router /v2/org/{org}/nico/vpcprefix [get]
func (gash GetAllVpcPrefixHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC prefix", "GetAll", c, gash.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC prefix endpoints
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
	err = pageRequest.Validate(cdbm.VpcPrefixOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Validate the tenant for which this VPC prefix is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, gash.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.VpcPrefixRelatedEntities)
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
	if includeUsageStats && !slices.Contains(queryIncludeRelations, cdbm.IPBlockRelationName) {
		queryIncludeRelations = append(queryIncludeRelations, cdbm.IPBlockRelationName)
	}

	// Get site ID from query param
	tsDAO := cdbm.NewTenantSiteDAO(gash.dbSession)
	var siteIDs []uuid.UUID
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gash.dbSession)
		if err != nil {
			logger.Warn().Err(err).Msg("error getting site in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Site specified in query param, invalid ID or DB error", nil)
		}
		siteIDs = append(siteIDs, site.ID)

		// Check Site association with Tenant
		_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
		if err != nil {
			if err == cdb.ErrDoesNotExist {
				return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to this Site", nil)
			}
			logger.Error().Err(err).Msg("error retrieving TenantSite from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to determine Tenant access to Site, DB error", nil)
		}
	}

	// verify vpc if specified in query string
	qVpcID := c.QueryParam("vpcId")
	var vpcIDs []uuid.UUID
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
		vpcIDs = append(vpcIDs, vpc.ID)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		gash.tracerSpan.SetAttribute(handlerSpan, attribute.String("query", *searchQuery), logger)
	}

	// Create response
	vpcPrefixDAO := cdbm.NewVpcPrefixDAO(gash.dbSession)
	vpcPrefixes, total, err := vpcPrefixDAO.GetAll(
		ctx,
		nil,
		cdbm.VpcPrefixFilterInput{
			SiteIDs:     siteIDs,
			VpcIDs:      vpcIDs,
			TenantIDs:   []uuid.UUID{tenant.ID},
			SearchQuery: searchQuery,
		},
		cdbp.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		queryIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error getting VPC Prefixes from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve VPC Prefixes", nil)
	}

	vpusageMap := map[uuid.UUID]*cip.Usage{}
	if includeUsageStats {
		vpcPrefixesForUsage := make([]*cdbm.VpcPrefix, 0, len(vpcPrefixes))
		for i := range vpcPrefixes {
			vp := &vpcPrefixes[i]
			if vp.IPBlock == nil {
				logger.Error().Str("vpcPrefixId", vp.ID.String()).Msg("VPC prefix missing IP Block relation for usage stats")
				continue
			}
			vpcPrefixesForUsage = append(vpcPrefixesForUsage, vp)
		}
		if len(vpcPrefixesForUsage) > 0 {
			prefixUsageMap, serr := vpcPrefixDAO.GetPrefixUsage(ctx, nil, vpcPrefixesForUsage...)
			if serr != nil {
				logger.Error().Err(serr).Msg("error retrieving usage stats for VPC prefixes")
			} else {
				for id, usage := range prefixUsageMap {
					vpusageMap[id] = usage
				}
			}
		}
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gash.dbSession)

	sdEntityIDs := []string{}
	for _, vp := range vpcPrefixes {
		sdEntityIDs = append(sdEntityIDs, vp.ID.String())
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for VPC Prefixes from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for VPC Prefixes", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiVpcPrefixes := []*model.APIVpcPrefix{}

	// get status details
	for _, vp := range vpcPrefixes {
		curvp := vp
		cipu := vpusageMap[vp.ID]
		apiVpcPrefix := model.NewAPIVpcPrefix(&curvp, ssdMap[vp.ID.String()], cipu)
		apiVpcPrefixes = append(apiVpcPrefixes, apiVpcPrefix)
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

	return c.JSON(http.StatusOK, apiVpcPrefixes)
}

// ~~~~~ Get Handler ~~~~~ //

// GetVpcPrefixHandler is the API Handler for retrieving VPC prefix
type GetVpcPrefixHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetVpcPrefixHandler initializes and returns a new handler to retrieve VPC prefix
func NewGetVpcPrefixHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetVpcPrefixHandler {
	return GetVpcPrefixHandler{
		dbSession:  dbSession,
		tc:         tc,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the VPC prefix
// @Description Retrieve the VPC prefix
// @Tags vpcprefix
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of VPC prefix"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Vpc', 'Tenant', 'IPv4Block', 'IPv6Block'"
// @Param includeUsageStats query boolean false "IPv4 usage (interface/instance-derived; same shape as IP Block usage)"
// @Success 200 {object} model.APIVpcPrefix
// @Router /v2/org/{org}/nico/vpcprefix/{id} [get]
func (gsh GetVpcPrefixHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC prefix", "Get", c, gsh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC prefix endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.VpcPrefixRelatedEntities)
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
	if includeUsageStats && !slices.Contains(queryIncludeRelations, cdbm.IPBlockRelationName) {
		queryIncludeRelations = append(queryIncludeRelations, cdbm.IPBlockRelationName)
	}

	// Get VPC prefix ID from URL param
	sStrID := c.Param("id")

	gsh.tracerSpan.SetAttribute(handlerSpan, attribute.String("VpcPrefixId", sStrID), logger)

	sID, err := uuid.Parse(sStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC prefix ID in URL", nil)
	}

	vpDAO := cdbm.NewVpcPrefixDAO(gsh.dbSession)

	// Validate the tenant for which this VPC prefix is being updated
	tenant, err := common.GetTenantForOrg(ctx, nil, gsh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}

	// Check that VPC prefix exists
	vpcPrefix, err := vpDAO.GetByID(ctx, nil, sID, queryIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC prefix DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve VPC prefix to update", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve VPC prefix to update", nil)
	}

	// verify tenant matches
	if tenant.ID != vpcPrefix.TenantID {
		logger.Warn().Msg("tenant in VPC prefix does not belong to tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for VPC prefix in request does not match tenant in org", nil)
	}

	// get status details for the response
	sdDAO := cdbm.NewStatusDetailDAO(gsh.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{vpcPrefix.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for VPC prefix from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for VPC prefix", nil)
	}

	var vpusage *cip.Usage
	if includeUsageStats {
		if vpcPrefix.IPBlock == nil {
			logger.Error().Str("vpcPrefixId", vpcPrefix.ID.String()).Msg("VPC prefix missing IP Block relation for usage stats")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats for VPC prefix", nil)
		}
		prefixUsageMap, err := vpDAO.GetPrefixUsage(ctx, nil, vpcPrefix)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving usage stats for VPC prefix")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats for VPC prefix", nil)
		}
		var ok bool
		vpusage, ok = prefixUsageMap[vpcPrefix.ID]
		// REST usage stats model IPv4 Interface allocations. IPv6 prefixes
		// intentionally have no usage entry and remain valid responses.
		if !ok && vpcPrefix.IPBlock.ProtocolVersion != cdbm.IPBlockProtocolVersionV6 {
			logger.Error().Str("vpcPrefixId", vpcPrefix.ID.String()).Msg("IPv4 VPC prefix missing usage stats")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Usage Stats for VPC prefix", nil)
		}
	}

	// Send response
	apiVpcPrefix := model.NewAPIVpcPrefix(vpcPrefix, ssds, vpusage)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiVpcPrefix)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateVpcPrefixHandler is the API Handler for updating a VPC prefix
type UpdateVpcPrefixHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateVpcPrefixHandler initializes and returns a new handler for updating VPC prefix
func NewUpdateVpcPrefixHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateVpcPrefixHandler {
	return UpdateVpcPrefixHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing VPC prefix
// @Description Update an existing VPC prefix
// @Tags vpcprefix
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of VPC prefix"
// @Param message body model.APIVpcPrefixUpdateRequest true "VPC prefix update request"
// @Success 200 {object} model.APIVpcPrefix
// @Router /v2/org/{org}/nico/vpcprefix/{id} [patch]
func (ush UpdateVpcPrefixHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC prefix", "Update", c, ush.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC prefix endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get VPC prefix ID from URL param
	sStrID := c.Param("id")

	ush.tracerSpan.SetAttribute(handlerSpan, attribute.String("VpcPrefixId", sStrID), logger)

	sID, err := uuid.Parse(sStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC prefix ID in URL", nil)
	}

	vpDAO := cdbm.NewVpcPrefixDAO(ush.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIVpcPrefixUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating VPC prefix update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating VPC prefix update request data", verr)
	}

	// Validate the tenant for which this VPC prefix is being updated
	tenant, err := common.GetTenantForOrg(ctx, nil, ush.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting tenant from org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Tenant from org", nil)
	}

	// Check that VPC prefix exists
	vpcPrefix, err := vpDAO.GetByID(ctx, nil, sID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC prefix DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve VPC prefix to update", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve VPC prefix to update", nil)
	}

	// verify tenant matches
	if tenant.ID != vpcPrefix.TenantID {
		logger.Warn().Msg("tenant in VPC prefix does not belong to tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for VPC prefix in request does not match tenant in org", nil)
	}

	if apiRequest.Name != nil && *apiRequest.Name != vpcPrefix.Name {
		vps, tot, serr := vpDAO.GetAll(ctx, nil, cdbm.VpcPrefixFilterInput{Names: []string{*apiRequest.Name}, SiteIDs: []uuid.UUID{vpcPrefix.SiteID}, TenantIDs: []uuid.UUID{vpcPrefix.TenantID}}, cdbp.PageInput{}, nil)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant VPC prefix")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create VPC prefix due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another VPC prefix with specified name already exists for Tenant", validation.Errors{
				"id": fmt.Errorf("%v", vps[0].ID.String()),
			})
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(ush.dbSession)

	var ssds []cdbm.StatusDetail
	var updatedVpcPrefix *cdbm.VpcPrefix
	// timeoutResp lets the closure signal a post-rollback handler — the
	// TerminateWorkflow call has to run after the closure returns so that
	// the DB tx unwinds before we make the second remote call. nil means
	// no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	err = cdb.WithTx(ctx, ush.dbSession, func(tx *cdb.Tx) error {
		updated, derr := vpDAO.Update(ctx, tx, cdbm.VpcPrefixUpdateInput{VpcPrefixID: vpcPrefix.ID, Name: apiRequest.Name})
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating VPC prefix in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update VPC prefix", nil)
		}

		// get status details for the response
		fetchedSSDs, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{updated.ID.String()}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for VPC prefix from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for VPC prefix", nil)
		}
		ssds = fetchedSSDs

		// Get the temporal client for the site we are working with.
		stc, derr := ush.scp.GetClientByID(updated.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		updateVpcPrefixRequest := apiRequest.ToProto(updated)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "vpc-prefix-update-" + updated.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering VPC prefix update workflow")

		// Add context deadlines
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "UpdateVpcPrefix", updateVpcPrefixRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to update VPC prefix")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update VPC prefix on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update VPC prefix workflow")

		// Block until the workflow has completed and returned success/error.
		wferr := we.Get(workflowCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to update VPC Prefix, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPCPrefix", "Update")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC Prefix update workflow timed out", nil)
			}

			code, uerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uerr).Msg("failed to synchronously execute Temporal workflow to update VPC prefix")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update VPC prefix on Site: %s", uerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update VPC prefix workflow")
		updatedVpcPrefix = updated
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update VPC prefix, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Send response
	apiVpcPrefix := model.NewAPIVpcPrefix(updatedVpcPrefix, ssds, nil)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiVpcPrefix)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteVpcPrefixHandler is the API Handler for deleting a VPC prefix
type DeleteVpcPrefixHandler struct {
	dbSession  *cdb.Session
	tc         temporalClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteVpcPrefixHandler initializes and returns a new handler for deleting VPC prefix
func NewDeleteVpcPrefixHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteVpcPrefixHandler {
	return DeleteVpcPrefixHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an existing VPC prefix
// @Description Delete an existing VPC prefix
// @Tags vpcprefix
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of VPC prefix"
// @Success 202
// @Router /v2/org/{org}/nico/vpcprefix/{id} [delete]
func (dsh DeleteVpcPrefixHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("VPC prefix", "Delete", c, dsh.tracerSpan)
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

	// Validate role, only Tenant Admins are allowed to interact with VPC prefix endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get VPC prefix ID from URL param
	sStrID := c.Param("id")

	dsh.tracerSpan.SetAttribute(handlerSpan, attribute.String("VpcPrefixId", sStrID), logger)

	sID, err := uuid.Parse(sStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid VPC prefix ID in URL", nil)
	}

	// Check that VPC prefix exists
	vpDAO := cdbm.NewVpcPrefixDAO(dsh.dbSession)
	vpcPrefix, err := vpDAO.GetByID(ctx, nil, sID, []string{cdbm.IPBlockRelationName, cdbm.TenantRelationName, cdbm.SiteRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving VPC prefix DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find VPC prefix to delete", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed not retrieve VPC prefix for deletion, DB error", nil)
	}

	if vpcPrefix.Tenant == nil {
		logger.Warn().Err(err).Msg("failed to retrieve Tenant details")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant details", nil)
	}

	// Validate the tenant for which this VPC prefix is being updated
	if vpcPrefix.Tenant.Org != org {
		logger.Warn().Msg("org specified in request does not match org of Tenant associated with VPC prefix")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org specified in request does not match org of Tenant associated with VPC prefix", nil)
	}

	// Verify that the VPC prefix is associated with a site and then that the site is
	// in a valid state.
	if vpcPrefix.Site == nil {
		logger.Error().Msg("failed to pull site data for VPC prefix")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for VPC prefix", nil)
	}

	// Verify if site is ready
	if vpcPrefix.Site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Str("Site ID", vpcPrefix.SiteID.String()).Msg("Site associated with VPC prefix must be in Registered state in order to proceed")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site associated with VPC prefix must be in Registered state in order to proceed", nil)
	}

	// Verify no instances are using the VPC prefix
	ifcDAO := cdbm.NewInterfaceDAO(dsh.dbSession)

	_, ifcCount, err := ifcDAO.GetAll(ctx, nil, cdbm.InterfaceFilterInput{VpcPrefixID: &vpcPrefix.ID}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Interfaces for VPC prefix from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve check for active Interfaces using VPC Prefix, DB error", nil)
	}

	if ifcCount > 0 {
		logger.Warn().Msg("could not delete VPC Prefix, one or more Instance Interfaces are using it")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "VPC Prefix is being used by one or more Instances and cannot be deleted", nil)
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
		derr := tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(vpcPrefix.IPBlockID.String()), nil)
		if derr != nil {
			// TODO: Add a retry here
			logger.Error().Err(derr).Msg("Failed to acquire advisory lock on ipblock")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete VPC Prefix, DB lock error", nil)
		}

		// Set VPC prefix status to Deleting
		status := cdbm.VpcPrefixStatusDeleting
		statusMsg := "VPC prefix deletion successfully initiated on Site"
		_, derr = vpDAO.Update(ctx, tx, cdbm.VpcPrefixUpdateInput{VpcPrefixID: vpcPrefix.ID, Status: &status})
		if derr != nil {
			logger.Error().Err(derr).Msg("error setting VPC prefix status to deleting")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update VPC prefix status, DB error", nil)
		}

		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: vpcPrefix.ID.String(), Status: status, Message: &statusMsg})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail for VPC prefix")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create VPC prefix status detail, DB error", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dsh.scp.GetClientByID(vpcPrefix.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Prepare the delete/release request workflow object
		deleteVpcPrefixRequest := vpcPrefix.ToDeletionRequestProto()

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:        "vpc-prefix-delete-" + vpcPrefix.ID.String(),
			TaskQueue: queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering VPC prefix delete workflow")

		// Trigger Site workflow to delete VPC prefix VPC prefix
		we, derr := stc.ExecuteWorkflow(ctx, workflowOptions, "DeleteVpcPrefix", deleteVpcPrefixRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to delete VPC prefix")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to delete VPC prefix on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete VPC prefix workflow")

		// Execute the workflow synchronously
		wferr := we.Get(ctx, nil)
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

		// Check if wferr is still nil now that we've handled any skippable errors.
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || ctx.Err() != nil {
				logger.Error().Err(wferr).Msg("failed to delete VPC Prefix, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "VPCPrefix", "Delete")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "VPC Prefix delete workflow timed out", nil)
			}

			code, uerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uerr).Msg("failed to delete VPC Prefix, timeout occurred executing workflow on Site.")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete VPC prefix on Site: %s", uerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete VPC prefix workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete VPC Prefix, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
