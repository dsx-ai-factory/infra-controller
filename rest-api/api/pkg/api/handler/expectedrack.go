// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"net/http"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	tclient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateExpectedRackHandler is the API Handler for creating a new ExpectedRack
type CreateExpectedRackHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateExpectedRackHandler initializes and returns a new handler for creating ExpectedRack
func NewCreateExpectedRackHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateExpectedRackHandler {
	return CreateExpectedRackHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an ExpectedRack
// @Description Create an ExpectedRack
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIExpectedRackCreateRequest true "ExpectedRack creation request"
// @Success 201 {object} model.APIExpectedRack
// @Router /v2/org/{org}/expected-rack [post]
func (cerh CreateExpectedRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIExpectedRackCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Expected Rack creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Rack creation data", verr)
	}

	logger = logger.With().Str("RackID", apiRequest.RackID).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("rack_id", apiRequest.RackID))

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cerh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	// Scope tenant privilege to the Site targeted by this request.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, cerh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, cerh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to Site", nil)
	}

	// Check if Site is in Registered state
	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg("Site is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, cannot perform operation", nil)
	}

	// Check for duplicate (site_id, rack_id) tuple — uniqueness is per-site, so the
	// same rack_id may exist in different sites.
	erDAOForCheck := cdbm.NewExpectedRackDAO(cerh.dbSession)
	existingRacks, count, err := erDAOForCheck.GetAll(ctx, nil, cdbm.ExpectedRackFilterInput{
		SiteIDs: []uuid.UUID{site.ID},
		RackIDs: []string{apiRequest.RackID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(1),
	}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error checking for duplicate Expected Rack")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Expected Rack uniqueness due to DB error", nil)
	}
	if count > 0 {
		logger.Warn().Str("RackID", apiRequest.RackID).Msg("Expected Rack with specified RackID already exists for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Rack with specified RackID already exists for Site", validation.Errors{
			"rackId": errors.New(existingRacks[0].ID.String()),
		})
	}

	// Build create input from request, defaulting metadata to zero values when omitted
	createInput := cdbm.ExpectedRackCreateInput{
		ExpectedRackID: uuid.New(),
		SiteID:         site.ID,
		RackID:         apiRequest.RackID,
		RackProfileID:  apiRequest.RackProfileID,
		Labels:         apiRequest.Labels,
		CreatedBy:      dbUser.ID,
	}
	if apiRequest.Name != nil {
		createInput.Name = *apiRequest.Name
	}
	if apiRequest.Description != nil {
		createInput.Description = *apiRequest.Description
	}

	erDAO := cdbm.NewExpectedRackDAO(cerh.dbSession)
	expectedRack, err := cdb.WithTxResult(ctx, cerh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedRack, error) {
		// Create the ExpectedRack in DB
		er, err := erDAO.Create(ctx, tx, createInput)
		if err != nil {
			logger.Error().Err(err).Msg("error creating ExpectedRack record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Expected Rack due to DB error", nil)
		}

		// Build the create request for workflow
		createExpectedRackRequest := er.ToProto()

		logger.Info().Msg("triggering Expected Rack create workflow on Site")

		// Create workflow options
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-rack-create-" + er.SiteID.String() + "-" + er.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Get the temporal client for the site we are working with
		stc, err := cerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Run workflow
		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "CreateExpectedRack", workflowOptions, createExpectedRackRequest); apiErr != nil {
			return nil, apiErr
		}
		return er, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Expected Rack due to DB transaction error")
	}

	// Create response
	apiExpectedRack := model.NewAPIExpectedRack(expectedRack)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiExpectedRack)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllExpectedRackHandler is the API Handler for getting all ExpectedRacks
type GetAllExpectedRackHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAllExpectedRackHandler initializes and returns a new handler for getting all ExpectedRacks
func NewGetAllExpectedRackHandler(dbSession *cdb.Session, cfg *config.Config) GetAllExpectedRackHandler {
	return GetAllExpectedRackHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all ExpectedRacks
// @Description Get all ExpectedRacks. Provider callers may omit siteId to list across their Sites; Tenant callers must specify siteId.
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site (optional, filters results to specific site)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIExpectedRack
// @Router /v2/org/{org}/expected-rack [get]
func (gaerh GetAllExpectedRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	filterInput := cdbm.ExpectedRackFilterInput{}

	// Get Site ID from query param if specified
	siteIDStr := c.QueryParam("siteId")
	var site *cdbm.Site
	var err error
	var privilegeScope *common.TenantPrivilegeScope
	if siteIDStr != "" {
		site, err = common.GetSiteFromIDString(ctx, nil, siteIDStr, gaerh.dbSession)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
			}
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
		}
		privilegeScope = &common.TenantPrivilegeScope{SiteID: &site.ID}
	}

	// A missing scope is the documented provider-wide list exemption above;
	// tenant-only callers without siteId are rejected below.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gaerh.dbSession, org, dbUser, true, privilegeScope)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if site != nil {
		// Validate ProviderTenantSite relationship and site state
		hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gaerh.dbSession, site, infrastructureProvider, tenant)
		if apiError != nil {
			return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
		}

		if !hasAccess {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
		}

		filterInput.SiteIDs = []uuid.UUID{site.ID}
	} else if tenant != nil && infrastructureProvider == nil {
		// Tenants without a provider identity must specify a Site ID
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site ID must be specified in query when retrieving Expected Racks as a Tenant", nil)
	} else {
		// Get all Sites for the org's Infrastructure Provider
		siteDAO := cdbm.NewSiteDAO(gaerh.dbSession)
		sites, _, err := siteDAO.GetAll(ctx, nil,
			cdbm.SiteFilterInput{InfrastructureProviderIDs: []uuid.UUID{infrastructureProvider.ID}},
			paginator.PageInput{Limit: cutil.GetPtr(math.MaxInt)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Sites from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites for org due to DB error", nil)
		}

		siteIDs := make([]uuid.UUID, 0, len(sites))
		for _, site := range sites {
			siteIDs = append(siteIDs, site.ID)
		}
		filterInput.SiteIDs = siteIDs
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedRackRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination attributes
	err = pageRequest.Validate(cdbm.ExpectedRackOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Expected Racks from DB
	erDAO := cdbm.NewExpectedRackDAO(gaerh.dbSession)
	expectedRacks, total, err := erDAO.GetAll(
		ctx,
		nil,
		filterInput,
		paginator.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		}, qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Expected Racks from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Racks due to DB error", nil)
	}

	// Create response
	apiExpectedRacks := []*model.APIExpectedRack{}
	for i := range expectedRacks {
		apiExpectedRack := model.NewAPIExpectedRack(&expectedRacks[i])
		apiExpectedRacks = append(apiExpectedRacks, apiExpectedRack)
	}

	// Create pagination response header
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to generate pagination response header", nil)
	}

	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiExpectedRacks)
}

// ~~~~~ Get Handler ~~~~~ //

// GetExpectedRackHandler is the API Handler for retrieving an ExpectedRack
type GetExpectedRackHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetExpectedRackHandler initializes and returns a new handler to retrieve ExpectedRack
func NewGetExpectedRackHandler(dbSession *cdb.Session, cfg *config.Config) GetExpectedRackHandler {
	return GetExpectedRackHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the ExpectedRack
// @Description Retrieve the ExpectedRack by ID
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Rack"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Success 200 {object} model.APIExpectedRack
// @Router /v2/org/{org}/expected-rack/{id} [get]
func (gerh GetExpectedRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Rack ID from URL param
	expectedRackID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Rack ID in URL", nil)
	}

	logger = logger.With().Str("ExpectedRackID", expectedRackID.String()).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("expected_rack_id", expectedRackID.String()))

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedRackRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Get ExpectedRack from DB by ID
	erDAO := cdbm.NewExpectedRackDAO(gerh.dbSession)
	expectedRack, err := erDAO.Get(ctx, nil, expectedRackID, qIncludeRelations, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Rack with ID: %s", expectedRackID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Rack from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack due to DB error", nil)
	}

	// Site is needed for the access check; reuse if loaded via includeRelation, else fetch.
	site := expectedRack.Site
	if site == nil {
		siteDAO := cdbm.NewSiteDAO(gerh.dbSession)
		site, err = siteDAO.GetByID(ctx, nil, expectedRack.SiteID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Rack due to DB error", nil)
		}
	}

	// Scope tenant privilege to the Expected Rack's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gerh.dbSession, org, dbUser, true, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gerh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Rack", nil)
	}

	// Create response
	apiExpectedRack := model.NewAPIExpectedRack(expectedRack)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiExpectedRack)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateExpectedRackHandler is the API Handler for updating an ExpectedRack
type UpdateExpectedRackHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateExpectedRackHandler initializes and returns a new handler for updating ExpectedRack
func NewUpdateExpectedRackHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateExpectedRackHandler {
	return UpdateExpectedRackHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing ExpectedRack
// @Description Update an existing ExpectedRack by ID
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Rack"
// @Param message body model.APIExpectedRackUpdateRequest true "ExpectedRack update request"
// @Success 200 {object} model.APIExpectedRack
// @Router /v2/org/{org}/expected-rack/{id} [patch]
func (uerh UpdateExpectedRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Rack ID from URL param
	expectedRackID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Rack ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedRackID", expectedRackID.String()).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("expected_rack_id", expectedRackID.String()))

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIExpectedRackUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ExpectedRack update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedRack update request data", verr)
	}

	// If ID is provided in body, it must match the path ID
	if apiRequest.ID != nil && *apiRequest.ID != expectedRackID.String() {
		logger.Warn().
			Str("URLID", expectedRackID.String()).
			Str("RequestDataID", *apiRequest.ID).
			Msg("Mismatched Expected Rack ID between path and body")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "If provided, Expected Rack ID specified in request data must match URL request value", nil)
	}

	// Get ExpectedRack from DB by ID, including Site relation
	erDAO := cdbm.NewExpectedRackDAO(uerh.dbSession)
	expectedRack, err := erDAO.Get(ctx, nil, expectedRackID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Rack with ID: %s", expectedRackID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Rack from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Rack
	site := expectedRack.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Rack")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Rack", nil)
	}

	// Scope tenant privilege to the Expected Rack's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, uerh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, uerh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Rack", nil)
	}

	// RackID is immutable: Core and Flow identify expected racks by rackId, so
	// PATCH may reassert the existing identity but cannot replace it. A rename
	// would first mutate the Cloud record and only then fail in Core's lookup
	// by the new rackId, so the mismatch is rejected here before any database
	// write or workflow trigger. This mirrors the ExpectedMachine BMC MAC
	// identity boundary.
	if apiRequest.RackID != nil && *apiRequest.RackID != expectedRack.RackID {
		logger.Warn().
			Str("requestRackID", *apiRequest.RackID).
			Str("currentRackID", expectedRack.RackID).
			Msg("RackID cannot be changed after creation")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedRack update request data", validation.Errors{
			"rackId": errors.New("RackID cannot be changed after creation"),
		})
	}

	// Build update input from request, mapping flat API fields to DAO fields.
	// RackID is intentionally not passed through: it is immutable, so the DAO
	// update path is structurally incapable of renaming an Expected Rack.
	updateInput := cdbm.ExpectedRackUpdateInput{
		ExpectedRackID: expectedRack.ID,
		RackProfileID:  apiRequest.RackProfileID,
		Name:           apiRequest.Name,
		Description:    apiRequest.Description,
	}
	if apiRequest.Labels != nil {
		updateInput.Labels = apiRequest.Labels
	}

	updatedExpectedRack, err := cdb.WithTxResult(ctx, uerh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedRack, error) {
		// Update ExpectedRack in DB
		er, err := erDAO.Update(ctx, tx, updateInput)
		if err != nil {
			logger.Error().Err(err).Msg("failed to update ExpectedRack record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Rack due to DB error", nil)
		}

		// Build the update request for workflow using the post-update state so the
		// workflow receives the authoritative merged state of the rack.
		updateExpectedRackRequest := er.ToProto()

		logger.Info().Msg("triggering ExpectedRack update workflow")

		// Create workflow options
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-rack-update-" + er.SiteID.String() + "-" + er.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Get the Temporal client for the site we are working with
		stc, err := uerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Run workflow
		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "UpdateExpectedRack", workflowOptions, updateExpectedRackRequest); apiErr != nil {
			return nil, apiErr
		}
		return er, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Expected Rack due to DB transaction error")
	}

	// Create response
	apiExpectedRack := model.NewAPIExpectedRack(updatedExpectedRack)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiExpectedRack)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteExpectedRackHandler is the API Handler for deleting an ExpectedRack
type DeleteExpectedRackHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteExpectedRackHandler initializes and returns a new handler for deleting ExpectedRack
func NewDeleteExpectedRackHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteExpectedRackHandler {
	return DeleteExpectedRackHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing ExpectedRack
// @Description Delete an existing ExpectedRack by ID
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Rack"
// @Success 204
// @Router /v2/org/{org}/expected-rack/{id} [delete]
func (derh DeleteExpectedRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Rack ID from URL param
	expectedRackID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Rack ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedRackID", expectedRackID.String()).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("expected_rack_id", expectedRackID.String()))

	// Get ExpectedRack from DB by ID, including Site relation
	erDAO := cdbm.NewExpectedRackDAO(derh.dbSession)
	expectedRack, err := erDAO.Get(ctx, nil, expectedRackID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Rack with ID: %s", expectedRackID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Rack from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Rack
	site := expectedRack.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Rack")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Rack", nil)
	}

	// Scope tenant privilege to the Expected Rack's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, derh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, derh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Rack", nil)
	}

	err = cdb.WithTx(ctx, derh.dbSession, func(tx *cdb.Tx) error {
		// Delete ExpectedRack from DB
		if err := erDAO.Delete(ctx, tx, expectedRack.ID); err != nil {
			logger.Error().Err(err).Msg("unable to delete ExpectedRack record from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Rack due to DB error", nil)
		}

		// Build the delete request for workflow
		deleteExpectedRackRequest := &corev1.ExpectedRackRequest{
			RackId: expectedRack.RackID,
		}

		logger.Info().Msg("triggering ExpectedRack delete workflow")

		// Create workflow options
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-rack-delete-" + expectedRack.SiteID.String() + "-" + expectedRack.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Get the temporal client for the site we are working with
		stc, err := derh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Run workflow
		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "DeleteExpectedRack", workflowOptions, deleteExpectedRackRequest); apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Rack due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ ReplaceAll Handler ~~~~~ //

// ReplaceAllExpectedRacksHandler is the API Handler for replacing the full
// set of ExpectedRacks for a given Site with a provided list.
type ReplaceAllExpectedRacksHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewReplaceAllExpectedRacksHandler initializes and returns a new handler for replacing all ExpectedRacks on a Site
func NewReplaceAllExpectedRacksHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) ReplaceAllExpectedRacksHandler {
	return ReplaceAllExpectedRacksHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Replace all ExpectedRacks for a Site
// @Description Replace the full set of ExpectedRacks for a given Site with the provided list
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIReplaceAllExpectedRacksRequest true "ExpectedRack replace-all request"
// @Success 200 {object} []model.APIExpectedRack
// @Router /v2/org/{org}/expected-rack [put]
func (raerh ReplaceAllExpectedRacksHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "ReplaceAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate request
	apiRequest := model.APIReplaceAllExpectedRacksRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ReplaceAllExpectedRacks request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ReplaceAllExpectedRacks request data", verr)
	}

	logger = logger.With().Str("SiteID", apiRequest.SiteID).Int("RackCount", len(apiRequest.ExpectedRacks)).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("site_id", apiRequest.SiteID))
	cotel.SetAttribute(handlerSpan, attribute.Int("rack_count", len(apiRequest.ExpectedRacks)))

	// Retrieve the Site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, raerh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	// Scope tenant privilege to the Site targeted by this request.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, raerh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, raerh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}
	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have access to Site", nil)
	}

	// Check if Site is in Registered state
	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg("Site is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, cannot perform operation", nil)
	}

	// Build replace-all inputs
	createInputs := make([]cdbm.ExpectedRackCreateInput, 0, len(apiRequest.ExpectedRacks))
	for _, er := range apiRequest.ExpectedRacks {
		input := cdbm.ExpectedRackCreateInput{
			ExpectedRackID: uuid.New(),
			SiteID:         site.ID,
			RackID:         er.RackID,
			RackProfileID:  er.RackProfileID,
			Labels:         er.Labels,
			CreatedBy:      dbUser.ID,
		}
		if er.Name != nil {
			input.Name = *er.Name
		}
		if er.Description != nil {
			input.Description = *er.Description
		}
		createInputs = append(createInputs, input)
	}

	erDAO := cdbm.NewExpectedRackDAO(raerh.dbSession)
	replacedRacks, err := cdb.WithTxResult(ctx, raerh.dbSession, func(tx *cdb.Tx) ([]cdbm.ExpectedRack, error) {
		// Replace the set scoped to this Site
		racks, err := erDAO.ReplaceAll(ctx, tx,
			cdbm.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{site.ID}},
			createInputs,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error replacing ExpectedRack records in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to replace Expected Racks due to DB error", nil)
		}

		// Build the workflow request: a list of all ExpectedRacks that should now exist for the Site
		protoRacks := make([]*corev1.ExpectedRack, 0, len(racks))
		for i := range racks {
			protoRacks = append(protoRacks, racks[i].ToProto())
		}
		replaceRequest := &corev1.ExpectedRackList{
			ExpectedRacks: protoRacks,
		}

		logger.Info().Msg("triggering ReplaceAllExpectedRacks workflow on Site")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-rack-replace-all-" + site.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := raerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "ReplaceAllExpectedRacks", workflowOptions, replaceRequest); apiErr != nil {
			return nil, apiErr
		}
		return racks, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to replace Expected Racks due to DB transaction error")
	}

	apiRacks := make([]*model.APIExpectedRack, 0, len(replacedRacks))
	for i := range replacedRacks {
		apiRacks = append(apiRacks, model.NewAPIExpectedRack(&replacedRacks[i]))
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiRacks)
}

