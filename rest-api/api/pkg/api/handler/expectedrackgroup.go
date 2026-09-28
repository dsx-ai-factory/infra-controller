// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"net/http"

	"google.golang.org/protobuf/types/known/emptypb"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateExpectedRackGroupHandler is the API Handler for creating a new ExpectedRackGroup
type CreateExpectedRackGroupHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateExpectedRackGroupHandler initializes and returns a new handler for creating ExpectedRackGroup
func NewCreateExpectedRackGroupHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateExpectedRackGroupHandler {
	return CreateExpectedRackGroupHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create an ExpectedRackGroup
// @Description Create an ExpectedRackGroup
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIExpectedRackGroupCreateRequest true "ExpectedRackGroup creation request"
// @Success 201 {object} model.APIExpectedRackGroup
// @Router /v2/org/{org}/nico/expected-rack-group [post]
func (cerh CreateExpectedRackGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "Create", c, cerh.tracerSpan)
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
	apiRequest := model.APIExpectedRackGroupCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Expected Rack Group creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Rack Group creation data", verr)
	}

	logger = logger.With().Str("RackGroupID", apiRequest.RackGroupID).Logger()
	cerh.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_group_id", apiRequest.RackGroupID), logger)

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

	// Check for a duplicate (site_id, rack_group_id) tuple. Group identity is
	// site-scoped, so the same rack_group_id may exist at different sites.
	erDAOForCheck := cdbm.NewExpectedRackGroupDAO(cerh.dbSession)
	existingRacks, count, err := erDAOForCheck.GetAll(ctx, nil, cdbm.ExpectedRackGroupFilterInput{
		SiteIDs:      []uuid.UUID{site.ID},
		RackGroupIDs: []string{apiRequest.RackGroupID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(1),
	}, nil)
	if err != nil {
		logger.Error().Err(err).Msg("error checking for duplicate Expected Rack Group")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate Expected Rack Group uniqueness due to DB error", nil)
	}
	if count > 0 {
		logger.Warn().Str("RackGroupID", apiRequest.RackGroupID).Msg("Expected Rack Group with specified RackGroupID already exists for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Rack Group with specified RackGroupID already exists for Site", validation.Errors{
			"rackGroupId": errors.New(existingRacks[0].ID.String()),
		})
	}

	// Build create input from request, defaulting metadata to zero values when omitted
	createInput := cdbm.ExpectedRackGroupCreateInput{
		ExpectedRackGroupID: uuid.New(),
		SiteID:              site.ID,
		RackGroupID:         apiRequest.RackGroupID,
		Topology:            apiRequest.Topology,
		Labels:              apiRequest.Labels,
		CreatedBy:           dbUser.ID,
	}
	for _, rack := range apiRequest.Racks {
		createInput.Racks = append(createInput.Racks, rack.ToDBModel())
	}
	if apiRequest.Name != nil {
		createInput.Name = *apiRequest.Name
	}
	if apiRequest.Description != nil {
		createInput.Description = *apiRequest.Description
	}

	erDAO := cdbm.NewExpectedRackGroupDAO(cerh.dbSession)
	expectedRackGroup, err := cdb.WithTxResult(ctx, cerh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedRackGroup, error) {
		// Create the ExpectedRackGroup in DB
		er, err := erDAO.Create(ctx, tx, createInput)
		if err != nil {
			logger.Error().Err(err).Msg("error creating ExpectedRackGroup record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Expected Rack Group due to DB error", nil)
		}

		// Build the create request for workflow
		createExpectedRackGroupRequest := er.ToProto()

		logger.Info().Msg("triggering Expected Rack Group create workflow on Site")

		// Get the temporal client for the site we are working with
		stc, err := cerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Run workflow
		apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_AddExpectedRackGroup_FullMethodName, createExpectedRackGroupRequest, nil, "")
		if apiErr != nil {
			return nil, apiErr
		}
		return er, nil
	})
	if err != nil {
		if (&cdb.PostgresErrorChecker{}).IsUniqueConstraintError(err) {
			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Rack Group with specified RackGroupID already exists for Site", nil)
		}
		return common.HandleTxError(c, logger, err, "Failed to create Expected Rack Group due to DB transaction error")
	}

	// Create response
	apiExpectedRackGroup := model.NewAPIExpectedRackGroup(expectedRackGroup)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiExpectedRackGroup)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllExpectedRackGroupHandler is the API Handler for getting all ExpectedRackGroups
