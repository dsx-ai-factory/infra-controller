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

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	common "github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateNetworkSecurityGroupHandler is the API Handler for creating a new NetworkSecurityGroup
type CreateNetworkSecurityGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateNetworkSecurityGroupHandler initializes and returns a new handler for creating NetworkSecurityGroup
func NewCreateNetworkSecurityGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateNetworkSecurityGroupHandler {
	return CreateNetworkSecurityGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create a NetworkSecurityGroup
// @Description Create a NetworkSecurityGroup
// @Tags NetworkSecurityGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APINetworkSecurityGroupCreateRequest true "NetworkSecurityGroup creation request"
// @Success 201 {object} model.APINetworkSecurityGroup
// @Router /v2/org/{org}/nico/network-security-group [post]
func (cnsgh CreateNetworkSecurityGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NetworkSecurityGroup", "Create", c)
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

	// Validate role, only Tenant Admins are allowed to create NetworkSecurityGroups
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APINetworkSecurityGroupCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Error().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate the tenant for which this NetworkSecurityGroup is being created
	// The api validation ensures non-nil tenantID in request

	tenant, err := common.GetTenantForOrg(ctx, nil, cnsgh.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Error().Err(err).Msg("Tenant not found for org in request")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant not found for org in request", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cnsgh.dbSession)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where this Network Security Group is being created could not be found", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "SiteID in request is not valid", nil)
	}

	// Ensure that Tenant has access to Site
	tsDAO := cdbm.NewTenantSiteDAO(cnsgh.dbSession)
	_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, tenant.ID, site.ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have access to Site, Network Security Group cannot be created", nil)
		}

		logger.Error().Err(err).Msg("error retrieving Tenant Site association")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant Site association", nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("The Site: %v where this NetworkSecurityGroup is being created is not in Registered state", site.ID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "The Site where this Network Security Group is being created is not in Registered state", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.NetworkSecurityGroup {
		logger.Warn().Msg("site does not have NetworkSecurityGroup capability")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NetworkSecurityGroup capability", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate(siteConfig)
	if verr != nil {
		logger.Error().Err(verr).Msg("error validating NetworkSecurityGroup creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Network Security Group creation request data", verr)
	}

	// Get our DB DAO object ready.
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(cnsgh.dbSession)

	var networkSecurityGroupID *string
	if apiRequest.ID != nil {
		requestedNetworkSecurityGroupID := apiRequest.ID.String()
		networkSecurityGroupID = &requestedNetworkSecurityGroupID

		_, total, err := nsgDAO.GetAll(ctx, nil, cdbm.NetworkSecurityGroupFilterInput{NetworkSecurityGroupIDs: []string{requestedNetworkSecurityGroupID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.DefaultLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error checking for NetworkSecurityGroup ID uniqueness")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check for existing Network Security Group", nil)
		}
		if total > 0 {
			logger.Warn().Str("tenantId", tenant.ID.String()).Str("id", requestedNetworkSecurityGroupID).Msg("network security group with same ID already exists")
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "A Network Security Group with specified ID already exists", validation.Errors{
				"id": errors.New(requestedNetworkSecurityGroupID),
			})
		}
	}

	// Check if an NSG already exists for the given name and Site ID
	// Another case where we might want to leave this to NICo
	// and simply return the error and map the response code from
	// the sync call to the appropriate http status code.
	nsgs, tot, err := nsgDAO.GetAll(ctx, nil, cdbm.NetworkSecurityGroupFilterInput{Name: &apiRequest.Name, TenantIDs: []uuid.UUID{tenant.ID}, SiteIDs: []uuid.UUID{site.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error checking for existing NetworkSecurityGroup")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check for existing Network Security Group", nil)
	}
	if tot > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, fmt.Sprintf("Network Security Group with name: %s for Site: %s already exists", apiRequest.Name, apiRequest.SiteID), validation.Errors{
			"id": errors.New(nsgs[0].ID),
		})
	}

	// Convert all the request rules into rules
	// we can store and send to NICo. Per-rule validation has already
	// run inside apiRequest.Validate, so rule.ToProto is a pure mapper
	// here; we only check for cross-rule constraints (duplicate names).
	rules := make([]*cdbm.NetworkSecurityGroupRule, len(apiRequest.Rules))

	names := map[string]bool{}

	for i, rule := range apiRequest.Rules {
		if rule.Name != nil {
			if names[*rule.Name] {
				logger.Error().Str("name", *rule.Name).Msg("duplicate rule name in request")
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Duplicate rule name `%s` in request", *rule.Name), nil)
			}
			names[*rule.Name] = true
		}

		rules[i] = &cdbm.NetworkSecurityGroupRule{NetworkSecurityGroupRuleAttributes: rule.ToProto()}
	}

	sdDAO := cdbm.NewStatusDetailDAO(cnsgh.dbSession)

	// timeoutResp lets the closure signal an outer-scope handler — TerminateWorkflowOnTimeOut
	// has to run after the closure returns so that the rollback completes before we touch
	// the workflow again. nil means no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var ssd *cdbm.StatusDetail
	var networkSecurityGroup *cdbm.NetworkSecurityGroup

	err = cdb.WithTx(ctx, cnsgh.dbSession, func(tx *cdb.Tx) error {
		// create the NetworkSecurityGroup record in the db
		nsg, derr := nsgDAO.Create(ctx, tx,
			cdbm.NetworkSecurityGroupCreateInput{
				Name:                   apiRequest.Name,
				Description:            apiRequest.Description,
				TenantID:               tenant.ID,
				TenantOrg:              tenant.Org,
				SiteID:                 site.ID,
				NetworkSecurityGroupID: networkSecurityGroupID,
				StatefulEgress:         apiRequest.StatefulEgress,
				Rules:                  rules,
				Labels:                 apiRequest.Labels,
				Status:                 cdbm.NetworkSecurityGroupStatusPending,
				CreatedByID:            dbUser.ID,
			},
		)
		if derr != nil {
			if networkSecurityGroupID != nil && (&cdb.PostgresErrorChecker{}).IsUniqueConstraintError(derr) {
				logger.Warn().Err(derr).Str("id", *networkSecurityGroupID).Msg("network security group with specified ID already exists")
				return cutil.NewAPIError(http.StatusConflict, "A Network Security Group with specified ID already exists", validation.Errors{
					"id": errors.New(*networkSecurityGroupID),
				})
			}
			logger.Error().Err(derr).Msg("unable to create NetworkSecurityGroup record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating Network Security Group record, DB error", nil)
		}

		// create the status detail record
		statusDetail, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: nsg.ID, Status: *cutil.GetPtr(cdbm.NetworkSecurityGroupStatusReady), Message: cutil.GetPtr("processed network security group creation request")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Network Security Group, DB error", nil)
		}
		if statusDetail == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for Network Security Group", nil)
		}
		ssd = statusDetail

		// Get the temporal client for the site we are working with.
		stc, derr := cnsgh.scp.GetClientByID(nsg.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// The DB record is the canonical source for Metadata and the
		// assigned ID; request-shape fields (rules, statefulEgress)
		// come from the validated apiRequest.
		createNetworkSecurityGroupRequest := apiRequest.ToProto(nsg)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "network-security-group-create-" + nsg.ID,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		logger.Info().Msg("triggering network security group create workflow")

		// Add context deadlines
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow to update networkSecurityGroup
		we, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "CreateNetworkSecurityGroup", createNetworkSecurityGroupRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to create NetworkSecurityGroup")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to create Network Security Group on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create NetworkSecurityGroup workflow")

		// Execute the workflow synchronously
		wferr := we.Get(workflowCtx, nil)
		if wferr != nil {
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
				logger.Error().Msg("feature not yet implemented on target Site")
				return cutil.NewAPIError(http.StatusNotImplemented, fmt.Sprintf("Feature not yet implemented on target Site: %s", wferr), nil)
			}

			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "NetworkSecurityGroup", "CreateNetworkSecurityGroup")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Network Security Group create workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to update CreateNetworkSecurityGroup")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create Network Security Group on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create NetworkSecurityGroup workflow")
		networkSecurityGroup = nsg
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create Network Security Group, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	apiNetworkSecurityGroup := model.NewAPINetworkSecurityGroup(networkSecurityGroup, []cdbm.StatusDetail{*ssd})

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiNetworkSecurityGroup)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllNetworkSecurityGroupHandler is the API Handler for getting all NetworkSecurityGroups
type GetAllNetworkSecurityGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllNetworkSecurityGroupHandler initializes and returns a new handler for getting all NetworkSecurityGroups
func NewGetAllNetworkSecurityGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllNetworkSecurityGroupHandler {
	return GetAllNetworkSecurityGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all NetworkSecurityGroups
// @Description Get all NetworkSecurityGroups for a given Site
// @Tags networksecuritygroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param status query string false "Query input for status"
// @Param query query string false "Query input for full text search"
// @Param includeAttachmentStats query boolean false "Attachment stats to include in response"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Tenant', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APINetworkSecurityGroup
// @Router /v2/org/{org}/nico/network-security-group [get]
func (gansgh GetAllNetworkSecurityGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NetworkSecurityGroup", "GetAll", c)
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

	// Validate paginantion request
	// Bind request data to API model
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Error().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate paginantion request attributes
	err = pageRequest.Validate(cdbm.NetworkSecurityGroupOrderByFields)
	if err != nil {
		logger.Error().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Validate role.  Only Tenant Admins are allowed to interact with NetworkSecurityGroup endpoints.
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	var siteID *uuid.UUID
	qstID := c.QueryParam("siteId")
	if qstID != "" {
		stID, err := uuid.Parse(qstID)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Site ID in query", nil)
		}
		siteID = &stID

		// Check site for existence
		stDAO := cdbm.NewSiteDAO(gansgh.dbSession)
		_, err = stDAO.GetByID(ctx, nil, *siteID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Could not retrieve Site with ID specified in query", nil)
		}
	}

	// Check `includeAttachmentStats` in query
	includeAttachmentStats := false
	qoa := c.QueryParam("includeAttachmentStats")
	if qoa != "" {
		includeAttachmentStats, err = strconv.ParseBool(qoa)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeAttachmentStats` query param", nil)
		}
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	var statuses []string

	// Get status from query param
	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		_, ok := cdbm.NetworkSecurityGroupStatusMap[statusQuery]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = []string{statusQuery}
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.NetworkSecurityGroupRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get all NetworkSecurityGroups
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(gansgh.dbSession)

	var siteIDs []uuid.UUID
	if siteID != nil {
		siteIDs = []uuid.UUID{*siteID}
	}

	// We can simply filter on org because we've already determined that the caller
	// is a tenant with the right role/permission, and we are allowed to assume
	// 1:1  for tenant:org

	filter := cdbm.NetworkSecurityGroupFilterInput{SiteIDs: siteIDs, TenantOrgs: []string{org}, Statuses: statuses, SearchQuery: searchQuery}

	nsgs, total, err := nsgDAO.GetAll(ctx, nil, filter, cdbp.PageInput{Offset: pageRequest.Offset, Limit: pageRequest.Limit, OrderBy: pageRequest.OrderBy}, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroups for Site specified in query")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Network Security Groups for Site in query", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gansgh.dbSession)

	sdEntityIDs := []string{}
	for _, it := range nsgs {
		sdEntityIDs = append(sdEntityIDs, it.ID)
	}
	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Status Details for NetworkSecurityGroups from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Network Security Groups", nil)
	}
	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	itIDs := []string{}
	for _, it := range nsgs {
		itIDs = append(itIDs, it.ID)
	}

	statsMap := map[string]*model.APINetworkSecurityGroupStats{}

	if includeAttachmentStats {

		insDAO := cdbm.NewInstanceDAO(gansgh.dbSession)
		vpcDAO := cdbm.NewVpcDAO(gansgh.dbSession)

		instances, _, err := insDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{NetworkSecurityGroupIDs: itIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve related Instances for Network Security Groups", nil)
		}

		vpcs, _, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{NetworkSecurityGroupIDs: itIDs}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve related VPCs for Network Security Groups", nil)
		}

		// Loop through the instances and vpcs and fill in the statsMap

		for _, instance := range instances {

			if statsMap[*instance.NetworkSecurityGroupID] == nil {
				statsMap[*instance.NetworkSecurityGroupID] = &model.APINetworkSecurityGroupStats{}
			}

			nsgStats := statsMap[*instance.NetworkSecurityGroupID]
			nsgStats.InUse = true

			nsgStats.InstanceAttachmentCount++
			nsgStats.TotalAttachmentCount++
		}

		for _, vpc := range vpcs {

			if statsMap[*vpc.NetworkSecurityGroupID] == nil {
				statsMap[*vpc.NetworkSecurityGroupID] = &model.APINetworkSecurityGroupStats{}
			}

			nsgStats := statsMap[*vpc.NetworkSecurityGroupID]
			nsgStats.InUse = true

			nsgStats.VpcAttachmentCount++
			nsgStats.TotalAttachmentCount++
		}
	}

	// Create response
	aits := make([]*model.APINetworkSecurityGroup, len(nsgs))

	// Loop through the NSGs, create the API response, and attach the statsMap data.
	for i, nsg := range nsgs {
		apiNSG := model.NewAPINetworkSecurityGroup(&nsg, ssdMap[nsg.ID])

		aits[i] = apiNSG
		apiNSG.AttachmentStats = statsMap[nsg.ID]

		// If the caller requested attachment stats but the NSG is not
		// actually attached to anything, then the earlier loop wouldn't have
		// initialized an APINetworkSecurityGroupStats for the response,
		// so we can do that now so that the caller still gets the attachment stats
		// they requested, even though there are no attachments.
		if includeAttachmentStats && apiNSG.AttachmentStats == nil {
			apiNSG.AttachmentStats = &model.APINetworkSecurityGroupStats{
				VpcAttachmentCount:      0,
				InstanceAttachmentCount: 0,
				TotalAttachmentCount:    0,
				InUse:                   false,
			}
		}
	}

	// Create pagination response header
	pageReponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageReponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	// Create response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, aits)
}

// ~~~~~ Get Handler ~~~~~ //

// GetAllNetworkSecurityGroupHandler is the API Handler for getting a NetworkSecurityGroup
type GetNetworkSecurityGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllNetworkSecurityGroupHandler initializes and returns a new handler for getting all NetworkSecurityGroups
func NewGetNetworkSecurityGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetNetworkSecurityGroupHandler {
	return GetNetworkSecurityGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get a NetworkSecurityGroup
// @Description Get a NetworkSecurityGroup for a given Tenant
// @Tags networksecuritygroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param includeAttachmentStats query boolean false "Attachment stats to include in response"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Tenant', 'Site'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APINetworkSecurityGroup
// @Router /v2/org/{org}/nico/network-security-group [get]
func (gansgh GetNetworkSecurityGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NetworkSecurityGroup", "Get", c)
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

	// Validate paginantion request
	// Bind request data to API model
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Error().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate paginantion request attributes
	err = pageRequest.Validate(cdbm.NetworkSecurityGroupOrderByFields)
	if err != nil {
		logger.Error().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Validate role.  Only Tenant Admins are allowed to interact with NetworkSecurityGroup endpoints.
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Check `includeAttachmentStats` in query
	includeAttachmentStats := false
	qoa := c.QueryParam("includeAttachmentStats")
	if qoa != "" {
		includeAttachmentStats, err = strconv.ParseBool(qoa)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid value specified for `includeAttachmentStats` query param", nil)
		}
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.NetworkSecurityGroupRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get NSG ID from path
	nsgID := c.Param("id")

	// Get all NetworkSecurityGroups
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(gansgh.dbSession)

	nsg, err := nsgDAO.GetByID(ctx, nil, nsgID, qIncludeRelations)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			logger.Error().Err(err).Msg("NetworkSecurityGroup in request not found")
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Network Security Group in request not found", nil)
		}

		logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroup for Site specified in query")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Network Security Group for Site in query", nil)
	}

	if nsg.TenantOrg != org {
		logger.Error().Err(err).Msg("org specified in request does not match org of Tenant associated with NetworkSecurityGroup")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Org specified in request does not match org of Tenant associated with Network Security Group", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gansgh.dbSession)

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{nsgID}, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Status Details for NetworkSecurityGroup from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for Network Security Group", nil)
	}

	// Create response
	apiNetworkSecurityGroup := model.NewAPINetworkSecurityGroup(nsg, ssds)

	// Attach stats if requested
	if includeAttachmentStats {

		insDAO := cdbm.NewInstanceDAO(gansgh.dbSession)
		vpcDAO := cdbm.NewVpcDAO(gansgh.dbSession)

		_, instanceCount, err := insDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{NetworkSecurityGroupIDs: []string{nsgID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve related Instances for Network Security Group", nil)
		}

		_, vpcCount, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{NetworkSecurityGroupIDs: []string{nsgID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
		if err != nil {
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve related VPCs for Network Security Group", nil)
		}
		apiNetworkSecurityGroup.AttachmentStats = &model.APINetworkSecurityGroupStats{
			VpcAttachmentCount:      vpcCount,
			InstanceAttachmentCount: instanceCount,
			TotalAttachmentCount:    instanceCount + vpcCount,
			InUse:                   (instanceCount + vpcCount) > 0,
		}

	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiNetworkSecurityGroup)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteNetworkSecurityGroupHandler is the API Handler for deleting a new NetworkSecurityGroup
type DeleteNetworkSecurityGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteNetworkSecurityGroupHandler initializes and returns a new handler for creating NetworkSecurityGroup
func NewDeleteNetworkSecurityGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteNetworkSecurityGroupHandler {
	return DeleteNetworkSecurityGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete a NetworkSecurityGroup
// @Description Delete a NetworkSecurityGroup
// @Tags NetworkSecurityGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of NetworkSecurityGroup"
// @Success 202 {object} model.APINetworkSecurityGroup
// @Router /v2/org/{org}/nico/network-security-group [post]
func (dnsgh DeleteNetworkSecurityGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NetworkSecurityGroup", "Delete", c)
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

	// Validate role, only Tenant Admins are allowed to interact with NetworkSecurityGroup endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get NetworkSecurityGroup ID from URL param
	nsgID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("networksecuritygroup_id", nsgID))

	// Get NSG from DB
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(dnsgh.dbSession)
	nsg, err := nsgDAO.GetByID(ctx, nil, nsgID, []string{
		cdbm.SiteRelationName,
	})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Network Security Group with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroup from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Network Security Group with specified ID", nil)
	}

	// Validate the tenant for which this NetworkSecurityGroup is being deleted
	if nsg.TenantOrg != org {
		logger.Warn().Msg("org specified in request does not match org of Tenant associated with NetworkSecurityGroup")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org specified in request does not match org of Tenant associated with Network Security Group", nil)
	}

	// Check if any objects are using the NetworkSecurityGroup
	// NOTE: We don't really _need_ to do this here.  NICo
	// already performs all of these checks, so we could skip
	// this here and defer to the sites.

	insDAO := cdbm.NewInstanceDAO(dnsgh.dbSession)
	vpcDAO := cdbm.NewVpcDAO(dnsgh.dbSession)

	_, instanceCount, err := insDAO.GetAll(ctx, nil, cdbm.InstanceFilterInput{NetworkSecurityGroupIDs: []string{nsgID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve related Instances for Network Security Group", nil)
	}

	if instanceCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Cannot delete NetworkSecurityGroup, one or more Instances have attached this Network Security Group", nil)
	}

	_, vpcCount, err := vpcDAO.GetAll(ctx, nil, cdbm.VpcFilterInput{NetworkSecurityGroupIDs: []string{nsgID}}, cdbp.PageInput{Limit: cutil.GetPtr(0)}, nil)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve related VPCs for Network Security Group", nil)
	}

	if vpcCount > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Cannot delete NetworkSecurityGroup, one or more VPCs have attached this Network Security Group", nil)
	}

	sdDAO := cdbm.NewStatusDetailDAO(dnsgh.dbSession)

	// timeoutResp lets the closure signal an outer-scope handler — TerminateWorkflowOnTimeOut
	// has to run after the closure returns so that the rollback completes before we touch
	// the workflow again. nil means no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dnsgh.dbSession, func(tx *cdb.Tx) error {
		// Update NetworkSecurityGroup to set status to Deleting
		unsgInput := cdbm.NetworkSecurityGroupUpdateInput{
			NetworkSecurityGroupID: nsg.ID,
			Status:                 cutil.GetPtr(cdbm.NetworkSecurityGroupStatusDeleting),
		}
		_, derr := nsgDAO.Update(ctx, tx, unsgInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating NetworkSecurityGroup in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Network Security Group", nil)
		}

		// Delete NetworkSecurityGroup
		dnsgInput := cdbm.NetworkSecurityGroupDeleteInput{
			NetworkSecurityGroupID: nsg.ID,
			UpdatedByID:            dbUser.ID,
		}

		derr = nsgDAO.Delete(ctx, tx, dnsgInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error deleting NetworkSecurityGroup in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Network Security Group", nil)
		}

		// Create status detail
		_, derr = sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: nsg.ID, Status: *cutil.GetPtr(cdbm.NetworkSecurityGroupStatusDeleting), Message: cutil.GetPtr("received request for deletion, pending processing")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for Network Security Group", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dnsgh.scp.GetClientByID(nsg.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		deleteNetworkSecurityGroupRequest := nsg.ToDeletionRequestProto()

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "network-security-group-delete-" + nsg.ID,
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering NetworkSecurityGroup delete workflow")

		// Add context deadlines
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "DeleteNetworkSecurityGroup", deleteNetworkSecurityGroupRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to delete NetworkSecurityGroup")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to delete Network Security Group on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete NetworkSecurityGroup workflow")

		// Execute the workflow synchronously
		wferr := we.Get(workflowCtx, nil)
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
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
				logger.Error().Msg("feature not yet implemented on target Site")
				return cutil.NewAPIError(http.StatusNotImplemented, fmt.Sprintf("Feature not yet implemented on target Site: %s", wferr), nil)
			}

			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "NetworkSecurityGroup", "DeleteNetworkSecurityGroup")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Network Security Group delete workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to delete NetworkSecurityGroup")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete Network Security Group on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete NetworkSecurityGroup workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete Network Security Group, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Return response
	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteNetworkSecurityGroupHandler is the API Handler for deleting a new NetworkSecurityGroup
type UpdateNetworkSecurityGroupHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteNetworkSecurityGroupHandler initializes and returns a new handler for creating NetworkSecurityGroup
func NewUpdateNetworkSecurityGroupHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateNetworkSecurityGroupHandler {
	return UpdateNetworkSecurityGroupHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update a NetworkSecurityGroup
// @Description Update a NetworkSecurityGroup
// @Tags NetworkSecurityGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APINetworkSecurityGroupUpdateRequest true "NetworkSecurityGroup update request"
// @Success 200 {object} model.APINetworkSecurityGroup
// @Router /v2/org/{org}/nico/network-security-group [post]
func (dnsgh UpdateNetworkSecurityGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NetworkSecurityGroup", "Update", c)
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

	// Validate role, only Tenant Admins are allowed to interact with NetworkSecurityGroup endpoints
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role with org, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get NetworkSecurityGroup ID from URL param
	nsgID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("networksecuritygroup_id", nsgID))

	// Validate request
	// Bind request data to API model
	apiRequest := model.APINetworkSecurityGroupUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Error().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Get NSG from DB
	nsgDAO := cdbm.NewNetworkSecurityGroupDAO(dnsgh.dbSession)
	nsg, err := nsgDAO.GetByID(ctx, nil, nsgID, []string{
		cdbm.SiteRelationName,
	})
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find Network Security Group with specified ID", nil)
		}
		logger.Error().Err(err).Msg("error retrieving NetworkSecurityGroup from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Network Security Group with specified ID", nil)
	}

	// Validate the tenant for which this NetworkSecurityGroup is being deleted
	if nsg.TenantOrg != org {
		logger.Warn().Msg("org specified in request does not match org of Tenant associated with NetworkSecurityGroup")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org specified in request does not match org of Tenant associated with Network Security Group", nil)
	}

	// Get any site-specific config
	stDAO := cdbm.NewSiteDAO(dnsgh.dbSession)
	site, err := stDAO.GetByID(ctx, nil, nsg.SiteID, nil, false)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "The Site where this Network Security Group is being updated could not be retrieved", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	// Technically, if a site didn't have NetworkSecurityGroup enabled,
	// they couldn't have created an NSG in the first place, so they couldn't
	// have created a valid update request anyway.
	if !siteConfig.NetworkSecurityGroup {
		logger.Warn().Msg("site does not have NetworkSecurityGroup capability")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NetworkSecurityGroup capability", nil)
	}

	// Validate request attributes
	err = apiRequest.Validate(siteConfig)
	if err != nil {
		logger.Error().Err(err).Msg("error validating NetworkSecurityGroup update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Network Security Group update request data", err)
	}

	// If a name change is happening, check for name conflicts.
	if apiRequest.Name != nil {
		nsgs, tot, err := nsgDAO.GetAll(ctx, nil, cdbm.NetworkSecurityGroupFilterInput{Name: apiRequest.Name, TenantOrgs: []string{nsg.TenantOrg}, SiteIDs: []uuid.UUID{nsg.SiteID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		if err != nil {
			logger.Error().Err(err).Msg("error checking for existing NetworkSecurityGroup")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to check for existing Network Security Group", nil)
		}
		// If we found one, and it's not the one in this request,
		// no good.
		if tot > 0 && nsgs[0].ID != nsg.ID {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, fmt.Sprintf("Network Security Group with name: %s for Site: %s already exists", *apiRequest.Name, nsg.SiteID), validation.Errors{
				"id": errors.New(nsgs[0].ID),
			})
		}
	}

	var rules []*cdbm.NetworkSecurityGroupRule

	// Override rules if requested. Per-rule validation has already run
	// inside apiRequest.Validate, so rule.ToProto is a pure mapper
	// here; we only check for cross-rule constraints (duplicate names).
	if apiRequest.Rules != nil {
		rules = make([]*cdbm.NetworkSecurityGroupRule, len(apiRequest.Rules))

		names := map[string]bool{}

		for i, rule := range apiRequest.Rules {

			if rule.Name != nil {
				if names[*rule.Name] {
					logger.Error().Str("name", *rule.Name).Msg("duplicate rule name in request")
					return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Duplicate rule name `%s` in request", *rule.Name), nil)
				}

				names[*rule.Name] = true
			}

			rules[i] = &cdbm.NetworkSecurityGroupRule{NetworkSecurityGroupRuleAttributes: rule.ToProto()}
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(dnsgh.dbSession)

	// timeoutResp lets the closure signal an outer-scope handler — TerminateWorkflowOnTimeOut
	// has to run after the closure returns so that the rollback completes before we touch
	// the workflow again. nil means no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var ssds []cdbm.StatusDetail
	var updatedNSG *cdbm.NetworkSecurityGroup

	err = cdb.WithTx(ctx, dnsgh.dbSession, func(tx *cdb.Tx) error {
		// Update NetworkSecurityGroup
		unsgInput := cdbm.NetworkSecurityGroupUpdateInput{
			NetworkSecurityGroupID: nsg.ID,
			Name:                   apiRequest.Name,
			Description:            apiRequest.Description,
			Labels:                 apiRequest.Labels,
			StatefulEgress:         apiRequest.StatefulEgress,
			Rules:                  rules,
			UpdatedByID:            dbUser.ID,
		}
		updated, derr := nsgDAO.Update(ctx, tx, unsgInput)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating NetworkSecurityGroup in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Network Security Group", nil)
		}

		// Get status details
		statusDetails, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{updated.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for NetworkSecurityGroup from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for Network Security Group", nil)
		}
		ssds = statusDetails

		// Get the temporal client for the site we are working with.
		stc, derr := dnsgh.scp.GetClientByID(updated.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// The DB record has already been updated with the request
		// data, so its `ToProto()` is the canonical wire form for
		// both Metadata and the post-merge Attributes (rule list +
		// statefulEgress).
		updateNetworkSecurityGroupRequest := apiRequest.ToProto(updated)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "network-security-group-update-" + updated.ID,
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering NetworkSecurityGroup update workflow")

		// Add context deadlines
		workflowCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(workflowCtx, workflowOptions, "UpdateNetworkSecurityGroup", updateNetworkSecurityGroupRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to update NetworkSecurityGroup")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update Network Security Group on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update NetworkSecurityGroup workflow")

		// Execute the workflow synchronously
		wferr := we.Get(workflowCtx, nil)
		if wferr != nil {
			var applicationErr *tp.ApplicationError
			if errors.As(wferr, &applicationErr) && slices.Contains(swe.UnimplementedOrDeniedErrTypes(), applicationErr.Type()) {
				logger.Error().Msg("feature not yet implemented on target Site")
				return cutil.NewAPIError(http.StatusNotImplemented, fmt.Sprintf("Feature not yet implemented on target Site: %s", wferr), nil)
			}

			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || workflowCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "NetworkSecurityGroup", "UpdateNetworkSecurityGroup")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "Network Security Group update workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to update NetworkSecurityGroup")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update Network Security Group on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update NetworkSecurityGroup workflow")
		updatedNSG = updated
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update Network Security Group, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	apiNetworkSecurityGroup := model.NewAPINetworkSecurityGroup(updatedNSG, ssds)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiNetworkSecurityGroup)
}