// ~~~~~ DeleteAll Handler ~~~~~ //

// DeleteAllExpectedRacksHandler is the API Handler for deleting all ExpectedRacks
// scoped to a specific Site (siteId query parameter).
type DeleteAllExpectedRacksHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteAllExpectedRacksHandler initializes and returns a new handler for deleting all ExpectedRacks for a Site
func NewDeleteAllExpectedRacksHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteAllExpectedRacksHandler {
	return DeleteAllExpectedRacksHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete all ExpectedRacks for a Site
// @Description Delete all ExpectedRacks for the Site identified by siteId query parameter
// @Tags ExpectedRack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site whose ExpectedRacks should be deleted"
// @Success 204
// @Router /v2/org/{org}/expected-rack/all [delete]
func (daerh DeleteAllExpectedRacksHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRack", "DeleteAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// siteId query parameter is required to scope the delete operation
	siteIDStr := c.QueryParam("siteId")
	if siteIDStr == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	if _, err := uuid.Parse(siteIDStr); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid siteId in query parameter", nil)
	}

	logger = logger.With().Str("SiteID", siteIDStr).Logger()
	cotel.SetAttribute(handlerSpan, attribute.String("site_id", siteIDStr))

	// Retrieve the Site
	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, daerh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in query does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in query due to DB error", nil)
	}

	// Scope tenant privilege to the Site targeted by this request.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, daerh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, daerh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}
	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
	}

	erDAO := cdbm.NewExpectedRackDAO(daerh.dbSession)
	err = cdb.WithTx(ctx, daerh.dbSession, func(tx *cdb.Tx) error {
		// Delete all ExpectedRacks for the Site
		if err := erDAO.DeleteAll(ctx, tx, cdbm.ExpectedRackFilterInput{SiteIDs: []uuid.UUID{site.ID}}); err != nil {
			logger.Error().Err(err).Msg("error deleting ExpectedRack records from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Racks due to DB error", nil)
		}

		logger.Info().Msg("triggering DeleteAllExpectedRacks workflow on Site")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-rack-delete-all-" + site.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := daerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// The DeleteAllExpectedRacks workflow takes no parameters (operates on the Site of the agent).
		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "DeleteAllExpectedRacks", workflowOptions, nil); apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Racks due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")
	return c.NoContent(http.StatusNoContent)
}