type GetAllExpectedRackGroupHandler struct {
	dbSession  *cdb.Session
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllExpectedRackGroupHandler initializes and returns a new handler for getting all ExpectedRackGroups
func NewGetAllExpectedRackGroupHandler(dbSession *cdb.Session, cfg *config.Config) GetAllExpectedRackGroupHandler {
	return GetAllExpectedRackGroupHandler{
		dbSession:  dbSession,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all ExpectedRackGroups
// @Description Get all ExpectedRackGroups. Provider callers may omit siteId to list across their Sites; Tenant callers must specify siteId.
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site (optional, filters results to specific site)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIExpectedRackGroup
// @Router /v2/org/{org}/nico/expected-rack-group [get]
func (gaerh GetAllExpectedRackGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "GetAll", c, gaerh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	filterInput := cdbm.ExpectedRackGroupFilterInput{}

	// Get Site ID from query param if specified
	siteIDStr := c.QueryParam("siteId")
	var site *cdbm.Site
	var err error
	var privilegeScope *common.TenantPrivilegeScope
	if siteIDStr != "" {
		site, err = common.GetSiteFromIDString(ctx, nil, siteIDStr, gaerh.dbSession)
		if err != nil {
			if errors.Is(err, common.ErrInvalidID) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid siteId in query parameter", nil)
			}
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
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site ID must be specified in query when retrieving Expected Rack Groups as a Tenant", nil)
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
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedRackGroupRelatedEntities)
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
	err = pageRequest.Validate(cdbm.ExpectedRackGroupOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Expected Rack Groups from DB
	erDAO := cdbm.NewExpectedRackGroupDAO(gaerh.dbSession)
	expectedRackGroups, total, err := erDAO.GetAll(
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
		logger.Error().Err(err).Msg("error retrieving Expected Rack Groups from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack Groups due to DB error", nil)
	}

	// Create response
	apiExpectedRackGroups := []*model.APIExpectedRackGroup{}
	for i := range expectedRackGroups {
		apiExpectedRackGroup := model.NewAPIExpectedRackGroup(&expectedRackGroups[i])
		apiExpectedRackGroups = append(apiExpectedRackGroups, apiExpectedRackGroup)
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

	return c.JSON(http.StatusOK, apiExpectedRackGroups)
}

// ~~~~~ Get Handler ~~~~~ //

// GetExpectedRackGroupHandler is the API Handler for retrieving an ExpectedRackGroup
type GetExpectedRackGroupHandler struct {
	dbSession  *cdb.Session
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetExpectedRackGroupHandler initializes and returns a new handler to retrieve ExpectedRackGroup
func NewGetExpectedRackGroupHandler(dbSession *cdb.Session, cfg *config.Config) GetExpectedRackGroupHandler {
	return GetExpectedRackGroupHandler{
		dbSession:  dbSession,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve the ExpectedRackGroup
// @Description Retrieve the ExpectedRackGroup by ID
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Rack Group"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Success 200 {object} model.APIExpectedRackGroup
// @Router /v2/org/{org}/nico/expected-rack-group/{id} [get]
func (gerh GetExpectedRackGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "Get", c, gerh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Rack Group ID from URL param
	expectedRackGroupID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Rack Group ID in URL", nil)
	}

	logger = logger.With().Str("ExpectedRackGroupID", expectedRackGroupID.String()).Logger()
	gerh.tracerSpan.SetAttribute(handlerSpan, attribute.String("expected_rack_group_id", expectedRackGroupID.String()), logger)

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedRackGroupRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Get ExpectedRackGroup from DB by ID
	erDAO := cdbm.NewExpectedRackGroupDAO(gerh.dbSession)
	expectedRackGroup, err := erDAO.Get(ctx, nil, expectedRackGroupID, qIncludeRelations, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Rack Group with ID: %s", expectedRackGroupID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Rack Group from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack Group due to DB error", nil)
	}

	// Site is needed for the access check; reuse if loaded via includeRelation, else fetch.
	site := expectedRackGroup.Site
	if site == nil {
		siteDAO := cdbm.NewSiteDAO(gerh.dbSession)
		site, err = siteDAO.GetByID(ctx, nil, expectedRackGroup.SiteID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Rack Group due to DB error", nil)
		}
	}

	// Scope tenant privilege to the Expected Rack Group's Site.
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
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Rack Group", nil)
	}

	// Create response
	apiExpectedRackGroup := model.NewAPIExpectedRackGroup(expectedRackGroup)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiExpectedRackGroup)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateExpectedRackGroupHandler is the API Handler for updating an ExpectedRackGroup
type UpdateExpectedRackGroupHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateExpectedRackGroupHandler initializes and returns a new handler for updating ExpectedRackGroup
func NewUpdateExpectedRackGroupHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateExpectedRackGroupHandler {
	return UpdateExpectedRackGroupHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an existing ExpectedRackGroup
// @Description Update an existing ExpectedRackGroup by ID
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Rack Group"
// @Param message body model.APIExpectedRackGroupUpdateRequest true "ExpectedRackGroup update request"
// @Success 200 {object} model.APIExpectedRackGroup
// @Router /v2/org/{org}/nico/expected-rack-group/{id} [patch]
func (uerh UpdateExpectedRackGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "Update", c, uerh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Rack Group ID from URL param
	expectedRackGroupID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Rack Group ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedRackGroupID", expectedRackGroupID.String()).Logger()
	uerh.tracerSpan.SetAttribute(handlerSpan, attribute.String("expected_rack_group_id", expectedRackGroupID.String()), logger)

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIExpectedRackGroupUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ExpectedRackGroup update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedRackGroup update request data", verr)
	}

	// If ID is provided in body, it must match the path ID
	if apiRequest.ID != nil && *apiRequest.ID != expectedRackGroupID.String() {
		logger.Warn().
			Str("URLID", expectedRackGroupID.String()).
			Str("RequestDataID", *apiRequest.ID).
			Msg("Mismatched Expected Rack Group ID between path and body")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "If provided, Expected Rack Group ID specified in request data must match URL request value", nil)
	}

	// Get ExpectedRackGroup from DB by ID, including Site relation
	erDAO := cdbm.NewExpectedRackGroupDAO(uerh.dbSession)
	expectedRackGroup, err := erDAO.Get(ctx, nil, expectedRackGroupID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Rack Group with ID: %s", expectedRackGroupID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Rack Group from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack Group due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Rack Group
	site := expectedRackGroup.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Rack Group")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Rack Group", nil)
	}

	// Scope tenant privilege to the Expected Rack Group's Site.
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
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Rack Group", nil)
	}

	// RackGroupID is immutable: Core identifies expected rack groups by rackGroupId, so
	// PATCH may reassert the existing identity but cannot replace it. A rename
	// would first mutate the Cloud record and only then fail in Core's lookup
	// by the new rackGroupId, so the mismatch is rejected here before any database
	// write or workflow trigger. This mirrors the ExpectedMachine BMC MAC
	// identity boundary.
	if apiRequest.RackGroupID != nil && *apiRequest.RackGroupID != expectedRackGroup.RackGroupID {
		logger.Warn().
			Str("requestRackGroupID", *apiRequest.RackGroupID).
			Str("currentRackGroupID", expectedRackGroup.RackGroupID).
			Msg("RackGroupID cannot be changed after creation")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedRackGroup update request data", validation.Errors{
			"rackGroupId": errors.New("RackGroupID cannot be changed after creation"),
		})
	}

	// Build update input from request, mapping flat API fields to DAO fields.
	// RackGroupID is intentionally not passed through: it is immutable, so the DAO
	// update path is structurally incapable of renaming an Expected Rack Group.
	updateInput := cdbm.ExpectedRackGroupUpdateInput{
		ExpectedRackGroupID: expectedRackGroup.ID,
		Topology:            apiRequest.Topology,
		Name:                apiRequest.Name,
		Description:         apiRequest.Description,
	}
	if apiRequest.Racks != nil {
		updateInput.Racks = make([]cdbm.ExpectedRackGroupRack, 0, len(apiRequest.Racks))
		for _, rack := range apiRequest.Racks {
			updateInput.Racks = append(updateInput.Racks, rack.ToDBModel())
		}
	}
	if apiRequest.Labels != nil {
		updateInput.Labels = apiRequest.Labels
	}

	updatedExpectedRackGroup, err := cdb.WithTxResult(ctx, uerh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedRackGroup, error) {
		// Update ExpectedRackGroup in DB
		er, err := erDAO.Update(ctx, tx, updateInput)
		if err != nil {
			logger.Error().Err(err).Msg("failed to update ExpectedRackGroup record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Rack Group due to DB error", nil)
		}

		// Build the update request for workflow using the post-update state so the
		// workflow receives the authoritative merged state of the rack.
		updateExpectedRackGroupRequest := er.ToProto()

		logger.Info().Msg("triggering ExpectedRackGroup update workflow")

		// Get the Temporal client for the site we are working with
		stc, err := uerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Run workflow
		apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_UpdateExpectedRackGroup_FullMethodName, updateExpectedRackGroupRequest, nil, "")
		if apiErr != nil {
			return nil, apiErr
		}
		return er, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Expected Rack Group due to DB transaction error")
	}

	// Create response
	apiExpectedRackGroup := model.NewAPIExpectedRackGroup(updatedExpectedRackGroup)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiExpectedRackGroup)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteExpectedRackGroupHandler is the API Handler for deleting an ExpectedRackGroup
type DeleteExpectedRackGroupHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteExpectedRackGroupHandler initializes and returns a new handler for deleting ExpectedRackGroup
func NewDeleteExpectedRackGroupHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteExpectedRackGroupHandler {
	return DeleteExpectedRackGroupHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an existing ExpectedRackGroup
// @Description Delete an existing ExpectedRackGroup by ID
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Rack Group"
// @Success 204
// @Router /v2/org/{org}/nico/expected-rack-group/{id} [delete]
func (derh DeleteExpectedRackGroupHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "Delete", c, derh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Rack Group ID from URL param
	expectedRackGroupID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Rack Group ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedRackGroupID", expectedRackGroupID.String()).Logger()
	derh.tracerSpan.SetAttribute(handlerSpan, attribute.String("expected_rack_group_id", expectedRackGroupID.String()), logger)

	// Get ExpectedRackGroup from DB by ID, including Site relation
	erDAO := cdbm.NewExpectedRackGroupDAO(derh.dbSession)
	expectedRackGroup, err := erDAO.Get(ctx, nil, expectedRackGroupID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Rack Group with ID: %s", expectedRackGroupID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Rack Group from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Rack Group due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Rack Group
	site := expectedRackGroup.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Rack Group")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Rack Group", nil)
	}

	// Scope tenant privilege to the Expected Rack Group's Site.
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
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Rack Group", nil)
	}

	err = cdb.WithTx(ctx, derh.dbSession, func(tx *cdb.Tx) error {
		// Delete ExpectedRackGroup from DB
		if err := erDAO.Delete(ctx, tx, expectedRackGroup.ID); err != nil {
			logger.Error().Err(err).Msg("unable to delete ExpectedRackGroup record from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Rack Group due to DB error", nil)
		}

		// Build the delete request for workflow
		deleteExpectedRackGroupRequest := &corev1.ExpectedRackGroupRequest{
			RackGroupId: expectedRackGroup.RackGroupID,
		}

		logger.Info().Msg("triggering ExpectedRackGroup delete workflow")

		// Get the temporal client for the site we are working with
		stc, err := derh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Run workflow
		apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteExpectedRackGroup_FullMethodName, deleteExpectedRackGroupRequest, nil, "")
		if apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Rack Group due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ ReplaceAll Handler ~~~~~ //

// ReplaceAllExpectedRackGroupsHandler is the API Handler for replacing the full
// set of ExpectedRackGroups for a given Site with a provided list.
type ReplaceAllExpectedRackGroupsHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewReplaceAllExpectedRackGroupsHandler initializes and returns a new handler for replacing all ExpectedRackGroups on a Site
func NewReplaceAllExpectedRackGroupsHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) ReplaceAllExpectedRackGroupsHandler {
	return ReplaceAllExpectedRackGroupsHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Replace all ExpectedRackGroups for a Site
// @Description Replace the full set of ExpectedRackGroups for a given Site with the provided list
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIReplaceAllExpectedRackGroupsRequest true "ExpectedRackGroup replace-all request"
// @Success 200 {object} []model.APIExpectedRackGroup
// @Router /v2/org/{org}/nico/expected-rack-group [put]
func (raerh ReplaceAllExpectedRackGroupsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "ReplaceAll", c, raerh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate request
	apiRequest := model.APIReplaceAllExpectedRackGroupsRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ReplaceAllExpectedRackGroups request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ReplaceAllExpectedRackGroups request data", verr)
	}

	logger = logger.With().Str("SiteID", apiRequest.SiteID).Int("RackCount", len(apiRequest.ExpectedRackGroups)).Logger()
	raerh.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", apiRequest.SiteID), logger)
	raerh.tracerSpan.SetAttribute(handlerSpan, attribute.Int("rack_count", len(apiRequest.ExpectedRackGroups)), logger)

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
	createInputs := make([]cdbm.ExpectedRackGroupCreateInput, 0, len(apiRequest.ExpectedRackGroups))
	for _, er := range apiRequest.ExpectedRackGroups {
		input := cdbm.ExpectedRackGroupCreateInput{
			ExpectedRackGroupID: uuid.New(),
			SiteID:              site.ID,
			RackGroupID:         er.RackGroupID,
			Topology:            er.Topology,
			Labels:              er.Labels,
			CreatedBy:           dbUser.ID,
		}
		for _, rack := range er.Racks {
			input.Racks = append(input.Racks, rack.ToDBModel())
		}
		if er.Name != nil {
			input.Name = *er.Name
		}
		if er.Description != nil {
			input.Description = *er.Description
		}
		createInputs = append(createInputs, input)
	}

	erDAO := cdbm.NewExpectedRackGroupDAO(raerh.dbSession)
	replacedRackGroups, err := cdb.WithTxResult(ctx, raerh.dbSession, func(tx *cdb.Tx) ([]cdbm.ExpectedRackGroup, error) {
		// Replace the set scoped to this Site
		racks, err := erDAO.ReplaceAll(ctx, tx,
			cdbm.ExpectedRackGroupFilterInput{SiteIDs: []uuid.UUID{site.ID}},
			createInputs,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error replacing ExpectedRackGroup records in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to replace Expected Rack Groups due to DB error", nil)
		}

		// Build the workflow request: a list of all ExpectedRackGroups that should now exist for the Site
		protoRacks := make([]*corev1.ExpectedRackGroup, 0, len(racks))
		for i := range racks {
			protoRacks = append(protoRacks, racks[i].ToProto())
		}
		replaceRequest := &corev1.ExpectedRackGroupList{
			ExpectedRackGroups: protoRacks,
		}

		logger.Info().Msg("triggering ReplaceAllExpectedRackGroups workflow on Site")

		stc, err := raerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_ReplaceAllExpectedRackGroups_FullMethodName, replaceRequest, nil, "")
		if apiErr != nil {
			return nil, apiErr
		}
		return racks, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to replace Expected Rack Groups due to DB transaction error")
	}

	apiRackGroups := make([]*model.APIExpectedRackGroup, 0, len(replacedRackGroups))
	for i := range replacedRackGroups {
		apiRackGroups = append(apiRackGroups, model.NewAPIExpectedRackGroup(&replacedRackGroups[i]))
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiRackGroups)
}

