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

	"go.opentelemetry.io/otel/attribute"
	temporalClient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	goset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"

	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateInfiniBandPartitionHandler is the API Handler for creating new InfiniBandPartition
type CreateInfiniBandPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateInfiniBandPartitionHandler initializes and returns a new handler for creating InfiniBandPartition
func NewCreateInfiniBandPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateInfiniBandPartitionHandler {
	return CreateInfiniBandPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an InfiniBandPartition
// @Description Create an InfiniBandPartition
// @Tags InfiniBandPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIInfiniBandPartitionCreateRequest true "InfiniBandPartition creation request"
// @Success 201 {object} model.APIInfiniBandPartition
// @Router /v2/org/{org}/nico/infiniband-partition [post]
func (cibph CreateInfiniBandPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfiniBandPartition", "Create", c)
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

	// Validate role, only Tenant Admins are allowed to create InfiniBandPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIInfiniBandPartitionCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating InfiniBand Partition creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating InfiniBand Partition request creation data", verr)
	}

	// Validate the tenant for which this InfiniBandPartition is being created
	orgTenant, err := common.GetTenantForOrg(ctx, nil, cibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Validate and Verify if Site is ready
	site, serr := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cibph.dbSession)
	if serr != nil {
		if serr == common.ErrInvalidID {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create InfiniBand Partition, Invalid Site ID: %s", apiRequest.SiteID), nil)
		}
		if serr == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Failed to create InfiniBand Partition, Could not find Site with ID: %s ", apiRequest.SiteID), nil)
		}
		logger.Warn().Err(serr).Str("Site ID", apiRequest.SiteID).Msg("error retrieving Site from DB by ID")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create InfiniBand Partition, Could not find Site with ID: %s, DB error", apiRequest.SiteID), nil)
	}

	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg(fmt.Sprintf("Unable to associate InfiniBand Partition to Site: %s. Site is not in Registered state", site.ID.String()))
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("Failed to create InfiniBand Partition, Site: %s specified in request is not in Registered state", site.ID.String()), nil)
	}

	// Determine if tenant has access to requested site
	tsDAO := cdbm.NewTenantSiteDAO(cibph.dbSession)
	_, err = tsDAO.GetByTenantIDAndSiteID(ctx, nil, orgTenant.ID, site.ID, nil)
	if err != nil {
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant is not associated with Site specified in query", nil)
		}
		logger.Warn().Err(err).Msg("error retrieving Tenant Site association from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to determine if Tenant has access to Site specified in query, DB error", nil)
	}

	// Ensure that Tenant has an Allocation with specified Site
	aDAO := cdbm.NewAllocationDAO(cibph.dbSession)
	allocationFilter := cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{orgTenant.ID}, SiteIDs: []uuid.UUID{site.ID}}
	aCount, serr := aDAO.GetCount(ctx, nil, allocationFilter)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Allocations count from DB for Tenant and Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site Allocations count for Tenant", nil)
	}

	if aCount == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
			"Tenant does not have any Allocations with Site specified in request data", nil)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another InfiniBand Partition with same name
	// TODO consider doing this with an advisory lock for correctness
	ibpDAO := cdbm.NewInfiniBandPartitionDAO(cibph.dbSession)
	ibps, tot, err := ibpDAO.GetAll(
		ctx,
		nil,
		cdbm.InfiniBandPartitionFilterInput{
			Names:     []string{apiRequest.Name},
			SiteIDs:   []uuid.UUID{site.ID},
			TenantIDs: []uuid.UUID{orgTenant.ID},
		},
		paginator.PageInput{},
		nil,
	)
	if err != nil {
		logger.Error().Err(err).Msg("db error checking for name uniqueness of tenant ib partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create InfiniBand Partition due to DB error", nil)
	}
	if tot > 0 {
		logger.Warn().Str("tenantId", orgTenant.ID.String()).Str("name", apiRequest.Name).Msg("InfiniBand Partition with same name already exists for Tenant")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another InfiniBand Partition with specified name already exists for Tenant", validation.Errors{
			"id": errors.New(ibps[0].ID.String()),
		})
	}

	// Labels support
	var labels map[string]string
	if apiRequest.Labels != nil {
		labels = apiRequest.Labels
	}

	sdDAO := cdbm.NewStatusDetailDAO(cibph.dbSession)

	// timeoutResp lets the closure signal an outer-scope handler — TerminateWorkflowOnTimeOut
	// has to run after the closure returns so that the rollback completes before we touch
	// the workflow again. nil means no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var ssd *cdbm.StatusDetail
	var createdIBP *cdbm.InfiniBandPartition

	err = cdb.WithTx(ctx, cibph.dbSession, func(tx *cdb.Tx) error {
		// create the db record for InfiniBand Partition
		ibp, derr := ibpDAO.Create(
			ctx,
			tx,
			cdbm.InfiniBandPartitionCreateInput{
				Name:        apiRequest.Name,
				Description: apiRequest.Description,
				TenantOrg:   org,
				SiteID:      site.ID,
				TenantID:    orgTenant.ID,
				Labels:      labels,
				Status:      cdbm.InfiniBandPartitionStatusPending,
				CreatedBy:   dbUser.ID,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("unable to create InfiniBand Partition record in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed creating InfiniBand Partition record", nil)
		}

		// create the status detail record
		newSSD, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ibp.ID.String(), Status: string(cdbm.InfiniBandPartitionStatusPending), Message: cutil.GetPtr("received InfiniBand Partition creation request, pending")})
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for InfiniBand Partition", nil)
		}
		if newSSD == nil {
			logger.Error().Msg("Status Detail DB entry not returned from Create")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to get new Status Detail for InfiniBand Partition", nil)
		}
		ssd = newSSD

		// Get the temporal client for the site we are working with.
		stc, derr := cibph.scp.GetClientByID(site.ID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		createIBPRequest := apiRequest.ToProto(ibp)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "infiniband-partition-create-" + ibp.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering InfiniBand Partition create workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "CreateInfiniBandPartitionV2", createIBPRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to create InfiniBand Partition")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to create InfiniBand Partition on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous create InfiniBand Partition workflow")

		// Block until the workflow has completed and returned success/error.
		wferr := we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "InfiniBandPartition", "CreateInfiniBandPartitionV2")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "InfiniBand Partition create workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to create InfiniBand Partition")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to create InfiniBand Partition on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous create InfiniBand Partition workflow")
		createdIBP = ibp
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to create InfiniBand Partition, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// create response
	apiInfiniBandPartition := model.NewAPIInfiniBandPartition(createdIBP, []cdbm.StatusDetail{*ssd})
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiInfiniBandPartition)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllInfiniBandPartitionHandler is the API Handler for getting all InfiniBandPartitions
type GetAllInfiniBandPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetAllInfiniBandPartitionHandler initializes and returns a new handler for getting all InfiniBandPartitions
func NewGetAllInfiniBandPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetAllInfiniBandPartitionHandler {
	return GetAllInfiniBandPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all InfiniBandPartitions
// @Description Get all InfiniBandPartitions
// @Tags InfiniBandPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site"
// @Param status query string false "Filter by status" e.g. 'Pending', 'Error'"
// @Param query query string false "Query input for full text search"
// @Param includeRelation query string false "Related entities to include in response e.g. 'InfrastructureProvider', 'Tenant'"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIInfiniBandPartition
// @Router /v2/org/{org}/nico/infiniband-partition [get]
func (gaibph GetAllInfiniBandPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfiniBandPartition", "GetAll", c)
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

	// Validate role, only Tenant Admins are allowed to retrieve InfiniBandPartitions
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
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
	err = pageRequest.Validate(cdbm.InfiniBandPartitionOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Validate the tenant for which this InfiniBandPartition is being created
	tenant, err := common.GetTenantForOrg(ctx, nil, gaibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve tenant for org", nil)
	}

	// Get site ID from query param
	tsDAO := cdbm.NewTenantSiteDAO(gaibph.dbSession)
	var siteIDs []uuid.UUID
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gaibph.dbSession)
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

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InfiniBandPartitionRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get query text for full text search from query param
	searchQuery := common.GetSearchQuery(c)
	if searchQuery != nil {
		cotel.SetAttribute(handlerSpan, attribute.String("query", *searchQuery))
	}

	// Get status from query param
	var statuses []string

	statusQuery := c.QueryParam("status")
	if statusQuery != "" {
		cotel.SetAttribute(handlerSpan, attribute.String("status", statusQuery))
		_, ok := cdbm.InfiniBandPartitionStatusMap[cdbm.InfiniBandPartitionStatus(statusQuery)]
		if !ok {
			logger.Warn().Msg(fmt.Sprintf("invalid value in status query: %v", statusQuery))
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Status value in query", nil)
		}
		statuses = append(statuses, statusQuery)
	}

	ibpDAO := cdbm.NewInfiniBandPartitionDAO(gaibph.dbSession)
	ibps, total, err := ibpDAO.GetAll(
		ctx,
		nil,
		cdbm.InfiniBandPartitionFilterInput{
			SiteIDs:     siteIDs,
			TenantIDs:   []uuid.UUID{tenant.ID},
			Statuses:    statuses,
			SearchQuery: searchQuery,
		},
		paginator.PageInput{Offset: pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		},
		qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error getting InfiniBand Partitions from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Partitions, DB error", nil)
	}

	// Get status details
	sdDAO := cdbm.NewStatusDetailDAO(gaibph.dbSession)
	sdEntityIDs := []string{}
	for _, ibp := range ibps {
		sdEntityIDs = append(sdEntityIDs, ibp.ID.String())
	}

	ssds, serr := sdDAO.GetRecentByEntityIDs(ctx, nil, sdEntityIDs, common.RECENT_STATUS_DETAIL_COUNT)
	if serr != nil {
		logger.Warn().Err(serr).Msg("error retrieving Status Details for InfiniBand Partitions from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to populate status history for InfiniBand Partitions", nil)
	}

	ssdMap := map[string][]cdbm.StatusDetail{}
	for _, ssd := range ssds {
		cssd := ssd
		ssdMap[ssd.EntityID] = append(ssdMap[ssd.EntityID], cssd)
	}

	// Create response
	apiInfiniBandPartitions := []*model.APIInfiniBandPartition{}
	for _, ibp := range ibps {
		curIBP := ibp
		apiInfiniBandPartition := model.NewAPIInfiniBandPartition(&curIBP, ssdMap[ibp.ID.String()])
		apiInfiniBandPartitions = append(apiInfiniBandPartitions, apiInfiniBandPartition)
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

	return c.JSON(http.StatusOK, apiInfiniBandPartitions)
}

// ~~~~~ Get Handler ~~~~~ //

// GetInfiniBandPartitionHandler is the API Handler for retrieving InfiniBandPartition
type GetInfiniBandPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	cfg       *config.Config
}

// NewGetInfiniBandPartitionHandler initializes and returns a new handler to retrieve InfiniBandPartition
func NewGetInfiniBandPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, cfg *config.Config) GetInfiniBandPartitionHandler {
	return GetInfiniBandPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the InfiniBandPartition
// @Description Retrieve the InfiniBandPartition
// @Tags InfiniBandPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of InfiniBandPartition"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'Tenant'"
// @Success 200 {object} model.APIInfiniBandPartition
// @Router /v2/org/{org}/nico/infiniband-partition/{id} [get]
func (gibph GetInfiniBandPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfiniBandPartition", "Get", c)
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

	// Validate role, only Tenant Admins are allowed to retrieve InfiniBandPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errMsg := common.GetAndValidateQueryRelations(qParams, cdbm.InfiniBandPartitionRelatedEntities)
	if errMsg != "" {
		logger.Warn().Msg(errMsg)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errMsg, nil)
	}

	// Get IB Partition ID from URL
	ibpStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("infiniband_partition_id", ibpStrID))

	ibpID, err := uuid.Parse(ibpStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid InfiniBand Partition ID in URL", nil)
	}

	ibpDAO := cdbm.NewInfiniBandPartitionDAO(gibph.dbSession)

	// Validate the tenant for which this InfiniBandPartition is being retrieved
	orgTenant, err := common.GetTenantForOrg(ctx, nil, gibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Check that InfiniBand Partition exists
	ibp, err := ibpDAO.GetByID(ctx, nil, ibpID, qIncludeRelations)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving InfiniBand Partition DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve InfiniBand Partition with specified ID", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve InfiniBand Partition", nil)
	}

	if ibp.TenantID != orgTenant.ID {
		logger.Warn().Msg("tenant in org does not match tenant in InfiniBand Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for InfiniBand Partition in request does not match Tenant in org", nil)
	}

	// get status details for the response
	sdDAO := cdbm.NewStatusDetailDAO(gibph.dbSession)
	ssds, err := sdDAO.GetRecentByEntityIDs(ctx, nil, []string{ibp.ID.String()}, common.RECENT_STATUS_DETAIL_COUNT)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Status Details for InfiniBand Partition from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Status Details for InfiniBand Partition", nil)
	}

	// Send response
	apiIBP := model.NewAPIInfiniBandPartition(ibp, ssds)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiIBP)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateInfiniBandPartitionHandler is the API Handler for updating a InfiniBandPartition
type UpdateInfiniBandPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateInfiniBandPartitionHandler initializes and returns a new handler for updating InfiniBandPartition
func NewUpdateInfiniBandPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateInfiniBandPartitionHandler {
	return UpdateInfiniBandPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing InfiniBandPartition
// @Description Update an existing InfiniBandPartition
// @Tags InfiniBandPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of InfiniBandPartition"
// @Param message body model.APIInfiniBandPartitionUpdateRequest true "InfiniBandPartition update request"
// @Success 200 {object} model.APIInfiniBandPartition
// @Router /v2/org/{org}/nico/infiniband-partition/{id} [patch]
func (uibph UpdateInfiniBandPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfiniBandPartition", "Update", c)
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

	// Validate role, only Tenant Admins are allowed to update InfiniBandPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get IB Partition ID from URL
	ibpStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("infiniband_partition_id", ibpStrID))

	ibpID, err := uuid.Parse(ibpStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid InfiniBand Partition ID in URL", nil)
	}

	ibpDAO := cdbm.NewInfiniBandPartitionDAO(uibph.dbSession)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIInfiniBandPartitionUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating InfiniBand Partition update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating InfiniBand Partition update request data", verr)
	}

	// Validate the tenant for which this InfiniBandPartition is being updated
	orgTenant, err := common.GetTenantForOrg(ctx, nil, uibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// check that InfiniBandPartition exists
	ibp, err := ibpDAO.GetByID(ctx, nil, ibpID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving InfiniBand Partition DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not find InfiniBand Partition with ID specified in URL", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve InfiniBand Partition to update", nil)
	}

	// verify tenant matches
	if ibp.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in InfiniBand Partition does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for InfiniBand Partition in request does not match Tenant in org", nil)
	}

	// Ensure that Tenant has an Allocation with specified Site
	aDAO := cdbm.NewAllocationDAO(uibph.dbSession)
	allocationFilter := cdbm.AllocationFilterInput{TenantIDs: []uuid.UUID{ibp.TenantID}, SiteIDs: []uuid.UUID{ibp.SiteID}}
	aCount, serr := aDAO.GetCount(ctx, nil, allocationFilter)
	if serr != nil {
		logger.Error().Err(serr).Msg("error retrieving Allocations count from DB for Tenant and Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site Allocations count for Tenant", nil)
	}

	if aCount == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden,
			"Tenant does not have any Allocations with Site specified in request data", nil)
	}

	// check for name uniqueness for the tenant, ie, tenant cannot have another InfiniBand Partition with same name
	if apiRequest.Name != nil && *apiRequest.Name != ibp.Name {
		ibps, tot, serr := ibpDAO.GetAll(
			ctx,
			nil,
			cdbm.InfiniBandPartitionFilterInput{
				Names:     []string{*apiRequest.Name},
				SiteIDs:   []uuid.UUID{ibp.SiteID},
				TenantIDs: []uuid.UUID{orgTenant.ID},
			},
			paginator.PageInput{},
			nil,
		)
		if serr != nil {
			logger.Error().Err(serr).Msg("db error checking for name uniqueness of tenant's InfiniBand Partition")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to update InfiniBand Partition due to DB error", nil)
		}
		if tot > 0 {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Another InfiniBand Partition with specified name already exists for Tenant", validation.Errors{
				"id": errors.New(ibps[0].ID.String()),
			})
		}
	}

	// Labels support
	var labels map[string]string
	if apiRequest.Labels != nil {
		labels = apiRequest.Labels
	}

	sdDAO := cdbm.NewStatusDetailDAO(uibph.dbSession)

	// timeoutResp lets the closure signal an outer-scope handler — TerminateWorkflowOnTimeOut
	// has to run after the closure returns so that the rollback completes before we touch
	// the workflow again. nil means no timeout occurred and the normal flow continues.
	var timeoutResp func() error
	var ssds []cdbm.StatusDetail
	var updatedIBP *cdbm.InfiniBandPartition

	err = cdb.WithTx(ctx, uibph.dbSession, func(tx *cdb.Tx) error {
		uipb, derr := ibpDAO.Update(
			ctx,
			tx,
			cdbm.InfiniBandPartitionUpdateInput{
				InfiniBandPartitionID: ibpID,
				Name:                  apiRequest.Name,
				Description:           apiRequest.Description,
				Labels:                labels,
			},
		)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating InfiniBand Partition in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to update InfiniBand Partition", nil)
		}
		logger.Info().Msg("done updating InfiniBand Partition in DB")

		stc, derr := uibph.scp.GetClientByID(ibp.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		updateIBPRequest := apiRequest.ToProto(uipb)

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "infiniband-partition-update-" + ibp.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering InfiniBand Partition update workflow")

		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		we, derr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "UpdateInfiniBandPartition", updateIBPRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to update InfiniBand Partition")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to update InfiniBand Partition on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous update InfiniBand Partition workflow")

		wferr := we.Get(wfCtx, nil)
		if wferr != nil {
			var timeoutErr *tp.TimeoutError
			if errors.As(wferr, &timeoutErr) || wferr == context.DeadlineExceeded || wfCtx.Err() != nil {
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "InfiniBandPartition", "UpdateInfiniBandPartition")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "InfiniBand Partition update workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to update InfiniBand Partition")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to update InfiniBand Partition on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous update InfiniBand Partition workflow")

		// get status details for the response
		curSSDs, _, derr := sdDAO.GetAll(ctx, tx, cdbm.StatusDetailFilterInput{EntityIDs: []string{uipb.ID.String()}}, paginator.PageInput{Limit: cutil.GetPtr(pagination.MaxPageSize)})
		if derr != nil {
			logger.Error().Err(derr).Msg("error retrieving Status Details for InfiniBand Partition from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Status Details for InfiniBand Partition", nil)
		}
		ssds = curSSDs
		updatedIBP = uipb
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to update InfiniBand Partition, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// send response
	apiInfiniBandPartition := model.NewAPIInfiniBandPartition(updatedIBP, ssds)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiInfiniBandPartition)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteInfiniBandPartitionHandler is the API Handler for deleting a InfiniBandPartition
