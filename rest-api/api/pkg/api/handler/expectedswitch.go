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

// CreateExpectedSwitchHandler is the API Handler for creating new ExpectedSwitch
type CreateExpectedSwitchHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateExpectedSwitchHandler initializes and returns a new handler for creating ExpectedSwitch
func NewCreateExpectedSwitchHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateExpectedSwitchHandler {
	return CreateExpectedSwitchHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an ExpectedSwitch
// @Description Create an ExpectedSwitch
// @Tags ExpectedSwitch
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIExpectedSwitchCreateRequest true "ExpectedSwitch creation request"
// @Success 201 {object} model.APIExpectedSwitch
// @Router /v2/org/{org}/nico/expected-switch [post]
func (cesh CreateExpectedSwitchHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedSwitch", "Create", c)
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
	apiRequest := model.APIExpectedSwitchCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Expected Switch creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Switch creation data", verr)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cesh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	// Scope tenant privilege to the Site targeted by this request.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, cesh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, cesh.dbSession, site, infrastructureProvider, tenant)
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
	esDAO := cdbm.NewExpectedSwitchDAO(cesh.dbSession)
	ess, _, err := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{
		BmcMacAddresses: []string{apiRequest.BmcMacAddress},
		SiteIDs:         []uuid.UUID{site.ID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(1),
	}, nil)

	if err != nil {
		logger.Error().Err(err).Msg("error checking for duplicate MAC address on Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate MAC address uniqueness on Site due to DB error", nil)
	}

	if len(ess) > 0 {
		logger.Warn().Str("MacAddress", apiRequest.BmcMacAddress).Msg("Expected Switch with specified MAC address already exists on Site")

		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Switch with specified MAC address already exists on Site", validation.Errors{
			"id": errors.New(ess[0].ID.String()),
		})
	}

	// NVOS MACs identify the switch's management ports during discovery, so a
	// MAC claimed by another Expected Switch on the Site is a conflict.
	if len(apiRequest.NvosMacAddresses) > 0 {
		conflicts, _, derr := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{
			NvosMacAddresses: apiRequest.NvosMacAddresses,
			SiteIDs:          []uuid.UUID{site.ID},
		}, paginator.PageInput{
			Limit: cutil.GetPtr(1),
		}, nil)

		if derr != nil {
			logger.Error().Err(derr).Msg("error checking for duplicate NVOS MAC addresses on Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate NVOS MAC address uniqueness on Site due to DB error", nil)
		}

		if len(conflicts) > 0 {
			logger.Warn().Strs("NvosMacAddresses", apiRequest.NvosMacAddresses).Msg("Expected Switch with specified NVOS MAC address already exists on Site")

			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Switch with specified NVOS MAC address already exists on Site", validation.Errors{
				"id": errors.New(conflicts[0].ID.String()),
			})
		}
	}

	expectedSwitch, err := cdb.WithTxResult(ctx, cesh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedSwitch, error) {
		// Note: NvOsUsername and NvOsPassword are not stored in DB, only passed to workflow
		es, err := esDAO.Create(
			ctx,
			tx,
			cdbm.ExpectedSwitchCreateInput{
				ExpectedSwitchID:   uuid.New(),
				SiteID:             site.ID,
				BmcMacAddress:      apiRequest.BmcMacAddress,
				BmcIpAddress:       apiRequest.BmcIpAddress,
				SwitchSerialNumber: apiRequest.SwitchSerialNumber,
				NvosMacAddresses:   apiRequest.NvosMacAddresses,
				RackID:             apiRequest.RackID,
				Name:               apiRequest.Name,
				Manufacturer:       apiRequest.Manufacturer,
				Model:              apiRequest.Model,
				Description:        apiRequest.Description,
				SlotID:             apiRequest.SlotID,
				TrayIdx:            apiRequest.TrayIdx,
				HostID:             apiRequest.HostID,
				Labels:             apiRequest.Labels,
				CreatedBy:          dbUser.ID,
			},
		)
		if err != nil {
			logger.Error().Err(err).Msg("error creating ExpectedSwitch record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Expected Switch due to DB error", nil)
		}

		createExpectedSwitchRequest := es.ToProto(cdbm.ExpectedSwitchCredentials{
			BmcUsername:  apiRequest.DefaultBmcUsername,
			BmcPassword:  apiRequest.DefaultBmcPassword,
			NvosUsername: apiRequest.NvOsUsername,
			NvosPassword: apiRequest.NvOsPassword,
		})

		logger.Info().Msg("triggering Expected Switch create workflow on Site")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-switch-create-" + es.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := cesh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "CreateExpectedSwitch", workflowOptions, createExpectedSwitchRequest); apiErr != nil {
			return nil, apiErr
		}
		return es, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Expected Switch due to DB transaction error")
	}

	apiExpectedSwitch := model.NewAPIExpectedSwitch(expectedSwitch)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiExpectedSwitch)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllExpectedSwitchHandler is the API Handler for getting all ExpectedSwitches
type GetAllExpectedSwitchHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAllExpectedSwitchHandler initializes and returns a new handler for getting all ExpectedSwitches
func NewGetAllExpectedSwitchHandler(dbSession *cdb.Session, cfg *config.Config) GetAllExpectedSwitchHandler {
	return GetAllExpectedSwitchHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all ExpectedSwitches
// @Description Get all ExpectedSwitches. Provider callers may omit siteId to list across their Sites; Tenant callers must specify siteId.
// @Tags ExpectedSwitch
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site (optional, filters results to specific site)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIExpectedSwitch
// @Router /v2/org/{org}/nico/expected-switch [get]
func (gaesh GetAllExpectedSwitchHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedSwitch", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	filterInput := cdbm.ExpectedSwitchFilterInput{}

	// Get Site ID from query param if specified
	siteIDStr := c.QueryParam("siteId")
	var site *cdbm.Site
	var err error
	var privilegeScope *common.TenantPrivilegeScope
	if siteIDStr != "" {
		site, err = common.GetSiteFromIDString(ctx, nil, siteIDStr, gaesh.dbSession)
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
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gaesh.dbSession, org, dbUser, true, privilegeScope)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if site != nil {
		// Validate ProviderTenantSite relationship and site state
		hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gaesh.dbSession, site, infrastructureProvider, tenant)
		if apiError != nil {
			return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
		}

		if !hasAccess {
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
		}

		filterInput.SiteIDs = []uuid.UUID{site.ID}
	} else if tenant != nil && infrastructureProvider == nil {
		// Tenant-only callers must specify a Site ID.
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site ID must be specified in query when retrieving Expected Switches as a Tenant", nil)
	} else {
		// Get all Sites for the org's Infrastructure Provider
		siteDAO := cdbm.NewSiteDAO(gaesh.dbSession)
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
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedSwitchRelatedEntities)
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
	err = pageRequest.Validate(cdbm.ExpectedSwitchOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Expected Switches from DB
	esDAO := cdbm.NewExpectedSwitchDAO(gaesh.dbSession)
	expectedSwitches, total, err := esDAO.GetAll(
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
		logger.Error().Err(err).Msg("error retrieving Expected Switches from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Switches due to DB error", nil)
	}

	// Create response
	apiExpectedSwitches := []*model.APIExpectedSwitch{}
	for _, es := range expectedSwitches {
		apiExpectedSwitch := model.NewAPIExpectedSwitch(&es)
		apiExpectedSwitches = append(apiExpectedSwitches, apiExpectedSwitch)
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

	return c.JSON(http.StatusOK, apiExpectedSwitches)
}

// ~~~~~ Get Handler ~~~~~ //

// GetExpectedSwitchHandler is the API Handler for retrieving ExpectedSwitch
type GetExpectedSwitchHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetExpectedSwitchHandler initializes and returns a new handler to retrieve ExpectedSwitch
func NewGetExpectedSwitchHandler(dbSession *cdb.Session, cfg *config.Config) GetExpectedSwitchHandler {
	return GetExpectedSwitchHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the ExpectedSwitch
// @Description Retrieve the ExpectedSwitch by ID
// @Tags ExpectedSwitch
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Switch"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site'"
// @Success 200 {object} model.APIExpectedSwitch
// @Router /v2/org/{org}/nico/expected-switch/{id} [get]
func (gesh GetExpectedSwitchHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedSwitch", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Switch ID from URL param
	expectedSwitchIDStr := c.Param("id")
	expectedSwitchID, err := uuid.Parse(expectedSwitchIDStr)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Switch ID in URL", nil)
	}

	logger = logger.With().Str("ExpectedSwitchID", expectedSwitchID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_switch_id", expectedSwitchID.String()))

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedSwitchRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Get ExpectedSwitch from DB by ID
	esDAO := cdbm.NewExpectedSwitchDAO(gesh.dbSession)
	expectedSwitch, err := esDAO.Get(ctx, nil, expectedSwitchID, qIncludeRelations, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Switch with ID: %s", expectedSwitchID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Switch from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Switch due to DB error", nil)
	}

	// Site is needed for the access check; reuse if loaded via includeRelation, else fetch.
	site := expectedSwitch.Site
	if site == nil {
		siteDAO := cdbm.NewSiteDAO(gesh.dbSession)
		site, err = siteDAO.GetByID(ctx, nil, expectedSwitch.SiteID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Switch due to DB error", nil)
		}
	}

	// Scope tenant privilege to the Expected Switch's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gesh.dbSession, org, dbUser, true, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gesh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Switch", nil)
	}

	// Create response
	apiExpectedSwitch := model.NewAPIExpectedSwitch(expectedSwitch)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiExpectedSwitch)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateExpectedSwitchHandler is the API Handler for updating a ExpectedSwitch
type UpdateExpectedSwitchHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateExpectedSwitchHandler initializes and returns a new handler for updating ExpectedSwitch
func NewUpdateExpectedSwitchHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateExpectedSwitchHandler {
	return UpdateExpectedSwitchHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing ExpectedSwitch
// @Description Update an existing ExpectedSwitch by ID
// @Tags ExpectedSwitch
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Switch"
// @Param message body model.APIExpectedSwitchUpdateRequest true "ExpectedSwitch update request"
// @Success 200 {object} model.APIExpectedSwitch
// @Router /v2/org/{org}/nico/expected-switch/{id} [patch]
func (uesh UpdateExpectedSwitchHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedSwitch", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Switch ID from URL param
	expectedSwitchID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Switch ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedSwitchID", expectedSwitchID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_switch_id", expectedSwitchID.String()))

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIExpectedSwitchUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ExpectedSwitch update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedSwitch update request data", verr)
	}

	// If ID is provided in body, it must match the path ID
	if apiRequest.ID != nil && *apiRequest.ID != expectedSwitchID.String() {
		logger.Warn().
			Str("URLID", expectedSwitchID.String()).
			Str("RequestDataID", *apiRequest.ID).
			Msg("Mismatched Expected Switch ID between path and body")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "If provided, Expected Switch ID specified in request data must match URL request value", nil)
	}

	// Get ExpectedSwitch from DB by ID
	esDAO := cdbm.NewExpectedSwitchDAO(uesh.dbSession)
	expectedSwitch, err := esDAO.Get(ctx, nil, expectedSwitchID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Switch with ID: %s", expectedSwitchID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Switch from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Switch due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Switch
	site := expectedSwitch.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Switch")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Switch", nil)
	}

	// Scope tenant privilege to the Expected Switch's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, uesh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, uesh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Switch", nil)
	}

	if !bmcMacUnchanged(expectedSwitch.BmcMacAddress, apiRequest.BmcMacAddress) {
		validationErrors := bmcMacImmutableValidationError()
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Switch update data", validationErrors)
	}

	// NVOS MACs identify the switch's management ports during discovery, so a
	// MAC claimed by another Expected Switch on the Site is a conflict. The
	// switch being updated is excluded so re-asserting its own MACs stays valid.
	if len(apiRequest.NvosMacAddresses) > 0 {
		conflicts, _, derr := esDAO.GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{
			NvosMacAddresses:         apiRequest.NvosMacAddresses,
			SiteIDs:                  []uuid.UUID{site.ID},
			ExcludeExpectedSwitchIDs: []uuid.UUID{expectedSwitch.ID},
		}, paginator.PageInput{
			Limit: cutil.GetPtr(1),
		}, nil)

		if derr != nil {
			logger.Error().Err(derr).Msg("error checking for duplicate NVOS MAC addresses on Site")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to validate NVOS MAC address uniqueness on Site due to DB error", nil)
		}

		if len(conflicts) > 0 {
			logger.Warn().Strs("NvosMacAddresses", apiRequest.NvosMacAddresses).Msg("Expected Switch with specified NVOS MAC address already exists on Site")

			return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Switch with specified NVOS MAC address already exists on Site", validation.Errors{
				"id": errors.New(conflicts[0].ID.String()),
			})
		}
	}

	updatedExpectedSwitch, err := cdb.WithTxResult(ctx, uesh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedSwitch, error) {
		// Note: NvOsUsername and NvOsPassword are not stored in DB, only passed to workflow
		es, err := esDAO.Update(
			ctx,
			tx,
			cdbm.ExpectedSwitchUpdateInput{
				ExpectedSwitchID:   expectedSwitch.ID,
				BmcIpAddress:       apiRequest.BmcIpAddress,
				SwitchSerialNumber: apiRequest.SwitchSerialNumber,
				NvosMacAddresses:   apiRequest.NvosMacAddresses,
				RackID:             apiRequest.RackID,
				Name:               apiRequest.Name,
				Manufacturer:       apiRequest.Manufacturer,
				Model:              apiRequest.Model,
				Description:        apiRequest.Description,
				SlotID:             apiRequest.SlotID,
				TrayIdx:            apiRequest.TrayIdx,
				HostID:             apiRequest.HostID,
				Labels:             apiRequest.Labels,
			},
		)
		if err != nil {
			logger.Error().Err(err).Msg("failed to update ExpectedSwitch record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Switch due to DB error", nil)
		}

		updateExpectedSwitchRequest := es.ToProto(cdbm.ExpectedSwitchCredentials{
			BmcUsername:  apiRequest.DefaultBmcUsername,
			BmcPassword:  apiRequest.DefaultBmcPassword,
			NvosUsername: apiRequest.NvOsUsername,
			NvosPassword: apiRequest.NvOsPassword,
		})

		logger.Info().Msg("triggering ExpectedSwitch update workflow")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-switch-update-" + expectedSwitch.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := uesh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "UpdateExpectedSwitch", workflowOptions, updateExpectedSwitchRequest); apiErr != nil {
			return nil, apiErr
		}
		return es, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Expected Switch due to DB transaction error")
	}

	apiExpectedSwitch := model.NewAPIExpectedSwitch(updatedExpectedSwitch)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiExpectedSwitch)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteExpectedSwitchHandler is the API Handler for deleting a ExpectedSwitch
type DeleteExpectedSwitchHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteExpectedSwitchHandler initializes and returns a new handler for deleting ExpectedSwitch
func NewDeleteExpectedSwitchHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteExpectedSwitchHandler {
	return DeleteExpectedSwitchHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing ExpectedSwitch
// @Description Delete an existing ExpectedSwitch by ID
// @Tags ExpectedSwitch
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Switch"
// @Success 204
// @Router /v2/org/{org}/nico/expected-switch/{id} [delete]
func (desh DeleteExpectedSwitchHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedSwitch", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Switch ID from URL param
	expectedSwitchID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Switch ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedSwitchID", expectedSwitchID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_switch_id", expectedSwitchID.String()))

	// Get ExpectedSwitch from DB by ID
	esDAO := cdbm.NewExpectedSwitchDAO(desh.dbSession)
	expectedSwitch, err := esDAO.Get(ctx, nil, expectedSwitchID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Switch with ID: %s", expectedSwitchID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Switch from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Switch due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Switch
	site := expectedSwitch.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Switch")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Switch", nil)
	}

	// Scope tenant privilege to the Expected Switch's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, desh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, desh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Switch", nil)
	}

	err = cdb.WithTx(ctx, desh.dbSession, func(tx *cdb.Tx) error {
		if err := esDAO.Delete(ctx, tx, expectedSwitch.ID); err != nil {
			logger.Error().Err(err).Msg("unable to delete ExpectedSwitch record from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Switch due to DB error", nil)
		}

		deleteExpectedSwitchRequest := &corev1.ExpectedSwitchRequest{
			ExpectedSwitchId: &corev1.UUID{Value: expectedSwitch.ID.String()},
			BmcMacAddress:    expectedSwitch.BmcMacAddress,
		}

		logger.Info().Msg("triggering ExpectedSwitch delete workflow")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-switch-delete-" + expectedSwitch.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := desh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "DeleteExpectedSwitch", workflowOptions, deleteExpectedSwitchRequest); apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Switch due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")

	return c.NoContent(http.StatusNoContent)
}
