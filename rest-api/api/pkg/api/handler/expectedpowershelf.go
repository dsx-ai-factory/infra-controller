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

// CreateExpectedPowerShelfHandler is the API Handler for creating new ExpectedPowerShelf
type CreateExpectedPowerShelfHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateExpectedPowerShelfHandler initializes and returns a new handler for creating ExpectedPowerShelf
func NewCreateExpectedPowerShelfHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateExpectedPowerShelfHandler {
	return CreateExpectedPowerShelfHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an ExpectedPowerShelf
// @Description Create an ExpectedPowerShelf
// @Tags ExpectedPowerShelf
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIExpectedPowerShelfCreateRequest true "ExpectedPowerShelf creation request"
// @Success 201 {object} model.APIExpectedPowerShelf
// @Router /v2/org/{org}/nico/expected-power-shelf [post]
func (cepsh CreateExpectedPowerShelfHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedPowerShelf", "Create", c)
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
	apiRequest := model.APIExpectedPowerShelfCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Expected Power Shelf creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Power Shelf creation data", verr)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cepsh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	// Scope tenant privilege to the Site targeted by this request.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, cepsh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, cepsh.dbSession, site, infrastructureProvider, tenant)
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

	// Check for duplicate MAC address. The DB enforces UNIQUE (bmc_mac_address, site_id),
	// but we pre-check here so we can return the conflicting record's ID in the response.
	epsDAO := cdbm.NewExpectedPowerShelfDAO(cepsh.dbSession)
	epsList, count, err := epsDAO.GetAll(ctx, nil, cdbm.ExpectedPowerShelfFilterInput{
		BmcMacAddresses: []string{apiRequest.BmcMacAddress},
		SiteIDs:         []uuid.UUID{site.ID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(1),
	}, nil)

	if err != nil {
		logger.Error().Err(err).Msg("error checking for duplicate MAC address on Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate MAC address uniqueness on Site due to DB error", nil)
	}

	if count > 0 {
		logger.Warn().Str("MacAddress", apiRequest.BmcMacAddress).Msg("Expected Power Shelf with specified MAC address already exists on Site")

		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Power Shelf with specified MAC address already exists on Site", validation.Errors{
			"id": errors.New(epsList[0].ID.String()),
		})
	}

	expectedPowerShelf, err := cdb.WithTxResult(ctx, cepsh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedPowerShelf, error) {
		// Note: DefaultBmcUsername and BmcPassword are not stored in DB, only passed to workflow
		eps, err := epsDAO.Create(
			ctx,
			tx,
			cdbm.ExpectedPowerShelfCreateInput{
				ExpectedPowerShelfID: uuid.New(),
				SiteID:               site.ID,
				BmcMacAddress:        apiRequest.BmcMacAddress,
				ShelfSerialNumber:    apiRequest.ShelfSerialNumber,
				BmcIpAddress:         apiRequest.BmcIpAddress,
				RackID:               apiRequest.RackID,
				Name:                 apiRequest.Name,
				Manufacturer:         apiRequest.Manufacturer,
				Model:                apiRequest.Model,
				Description:          apiRequest.Description,
				SlotID:               apiRequest.SlotID,
				TrayIdx:              apiRequest.TrayIdx,
				HostID:               apiRequest.HostID,
				Labels:               apiRequest.Labels,
				CreatedBy:            dbUser.ID,
			},
		)
		if err != nil {
			logger.Error().Err(err).Msg("error creating ExpectedPowerShelf record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Expected Power Shelf due to DB error", nil)
		}

		createExpectedPowerShelfRequest := eps.ToProto(cdbm.ExpectedPowerShelfCredentials{
			Username: apiRequest.DefaultBmcUsername,
			Password: apiRequest.DefaultBmcPassword,
		})

		logger.Info().Msg("triggering Expected Power Shelf create workflow on Site")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-power-shelf-create-" + eps.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := cepsh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "CreateExpectedPowerShelf", workflowOptions, createExpectedPowerShelfRequest); apiErr != nil {
			return nil, apiErr
		}
		return eps, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Expected Power Shelf due to DB transaction error")
	}

	apiExpectedPowerShelf := model.NewAPIExpectedPowerShelf(expectedPowerShelf)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiExpectedPowerShelf)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllExpectedPowerShelfHandler is the API Handler for getting all ExpectedPowerShelves
type GetAllExpectedPowerShelfHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAllExpectedPowerShelfHandler initializes and returns a new handler for getting all ExpectedPowerShelves
func NewGetAllExpectedPowerShelfHandler(dbSession *cdb.Session, cfg *config.Config) GetAllExpectedPowerShelfHandler {
	return GetAllExpectedPowerShelfHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all ExpectedPowerShelves
// @Description Get all ExpectedPowerShelves. Provider callers may omit siteId to list across their Sites; Tenant callers must specify siteId.
// @Tags ExpectedPowerShelf
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site (optional, filters results to specific site)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIExpectedPowerShelf
// @Router /v2/org/{org}/nico/expected-power-shelf [get]
func (gaepsh GetAllExpectedPowerShelfHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedPowerShelf", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	filterInput := cdbm.ExpectedPowerShelfFilterInput{}

	// Get Site ID from query param if specified
	siteIDStr := c.QueryParam("siteId")
	var site *cdbm.Site
	var err error
	var privilegeScope *common.TenantPrivilegeScope
	if siteIDStr != "" {
		site, err = common.GetSiteFromIDString(ctx, nil, siteIDStr, gaepsh.dbSession)
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
	// tenant callers without siteId are rejected below.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gaepsh.dbSession, org, dbUser, true, privilegeScope)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if site != nil {
		// Validate ProviderTenantSite relationship and site state
		hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gaepsh.dbSession, site, infrastructureProvider, tenant)
		if apiError != nil {
			return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
		}

		if !hasAccess {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
		}

		filterInput.SiteIDs = []uuid.UUID{site.ID}
	} else if tenant != nil && infrastructureProvider == nil {
		// Tenant-only callers must specify a Site ID.
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site ID must be specified in query when retrieving Expected Power Shelves as a Tenant", nil)
	} else {
		// Get all Sites for the org's Infrastructure Provider
		siteDAO := cdbm.NewSiteDAO(gaepsh.dbSession)
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
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedPowerShelfRelatedEntities)
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
	err = pageRequest.Validate(cdbm.ExpectedPowerShelfOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Expected Power Shelves from DB
	epsDAO := cdbm.NewExpectedPowerShelfDAO(gaepsh.dbSession)
	expectedPowerShelves, total, err := epsDAO.GetAll(
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
		logger.Error().Err(err).Msg("error retrieving Expected Power Shelves from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Power Shelves due to DB error", nil)
	}

	// Create response
	apiExpectedPowerShelves := []*model.APIExpectedPowerShelf{}
	for _, eps := range expectedPowerShelves {
		apiExpectedPowerShelf := model.NewAPIExpectedPowerShelf(&eps)
		apiExpectedPowerShelves = append(apiExpectedPowerShelves, apiExpectedPowerShelf)
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

	return c.JSON(http.StatusOK, apiExpectedPowerShelves)
}

// ~~~~~ Get Handler ~~~~~ //

// GetExpectedPowerShelfHandler is the API Handler for retrieving ExpectedPowerShelf
type GetExpectedPowerShelfHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetExpectedPowerShelfHandler initializes and returns a new handler to retrieve ExpectedPowerShelf
func NewGetExpectedPowerShelfHandler(dbSession *cdb.Session, cfg *config.Config) GetExpectedPowerShelfHandler {
	return GetExpectedPowerShelfHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the ExpectedPowerShelf
// @Description Retrieve the ExpectedPowerShelf by ID
// @Tags ExpectedPowerShelf
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Power Shelf"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Success 200 {object} model.APIExpectedPowerShelf
// @Router /v2/org/{org}/nico/expected-power-shelf/{id} [get]
func (gepsh GetExpectedPowerShelfHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedPowerShelf", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Power Shelf ID from URL param
	expectedPowerShelfIDStr := c.Param("id")
	expectedPowerShelfID, err := uuid.Parse(expectedPowerShelfIDStr)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Power Shelf ID in URL", nil)
	}

	logger = logger.With().Str("ExpectedPowerShelfID", expectedPowerShelfID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_power_shelf_id", expectedPowerShelfID.String()))

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedPowerShelfRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Get ExpectedPowerShelf from DB by ID
	epsDAO := cdbm.NewExpectedPowerShelfDAO(gepsh.dbSession)
	expectedPowerShelf, err := epsDAO.Get(ctx, nil, expectedPowerShelfID, qIncludeRelations, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Power Shelf with ID: %s", expectedPowerShelfID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Power Shelf from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Power Shelf due to DB error", nil)
	}

	// Site is needed for the access check; reuse if loaded via includeRelation, else fetch.
	site := expectedPowerShelf.Site
	if site == nil {
		siteDAO := cdbm.NewSiteDAO(gepsh.dbSession)
		site, err = siteDAO.GetByID(ctx, nil, expectedPowerShelf.SiteID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Power Shelf due to DB error", nil)
		}
	}

	// Scope tenant privilege to the Expected Power Shelf's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gepsh.dbSession, org, dbUser, true, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gepsh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Power Shelf", nil)
	}

	// Create response
	apiExpectedPowerShelf := model.NewAPIExpectedPowerShelf(expectedPowerShelf)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiExpectedPowerShelf)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateExpectedPowerShelfHandler is the API Handler for updating a ExpectedPowerShelf
type UpdateExpectedPowerShelfHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateExpectedPowerShelfHandler initializes and returns a new handler for updating ExpectedPowerShelf
func NewUpdateExpectedPowerShelfHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateExpectedPowerShelfHandler {
	return UpdateExpectedPowerShelfHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing ExpectedPowerShelf
// @Description Update an existing ExpectedPowerShelf by ID
// @Tags ExpectedPowerShelf
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Power Shelf"
// @Param message body model.APIExpectedPowerShelfUpdateRequest true "ExpectedPowerShelf update request"
// @Success 200 {object} model.APIExpectedPowerShelf
// @Router /v2/org/{org}/nico/expected-power-shelf/{id} [patch]
func (uepsh UpdateExpectedPowerShelfHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedPowerShelf", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Power Shelf ID from URL param
	expectedPowerShelfID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Power Shelf ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedPowerShelfID", expectedPowerShelfID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_power_shelf_id", expectedPowerShelfID.String()))

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIExpectedPowerShelfUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ExpectedPowerShelf update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedPowerShelf update request data", verr)
	}

	// If ID is provided in body, it must match the path ID
	if apiRequest.ID != nil && *apiRequest.ID != expectedPowerShelfID.String() {
		logger.Warn().
			Str("URLID", expectedPowerShelfID.String()).
			Str("RequestDataID", *apiRequest.ID).
			Msg("Mismatched Expected Power Shelf ID between path and body")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "If provided, Expected Power Shelf ID specified in request data must match URL request value", nil)
	}

	// Get ExpectedPowerShelf from DB by ID
	epsDAO := cdbm.NewExpectedPowerShelfDAO(uepsh.dbSession)
	expectedPowerShelf, err := epsDAO.Get(ctx, nil, expectedPowerShelfID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Power Shelf with ID: %s", expectedPowerShelfID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Power Shelf from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Power Shelf due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Power Shelf
	site := expectedPowerShelf.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Power Shelf")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Power Shelf", nil)
	}

	// Scope tenant privilege to the Expected Power Shelf's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, uepsh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, uepsh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Power Shelf", nil)
	}

	if !bmcMacUnchanged(expectedPowerShelf.BmcMacAddress, apiRequest.BmcMacAddress) {
		validationErrors := bmcMacImmutableValidationError()
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Power Shelf update data", validationErrors)
	}

	updatedExpectedPowerShelf, err := cdb.WithTxResult(ctx, uepsh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedPowerShelf, error) {
		// Note: DefaultBmcUsername and BmcPassword are not stored in DB, only passed to workflow
		eps, err := epsDAO.Update(
			ctx,
			tx,
			cdbm.ExpectedPowerShelfUpdateInput{
				ExpectedPowerShelfID: expectedPowerShelf.ID,
				ShelfSerialNumber:    apiRequest.ShelfSerialNumber,
				BmcIpAddress:         apiRequest.BmcIpAddress,
				RackID:               apiRequest.RackID,
				Name:                 apiRequest.Name,
				Manufacturer:         apiRequest.Manufacturer,
				Model:                apiRequest.Model,
				Description:          apiRequest.Description,
				SlotID:               apiRequest.SlotID,
				TrayIdx:              apiRequest.TrayIdx,
				HostID:               apiRequest.HostID,
				Labels:               apiRequest.Labels,
			},
		)
		if err != nil {
			logger.Error().Err(err).Msg("failed to update ExpectedPowerShelf record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Power Shelf due to DB error", nil)
		}

		updateExpectedPowerShelfRequest := eps.ToProto(cdbm.ExpectedPowerShelfCredentials{
			Username: apiRequest.DefaultBmcUsername,
			Password: apiRequest.DefaultBmcPassword,
		})

		logger.Info().Msg("triggering ExpectedPowerShelf update workflow")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-power-shelf-update-" + expectedPowerShelf.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := uepsh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "UpdateExpectedPowerShelf", workflowOptions, updateExpectedPowerShelfRequest); apiErr != nil {
			return nil, apiErr
		}
		return eps, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Expected Power Shelf due to DB transaction error")
	}

	apiExpectedPowerShelf := model.NewAPIExpectedPowerShelf(updatedExpectedPowerShelf)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiExpectedPowerShelf)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteExpectedPowerShelfHandler is the API Handler for deleting a ExpectedPowerShelf
type DeleteExpectedPowerShelfHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteExpectedPowerShelfHandler initializes and returns a new handler for deleting ExpectedPowerShelf
func NewDeleteExpectedPowerShelfHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteExpectedPowerShelfHandler {
	return DeleteExpectedPowerShelfHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing ExpectedPowerShelf
// @Description Delete an existing ExpectedPowerShelf by ID
// @Tags ExpectedPowerShelf
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Power Shelf"
// @Success 204
// @Router /v2/org/{org}/nico/expected-power-shelf/{id} [delete]
func (depsh DeleteExpectedPowerShelfHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedPowerShelf", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Power Shelf ID from URL param
	expectedPowerShelfID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Power Shelf ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedPowerShelfID", expectedPowerShelfID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_power_shelf_id", expectedPowerShelfID.String()))

	// Get ExpectedPowerShelf from DB by ID
	epsDAO := cdbm.NewExpectedPowerShelfDAO(depsh.dbSession)
	expectedPowerShelf, err := epsDAO.Get(ctx, nil, expectedPowerShelfID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Power Shelf with ID: %s", expectedPowerShelfID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Power Shelf from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Power Shelf due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Power Shelf
	site := expectedPowerShelf.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Power Shelf")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Power Shelf", nil)
	}

	// Scope tenant privilege to the Expected Power Shelf's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, depsh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, depsh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Power Shelf", nil)
	}

	err = cdb.WithTx(ctx, depsh.dbSession, func(tx *cdb.Tx) error {
		if err := epsDAO.Delete(ctx, tx, expectedPowerShelf.ID); err != nil {
			logger.Error().Err(err).Msg("unable to delete ExpectedPowerShelf record from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Power Shelf due to DB error", nil)
		}

		deleteExpectedPowerShelfRequest := &corev1.ExpectedPowerShelfRequest{
			ExpectedPowerShelfId: &corev1.UUID{Value: expectedPowerShelf.ID.String()},
			BmcMacAddress:        expectedPowerShelf.BmcMacAddress,
		}

		logger.Info().Msg("triggering ExpectedPowerShelf delete workflow")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-power-shelf-delete-" + expectedPowerShelf.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := depsh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "DeleteExpectedPowerShelf", workflowOptions, deleteExpectedPowerShelfRequest); apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Power Shelf due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")

	return c.NoContent(http.StatusNoContent)
}