type DeleteInfiniBandPartitionHandler struct {
	dbSession *cdb.Session
	tc        temporalClient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteInfiniBandPartitionHandler initializes and returns a new handler for deleting InfiniBandPartition
func NewDeleteInfiniBandPartitionHandler(dbSession *cdb.Session, tc temporalClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteInfiniBandPartitionHandler {
	return DeleteInfiniBandPartitionHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing InfiniBandPartition
// @Description Delete an existing InfiniBandPartition
// @Tags InfiniBandPartition
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of InfiniBandPartition"
// @Success 202
// @Router /v2/org/{org}/nico/infiniband-partition/{id} [delete]
func (dibph DeleteInfiniBandPartitionHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("InfiniBandPartition", "Delete", c)
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

	// Validate role, only Tenant Admins are allowed to delete InfiniBandPartition
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.TenantAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Tenant Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Tenant Admin role with org", nil)
	}

	// Get InfiniBand Partition ID from URL param
	ibpStrID := c.Param("id")

	cotel.SetAttribute(handlerSpan, attribute.String("infiniband_partition_id", ibpStrID))

	ibpID, err := uuid.Parse(ibpStrID)
	if err != nil {
		logger.Warn().Err(err).Msg("error parsing id in url into uuid")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid InfiniBand Partition ID in URL", nil)
	}

	// Validate the tenant for which this InfiniBandPartition is being updated
	orgTenant, err := common.GetTenantForOrg(ctx, nil, dibph.dbSession, org)
	if err != nil {
		if err == common.ErrOrgTenantNotFound {
			logger.Warn().Err(err).Msg("Org does not have a Tenant associated")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Org does not have a Tenant associated", nil)
		}
		logger.Error().Err(err).Msg("unable to retrieve Tenant for org")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Tenant for org", nil)
	}

	// Check that InfiniBand Partition exists
	ibpDAO := cdbm.NewInfiniBandPartitionDAO(dibph.dbSession)
	ibp, err := ibpDAO.GetByID(ctx, nil, ibpID, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving InfiniBand Partition DB entity")
		if err == cdb.ErrDoesNotExist {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Could not retrieve InfiniBand Partition to delete", nil)
		}
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Could not retrieve InfiniBand Partition to delete", nil)
	}

	// verify tenant matches
	if ibp.TenantID != orgTenant.ID {
		logger.Warn().Msg("Tenant in InfiniBand Partition does not belong to Tenant in org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Tenant for InfiniBand Partition in request does not match Tenant in org", nil)
	}

	// Block deletion while Instances referenced by InfiniBand Interfaces still exist in the DB
	ibiDAO := cdbm.NewInfiniBandInterfaceDAO(dibph.dbSession)
	ibInterfaces, _, err := ibiDAO.GetAll(ctx, nil, cdbm.InfiniBandInterfaceFilterInput{
		InfiniBandPartitionIDs: []uuid.UUID{ibpID},
	}, paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving InfiniBand Interfaces from DB for InfiniBand Partition")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve InfiniBand Interfaces for InfiniBand Partition", nil)
	}
	instanceIDSet := goset.NewSet[uuid.UUID]()
	for _, ibi := range ibInterfaces {
		instanceIDSet.Add(ibi.InstanceID)
	}
	if instanceIDSet.Cardinality() > 0 {
		instanceDAO := cdbm.NewInstanceDAO(dibph.dbSession)
		activeCount, err := instanceDAO.GetCount(ctx, nil, cdbm.InstanceFilterInput{
			InstanceIDs: instanceIDSet.ToSlice(),
		})
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving count of Instances from DB for InfiniBand Partition interface check")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve count of Instances for InfiniBand Partition", nil)
		}
		if activeCount > 0 {
			logger.Warn().Int("active_instance_count", activeCount).Msg("InfiniBand Partition has active Instances associated via interfaces")
			msg := fmt.Sprintf("%d active Instances are associated with this InfiniBand Partition, unable to delete", activeCount)
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, msg, nil)
		}
	}

	sdDAO := cdbm.NewStatusDetailDAO(dibph.dbSession)

	// timeoutResp lets the closure signal an outer-scope handler — TerminateWorkflowOnTimeOut
	// has to run after the closure returns so that the rollback completes before we touch
	// the workflow again. nil means no timeout occurred and the normal flow continues.
	var timeoutResp func() error

	err = cdb.WithTx(ctx, dibph.dbSession, func(tx *cdb.Tx) error {
		// Update InfiniBand Partition and set status to Deleting
		deletingStatus := cdbm.InfiniBandPartitionStatusDeleting
		if _, derr := ibpDAO.Update(
			ctx,
			tx,
			cdbm.InfiniBandPartitionUpdateInput{
				InfiniBandPartitionID: ibp.ID,
				Status:                &deletingStatus,
			},
		); derr != nil {
			logger.Error().Err(derr).Msg("error updating InfiniBand Partition in DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete InfiniBand Partition, DB error", nil)
		}

		// Create status detail
		if _, derr := sdDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: ibp.ID.String(), Status: string(deletingStatus), Message: cutil.GetPtr("Received request for deletion, pending processing")}); derr != nil {
			logger.Error().Err(derr).Msg("error creating Status Detail DB entry")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Status Detail for InfiniBand Partition deletion", nil)
		}

		// Get the temporal client for the site we are working with.
		stc, derr := dibph.scp.GetClientByID(ibp.SiteID)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		deleteIBPRequest := ibp.ToDeletionRequestProto()

		workflowOptions := temporalClient.StartWorkflowOptions{
			ID:                       "infiniband-partition-delete-" + ibp.ID.String(),
			TaskQueue:                queue.SiteTaskQueue,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		}

		logger.Info().Msg("triggering InfiniBand Partition delete workflow")

		// Add context deadlines
		wfCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
		defer cancel()

		// Trigger Site workflow
		we, derr := stc.ExecuteWorkflow(wfCtx, workflowOptions, "DeleteInfiniBandPartitionV2", deleteIBPRequest)
		if derr != nil {
			logger.Error().Err(derr).Msg("failed to synchronously start Temporal workflow to delete InfiniBand Partition")
			return cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed start sync workflow to delete InfiniBand Partition on Site: %s", derr), nil)
		}

		wid := we.GetID()
		logger.Info().Str("Workflow ID", wid).Msg("executed synchronous delete InfiniBand Partition workflow")

		// Execute the workflow synchronously
		wferr := we.Get(wfCtx, nil)
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
				logger.Error().Err(wferr).Msg("failed to delete InfiniBand Partition, timeout occurred executing workflow on Site.")
				timeoutResp = func() error {
					return common.TerminateWorkflowOnTimeOut(c, logger, stc, wid, wferr, "InfiniBandPartition", "Delete")
				}
				return cutil.NewAPIError(http.StatusInternalServerError, "InfiniBand Partition delete workflow timed out", nil)
			}

			code, uwerr := common.UnwrapWorkflowError(wferr)
			logger.Error().Err(uwerr).Msg("failed to synchronously execute Temporal workflow to delete InfiniBand Partition")
			return cutil.NewAPIError(code, fmt.Sprintf("Failed to execute sync workflow to delete InfiniBand Partition on Site: %s", uwerr), nil)
		}

		logger.Info().Str("Workflow ID", wid).Msg("completed synchronous delete InfiniBand Partition workflow")
		return nil
	})
	// The wrapping `if err != nil` ensures real tx-helper errors (commit /
	// rollback failures that wrap into something other than the cutil.APIError
	// marker we returned for the timeout case) are surfaced via HandleTxError,
	// while the timeout-case APIError falls through to the timeoutResp call.
	if err != nil {
		var apiErr *cutil.APIError
		if !errors.As(err, &apiErr) || timeoutResp == nil {
			return common.HandleTxError(c, logger, err, "Failed to delete InfiniBand Partition, DB transaction error")
		}
	}
	if timeoutResp != nil {
		return timeoutResp()
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())

}