// ~~~~~ DeleteAll Handler ~~~~~ //

// DeleteAllExpectedRackGroupsHandler is the API Handler for deleting all ExpectedRackGroups
// scoped to a specific Site (siteId query parameter).
type DeleteAllExpectedRackGroupsHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteAllExpectedRackGroupsHandler initializes and returns a new handler for deleting all ExpectedRackGroups for a Site
func NewDeleteAllExpectedRackGroupsHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteAllExpectedRackGroupsHandler {
	return DeleteAllExpectedRackGroupsHandler{
		dbSession:  dbSession,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete all ExpectedRackGroups for a Site
// @Description Delete all ExpectedRackGroups for the Site identified by siteId query parameter
// @Tags ExpectedRackGroup
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of Site whose ExpectedRackGroups should be deleted"
// @Success 204
// @Router /v2/org/{org}/nico/expected-rack-group/all [delete]
func (daerh DeleteAllExpectedRackGroupsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedRackGroup", "DeleteAll", c, daerh.tracerSpan)
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
	daerh.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", siteIDStr), logger)

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

	erDAO := cdbm.NewExpectedRackGroupDAO(daerh.dbSession)
	err = cdb.WithTx(ctx, daerh.dbSession, func(tx *cdb.Tx) error {
		// Delete all ExpectedRackGroups for the Site
		if err := erDAO.DeleteAll(ctx, tx, cdbm.ExpectedRackGroupFilterInput{SiteIDs: []uuid.UUID{site.ID}}); err != nil {
			logger.Error().Err(err).Msg("error deleting ExpectedRackGroup records from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Rack Groups due to DB error", nil)
		}

		logger.Info().Msg("triggering DeleteAllExpectedRackGroups workflow on Site")

		stc, err := daerh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		apiErr := common.ExecuteCoreGRPC(ctx, stc, corev1.Forge_DeleteAllExpectedRackGroups_FullMethodName, &emptypb.Empty{}, nil, "")
		if apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Rack Groups due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")
	return c.NoContent(http.StatusNoContent)
}
