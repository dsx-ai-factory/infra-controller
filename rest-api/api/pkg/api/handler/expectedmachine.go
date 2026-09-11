// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"math"
	"net"
	"net/http"
	"slices"
	"strconv"
	"strings"

	mapset "github.com/deckarep/golang-set/v2"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
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

// ValidateProviderOrTenantSiteAccess validates if the provider or tenant has access to the site
func ValidateProviderOrTenantSiteAccess(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, site *cdbm.Site, infrastructureProvider *cdbm.InfrastructureProvider, tenant *cdbm.Tenant) (bool, *cutil.APIError) {
	providerHasAccess := infrastructureProvider != nil && site.InfrastructureProviderID == infrastructureProvider.ID
	tenantHasAccess := false

	if tenant != nil {
		// Effective TargetedInstanceCreation for this Site governs tenant access.
		// A TenantSite association alone must not bypass an explicit false override.
		var err error
		tenantHasAccess, err = common.TenantHasTargetedInstanceCreation(ctx, nil, dbSession, tenant, &common.TenantPrivilegeScope{SiteID: &site.ID})
		if err != nil {
			logger.Error().Err(err).Msg("error resolving TargetedInstanceCreation for Tenant/Site")
			return false, cutil.NewAPIError(http.StatusInternalServerError, "Failed to resolve Tenant capability for Site due to DB error", nil)
		}
	}

	return providerHasAccess || tenantHasAccess, nil
}

// ~~~~~ Create Handler ~~~~~ //

// CreateExpectedMachineHandler is the API Handler for creating new ExpectedMachine
type CreateExpectedMachineHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateExpectedMachineHandler initializes and returns a new handler for creating ExpectedMachine
func NewCreateExpectedMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateExpectedMachineHandler {
	return CreateExpectedMachineHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create an ExpectedMachine
// @Description Create an ExpectedMachine
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIExpectedMachineCreateRequest true "ExpectedMachine creation request"
// @Success 201 {object} model.APIExpectedMachine
// @Router /v2/org/{org}/nico/expected-machine [post]
func (cemh CreateExpectedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "Create", c)
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
	apiRequest := model.APIExpectedMachineCreateRequest{}
	err := c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating Expected Machine creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine creation data", verr)
	}

	// Validate that SKU exists if specified
	if apiRequest.SkuID != nil {
		skuDAO := cdbm.NewSkuDAO(cemh.dbSession)
		_, err = skuDAO.Get(ctx, nil, *apiRequest.SkuID)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				logger.Warn().Msg("SKU ID specified in request does not exist")
				return cutil.NewAPIErrorResponse(c, http.StatusUnprocessableEntity, "SKU ID specified in request does not exist", nil)
			}
			logger.Warn().Err(err).Msg("error validating SKU ID in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate SKU ID in request data due to DB error", nil)
		}
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cemh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site ID specified in request data is not valid", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	// Scope tenant privilege to the Site targeted by this request.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, cemh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, cemh.dbSession, site, infrastructureProvider, tenant)
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
	emDAO := cdbm.NewExpectedMachineDAO(cemh.dbSession)
	ems, count, err := emDAO.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{
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
		logger.Warn().Str("MacAddress", apiRequest.BmcMacAddress).Msg("Expected Machine with specified MAC address already exists on Site")

		return cutil.NewAPIErrorResponse(c, http.StatusConflict, "Expected Machine with specified MAC address already exists on Site", validation.Errors{
			"id": errors.New(ems[0].ID.String()),
		})
	}

	expectedMachine, err := cdb.WithTxResult(ctx, cemh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedMachine, error) {
		// Note: DefaultBmcUsername and BmcPassword are not stored in DB, only passed to workflow
		em, err := emDAO.Create(
			ctx,
			tx,
			cdbm.ExpectedMachineCreateInput{
				ExpectedMachineID:        uuid.New(),
				SiteID:                   site.ID,
				BmcMacAddress:            apiRequest.BmcMacAddress,
				BmcIpAddress:             apiRequest.BmcIpAddress,
				ChassisSerialNumber:      apiRequest.ChassisSerialNumber,
				SkuID:                    apiRequest.SkuID,
				FallbackDpuSerialNumbers: apiRequest.FallbackDPUSerialNumbers,
				RackID:                   apiRequest.RackID,
				Name:                     apiRequest.Name,
				Manufacturer:             apiRequest.Manufacturer,
				Model:                    apiRequest.Model,
				Description:              apiRequest.Description,
				SlotID:                   apiRequest.SlotID,
				TrayIdx:                  apiRequest.TrayIdx,
				HostID:                   apiRequest.HostID,
				IsDpfEnabled:             apiRequest.IsDpfEnabled,
				Labels:                   apiRequest.Labels,
				HostLifecycleProfile:     apiRequest.HostLifecycleProfile.ToDBModel(),
				CreatedBy:                dbUser.ID,
			},
		)
		if err != nil {
			logger.Error().Err(err).Msg("error creating ExpectedMachine record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Expected Machine due to DB error", nil)
		}

		createExpectedMachineRequest := em.ToProto(cdbm.ExpectedMachineCredentials{
			Username: apiRequest.DefaultBmcUsername,
			Password: apiRequest.DefaultBmcPassword,
		})

		logger.Info().Msg("triggering Expected Machine create workflow on Site")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-machine-create-" + em.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := cemh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "CreateExpectedMachine", workflowOptions, createExpectedMachineRequest); apiErr != nil {
			return nil, apiErr
		}
		return em, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Expected Machine due to DB transaction error")
	}

	apiExpectedMachine := model.NewAPIExpectedMachine(expectedMachine)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiExpectedMachine)
}

// ~~~~~ GetAll Handler ~~~~~ //

// GetAllExpectedMachineHandler is the API Handler for getting all ExpectedMachines
type GetAllExpectedMachineHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetAllExpectedMachineHandler initializes and returns a new handler for getting all ExpectedMachines
func NewGetAllExpectedMachineHandler(dbSession *cdb.Session, cfg *config.Config) GetAllExpectedMachineHandler {
	return GetAllExpectedMachineHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all ExpectedMachines
// @Description Get all ExpectedMachines. Tenant results are restricted to Sites with effective TargetedInstanceCreation; no single-Site privilege scope is required.
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string false "ID of Site (optional, filters results to specific site)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'SKU'"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {object} []model.APIExpectedMachine
// @Router /v2/org/{org}/nico/expected-machine [get]
func (gaemh GetAllExpectedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// ensure our user is a provider or tenant for the org. We do not request the
	// privileged-tenant pre-gate (requirePrivilegedScope=nil): tenant-only
	// callers are scoped below to Sites with effective TargetedInstanceCreation,
	// and receive 403 when none are resolved.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gaemh.dbSession, org, dbUser, true, nil)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Initialize SiteIDs to a non-nil empty slice so an unscoped caller (e.g. a
	// non-privileged Tenant) matches no Sites instead of every Site.
	siteIDs := mapset.NewSet[uuid.UUID]()

	if infrastructureProvider != nil {
		// Get all Sites for the org's Infrastructure Provider
		siteDAO := cdbm.NewSiteDAO(gaemh.dbSession)
		sites, _, err := siteDAO.GetAll(ctx, nil,
			cdbm.SiteFilterInput{InfrastructureProviderIDs: []uuid.UUID{infrastructureProvider.ID}},
			paginator.PageInput{Limit: cutil.GetPtr(math.MaxInt)},
			nil,
		)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Sites from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Sites for org due to DB error", nil)
		}

		for _, site := range sites {
			siteIDs.Add(site.ID)
		}
	}

	if tenant != nil {
		// Scope tenant listing to Sites where the Tenant has effective
		// TargetedInstanceCreation, honoring per-site TenantSite.config overrides.
		privilegedSiteIDs, err := common.GetPrivilegedAccessSiteIDsForTenant(ctx, nil, gaemh.dbSession, tenant)
		if err != nil {
			logger.Error().Err(err).Msg("error resolving privileged Site access for Tenant")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to resolve Tenant capability due to DB error", nil)
		}
		for _, siteID := range privilegedSiteIDs {
			siteIDs.Add(siteID)
		}
	}

	if infrastructureProvider == nil && tenant != nil && siteIDs.Cardinality() == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Tenant does not have Targeted Instance Creation capability enabled for any Site", nil)
	}

	siteIDStr := c.QueryParam("siteId")
	if siteIDStr != "" {
		site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, gaemh.dbSession)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
			}
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
		}

		// Validate Site association with org
		isAssociated := false
		if infrastructureProvider != nil {
			// Check if Site belongs to org's Infrastructure Provider
			if site.InfrastructureProviderID == infrastructureProvider.ID {
				isAssociated = true
			}
		}

		if !isAssociated && tenant != nil {
			// filterInput.SiteIDs already holds the Tenant's effective privileged Sites.
			isAssociated = siteIDs.Contains(site.ID)
		}

		if isAssociated {
			siteIDs.Clear()
			siteIDs.Add(site.ID)
		} else {
			logger.Error().Msg("Site is not associated with org")
			return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site specified in query", nil)
		}
	}

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedMachineRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err := c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination attributes
	err = pageRequest.Validate(cdbm.ExpectedMachineOrderByFields)
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get Expected Machines from DB
	emDAO := cdbm.NewExpectedMachineDAO(gaemh.dbSession)
	expectedMachines, total, err := emDAO.GetAll(
		ctx,
		nil,
		cdbm.ExpectedMachineFilterInput{
			SiteIDs: siteIDs.ToSlice(),
		},
		paginator.PageInput{
			Offset:  pageRequest.Offset,
			Limit:   pageRequest.Limit,
			OrderBy: pageRequest.OrderBy,
		}, qIncludeRelations,
	)
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Expected Machines from db")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machines due to DB error", nil)
	}

	// Create response
	apiExpectedMachines := []*model.APIExpectedMachine{}
	for _, em := range expectedMachines {
		apiExpectedMachine := model.NewAPIExpectedMachine(&em)
		apiExpectedMachines = append(apiExpectedMachines, apiExpectedMachine)
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

	return c.JSON(http.StatusOK, apiExpectedMachines)
}

// ~~~~~ Get Handler ~~~~~ //

// GetExpectedMachineHandler is the API Handler for retrieving ExpectedMachine
type GetExpectedMachineHandler struct {
	dbSession *cdb.Session
	cfg       *config.Config
}

// NewGetExpectedMachineHandler initializes and returns a new handler to retrieve ExpectedMachine
func NewGetExpectedMachineHandler(dbSession *cdb.Session, cfg *config.Config) GetExpectedMachineHandler {
	return GetExpectedMachineHandler{
		dbSession: dbSession,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Retrieve the ExpectedMachine
// @Description Retrieve the ExpectedMachine by ID
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Machine"
// @Param includeRelation query string false "Related entities to include in response e.g. 'Site', 'SKU'"
// @Success 200 {object} model.APIExpectedMachine
// @Router /v2/org/{org}/nico/expected-machine/{id} [get]
func (gemh GetExpectedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Machine ID from URL param
	expectedMachineIDStr := c.Param("id")
	expectedMachineID, err := uuid.Parse(expectedMachineIDStr)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Machine ID in URL", nil)
	}

	logger = logger.With().Str("ExpectedMachineID", expectedMachineID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_machine_id", expectedMachineID.String()))

	// Get and validate includeRelation params
	qParams := c.QueryParams()
	qIncludeRelations, errStr := common.GetAndValidateQueryRelations(qParams, cdbm.ExpectedMachineRelatedEntities)
	if errStr != "" {
		logger.Warn().Msg(errStr)
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, errStr, nil)
	}

	// Get ExpectedMachine from DB by ID
	emDAO := cdbm.NewExpectedMachineDAO(gemh.dbSession)
	expectedMachine, err := emDAO.Get(ctx, nil, expectedMachineID, qIncludeRelations, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Machine with ID: %s", expectedMachineID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Machine from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machine due to DB error", nil)
	}

	// Site is needed for the access check; reuse if loaded via includeRelation, else fetch.
	site := expectedMachine.Site
	if site == nil {
		siteDAO := cdbm.NewSiteDAO(gemh.dbSession)
		site, err = siteDAO.GetByID(ctx, nil, expectedMachine.SiteID, nil, false)
		if err != nil {
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Machine due to DB error", nil)
		}
	}

	// Scope tenant privilege to the Expected Machine's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, gemh.dbSession, org, dbUser, true, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, gemh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Machine", nil)
	}

	// Create response
	apiExpectedMachine := model.NewAPIExpectedMachine(expectedMachine)

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiExpectedMachine)
}

// ~~~~~ Update Handler ~~~~~ //

// UpdateExpectedMachineHandler is the API Handler for updating a ExpectedMachine
type UpdateExpectedMachineHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// bmcMacUnchanged accepts an omitted MAC or another spelling of the stored MAC.
func bmcMacUnchanged(stored string, submitted *string) bool {
	if submitted == nil {
		return true
	}

	storedMAC, storedErr := net.ParseMAC(stored)
	submittedMAC, submittedErr := net.ParseMAC(*submitted)
	return storedErr == nil && submittedErr == nil && slices.Equal(storedMAC, submittedMAC)
}

// bmcMacImmutableValidationError keeps BMC MAC PATCH validation responses aligned.
func bmcMacImmutableValidationError() validation.Errors {
	return validation.Errors{
		"bmcMacAddress": errors.New("BMC MAC address cannot be changed after creation"),
	}
}

// NewUpdateExpectedMachineHandler initializes and returns a new handler for updating ExpectedMachine
func NewUpdateExpectedMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateExpectedMachineHandler {
	return UpdateExpectedMachineHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update an existing ExpectedMachine
// @Description Update an existing ExpectedMachine by ID
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Machine"
// @Param message body model.APIExpectedMachineUpdateRequest true "ExpectedMachine update request"
// @Success 200 {object} model.APIExpectedMachine
// @Router /v2/org/{org}/nico/expected-machine/{id} [patch]
func (uemh UpdateExpectedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Machine ID from URL param
	expectedMachineID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Machine ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedMachineID", expectedMachineID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_machine_id", expectedMachineID.String()))

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIExpectedMachineUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating ExpectedMachine update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate ExpectedMachine update request data", verr)
	}

	// If ID is provided in body, it must match the path ID
	if apiRequest.ID != nil && *apiRequest.ID != expectedMachineID.String() {
		logger.Warn().
			Str("URLID", expectedMachineID.String()).
			Str("RequestDataID", *apiRequest.ID).
			Msg("Mismatched Expected Machine ID between path and body")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "If provided, Expected Machine ID specified in request data must match URL request value", nil)
	}

	// Validate that SKU exists if specified
	if apiRequest.SkuID != nil {
		skuDAO := cdbm.NewSkuDAO(uemh.dbSession)
		_, err = skuDAO.Get(ctx, nil, *apiRequest.SkuID)
		if err != nil {
			if errors.Is(err, cdb.ErrDoesNotExist) {
				logger.Warn().Msg("SKU ID specified in request does not exist")
				return cutil.NewAPIErrorResponse(c, http.StatusUnprocessableEntity, "SKU ID specified in request does not exist", nil)
			}
			logger.Warn().Err(err).Msg("error validating SKU ID in request data")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate SKU ID in request data due to DB error", nil)
		}
	}

	// Get ExpectedMachine from DB by ID
	emDAO := cdbm.NewExpectedMachineDAO(uemh.dbSession)
	expectedMachine, err := emDAO.Get(ctx, nil, expectedMachineID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Machine with ID: %s", expectedMachineID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Machine from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machine due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Machine
	site := expectedMachine.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Machine")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Machine", nil)
	}

	// Scope tenant privilege to the Expected Machine's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, uemh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, uemh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Machine", nil)
	}

	if !bmcMacUnchanged(expectedMachine.BmcMacAddress, apiRequest.BmcMacAddress) {
		validationErrors := bmcMacImmutableValidationError()
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine update data", validationErrors)
	}

	bmcIPAddress := apiRequest.BmcIpAddress
	clearBmcIPAddress := bmcIPAddress != nil && *bmcIPAddress == ""
	if clearBmcIPAddress {
		bmcIPAddress = nil
	}

	updatedExpectedMachine, err := cdb.WithTxResult(ctx, uemh.dbSession, func(tx *cdb.Tx) (*cdbm.ExpectedMachine, error) {
		// Note: DefaultBmcUsername and BmcPassword are not stored in DB, only passed to workflow
		em, err := emDAO.Update(
			ctx,
			tx,
			cdbm.ExpectedMachineUpdateInput{
				ExpectedMachineID:        expectedMachine.ID,
				BmcIpAddress:             bmcIPAddress,
				ChassisSerialNumber:      apiRequest.ChassisSerialNumber,
				SkuID:                    apiRequest.SkuID,
				FallbackDpuSerialNumbers: apiRequest.FallbackDPUSerialNumbers,
				RackID:                   apiRequest.RackID,
				Name:                     apiRequest.Name,
				Manufacturer:             apiRequest.Manufacturer,
				Model:                    apiRequest.Model,
				Description:              apiRequest.Description,
				SlotID:                   apiRequest.SlotID,
				TrayIdx:                  apiRequest.TrayIdx,
				HostID:                   apiRequest.HostID,
				Labels:                   apiRequest.Labels,
				IsDpfEnabled:             apiRequest.IsDpfEnabled,
				HostLifecycleProfile:     apiRequest.HostLifecycleProfile.ToDBModelPtr(),
			},
		)
		if err != nil {
			logger.Error().Err(err).Msg("failed to update ExpectedMachine record in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Machine due to DB error", nil)
		}

		if clearBmcIPAddress {
			em, err = emDAO.Clear(ctx, tx, cdbm.ExpectedMachineClearInput{
				ExpectedMachineID: expectedMachine.ID,
				BmcIpAddress:      true,
			})
			if err != nil {
				logger.Error().Err(err).Msg("failed to clear ExpectedMachine BMC IP address in DB")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Machine due to DB error", nil)
			}
		}

		updateExpectedMachineRequest := em.ToProto(cdbm.ExpectedMachineCredentials{
			Username: apiRequest.DefaultBmcUsername,
			Password: apiRequest.DefaultBmcPassword,
		})
		// REST storage keeps its current value when this PATCH omits the
		// address. Core needs the request value instead: nil preserves its
		// current HostBmc configuration, while an empty string clears it.
		updateExpectedMachineRequest.BmcIpAddress = apiRequest.BmcIpAddress

		logger.Info().Msg("triggering ExpectedMachine update workflow")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-machine-update-" + expectedMachine.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := uemh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "UpdateExpectedMachine", workflowOptions, updateExpectedMachineRequest); apiErr != nil {
			return nil, apiErr
		}
		return em, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Expected Machine due to DB transaction error")
	}

	apiExpectedMachine := model.NewAPIExpectedMachine(updatedExpectedMachine)

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiExpectedMachine)
}

// ~~~~~ Delete Handler ~~~~~ //

// DeleteExpectedMachineHandler is the API Handler for deleting a ExpectedMachine
type DeleteExpectedMachineHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteExpectedMachineHandler initializes and returns a new handler for deleting ExpectedMachine
func NewDeleteExpectedMachineHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteExpectedMachineHandler {
	return DeleteExpectedMachineHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete an existing ExpectedMachine
// @Description Delete an existing ExpectedMachine by ID
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Expected Machine"
// @Success 204
// @Router /v2/org/{org}/nico/expected-machine/{id} [delete]
func (demh DeleteExpectedMachineHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Get Expected Machine ID from URL param
	expectedMachineID, err := uuid.Parse(c.Param("id"))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Expected Machine ID in URL", nil)
	}
	logger = logger.With().Str("ExpectedMachineID", expectedMachineID.String()).Logger()

	cotel.SetAttribute(handlerSpan, attribute.String("expected_machine_id", expectedMachineID.String()))

	// Get ExpectedMachine from DB by ID
	emDAO := cdbm.NewExpectedMachineDAO(demh.dbSession)
	expectedMachine, err := emDAO.Get(ctx, nil, expectedMachineID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusNotFound, fmt.Sprintf("Could not find Expected Machine with ID: %s", expectedMachineID.String()), nil)
		}
		logger.Error().Err(err).Msg("error retrieving Expected Machine from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machine due to DB error", nil)
	}

	// Validate that Site relation exists for the Expected Machine
	site := expectedMachine.Site
	if site == nil {
		logger.Error().Msg("no Site relation found for Expected Machine")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site details for Expected Machine", nil)
	}

	// Scope tenant privilege to the Expected Machine's Site.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, demh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and site state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, demh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site of the Expected Machine", nil)
	}

	err = cdb.WithTx(ctx, demh.dbSession, func(tx *cdb.Tx) error {
		if err := emDAO.Delete(ctx, tx, expectedMachine.ID); err != nil {
			logger.Error().Err(err).Msg("unable to delete ExpectedMachine record from DB")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to delete Expected Machine due to DB error", nil)
		}

		deleteExpectedMachineRequest := &corev1.ExpectedMachineRequest{
			Id: &corev1.UUID{Value: expectedMachine.ID.String()},
		}

		logger.Info().Msg("triggering ExpectedMachine delete workflow")

		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       "expected-machine-delete-" + expectedMachine.ID.String(),
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		stc, err := demh.scp.GetClientByID(site.ID)
		if err != nil {
			logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
			return cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		if apiErr := common.ExecuteSyncWorkflow(ctx, logger, stc, "DeleteExpectedMachine", workflowOptions, deleteExpectedMachineRequest); apiErr != nil {
			return apiErr
		}
		return nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to delete Expected Machine due to DB transaction error")
	}

	logger.Info().Msg("finishing API handler")

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ CreateExpectedMachines Handler ~~~~~ //

// CreateExpectedMachinesHandler is the API Handler for creating multiple ExpectedMachines
type CreateExpectedMachinesHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateExpectedMachinesHandler initializes and returns a new handler for creating multiple ExpectedMachines
func NewCreateExpectedMachinesHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateExpectedMachinesHandler {
	return CreateExpectedMachinesHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create multiple ExpectedMachines
// @Description Create multiple ExpectedMachines in a single request. All machines must belong to the same site.
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body []model.APIExpectedMachineCreateRequest true "ExpectedMachine batch creation request"
// @Success 201 {object} model.APIExpectedMachineBatchResponse
// @Router /v2/org/{org}/nico/expected-machine/batch [post]
func (cemh CreateExpectedMachinesHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "CreateMultiple", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate request
	// Bind request data to API model (array payload)
	apiRequests := []model.APIExpectedMachineCreateRequest{}
	err := c.Bind(&apiRequests)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	if len(apiRequests) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Request data must contain at least 1 Expected Machine entry", nil)
	}

	if len(apiRequests) > model.ExpectedMachineMaxBatchItems {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("At most %d Expected Machine entries can be created in a batch request", model.ExpectedMachineMaxBatchItems), nil)
	}

	// Validate each item, also collect requested sku IDs
	// - SiteID is required, a valid UUID and unique across all items
	// - BMC address is required and must be unique
	// - Serial Number is required and must be unique
	// Note: this is early partial validation before we try to call the DB.
	validationErrors := validation.Errors{} //
	var foundSiteID *uuid.UUID
	bmcMacMap := make(map[string]int)
	serialMap := make(map[string]int)
	requestedSkuIDs := make(map[string]bool)
	for i, req := range apiRequests {
		strIndex := strconv.Itoa(i) // index/key as string for validation errors map
		itemErrors := validation.Errors{}

		verr := req.Validate()
		if verr != nil {
			var ok bool
			itemErrors, ok = verr.(validation.Errors)
			if !ok {
				common.AddToValidationErrors(itemErrors, "validation", verr)
			}
		}

		// SiteID is mandatory for batch create and must be the same for all items
		if req.SiteID == "" {
			common.AddToValidationErrors(itemErrors, "siteID", fmt.Errorf("Site ID is required"))
		}
		siteID, _ := uuid.Parse(req.SiteID) // already validated
		if foundSiteID == nil {
			foundSiteID = &siteID
		} else {
			if siteID != *foundSiteID {
				common.AddToValidationErrors(itemErrors, "siteID", fmt.Errorf(
					"Expected Machine does not belong to the same Site (%s) as other Expected Machines in request", req.SiteID))
			}
		}

		lowerMac := strings.ToLower(req.BmcMacAddress)
		if prev, ok := bmcMacMap[lowerMac]; ok {
			common.AddToValidationErrors(itemErrors, "bmcMacAddress", fmt.Errorf(
				"duplicate BMC MAC address '%s' found at indices %d and %d", req.BmcMacAddress, prev, i))
		}
		bmcMacMap[lowerMac] = i

		lowerSerial := strings.ToLower(req.ChassisSerialNumber)
		if prev, ok := serialMap[lowerSerial]; ok {
			common.AddToValidationErrors(itemErrors, "chassisSerialNumber", fmt.Errorf(
				"duplicate chassis serial number '%s' found at indices %d and %d", req.ChassisSerialNumber, prev, i))
		}
		serialMap[lowerSerial] = i

		if req.SkuID != nil {
			requestedSkuIDs[*req.SkuID] = true
		}

		if len(itemErrors) > 0 {
			validationErrors[strIndex] = itemErrors
		}
	}
	if len(validationErrors) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine create data", validationErrors)
	}
	siteID := *foundSiteID

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, siteID.String(), cemh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request data due to DB error", nil)
	}

	// Scope tenant privilege to the common Site targeted by this batch.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, cemh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate access to Site
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, cemh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}
	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site", nil)
	}

	// Retrieve all ExpectedMachines on Site from DB to allow unicity checks at Site level.
	// This is a pure validation read, so it stays outside the write transaction.
	emDAO := cdbm.NewExpectedMachineDAO(cemh.dbSession)
	existingMachinesOnSite, _, err := emDAO.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{
		SiteIDs: []uuid.UUID{siteID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(paginator.TotalLimit), // we want ALL records on site
	}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Expected Machines from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machines due to DB error", nil)
	}
	existingMacAddressMap := make(map[string]bool)
	existingSerialMap := make(map[string]bool)
	for _, machine := range existingMachinesOnSite {
		existingMacAddressMap[machine.BmcMacAddress] = true
		existingSerialMap[machine.ChassisSerialNumber] = true
	}

	// Retrieve all SKUs on Site to validate existence of SKU IDs in request.
	// This is a pure validation read, so it stays outside the write transaction.
	skuDAO := cdbm.NewSkuDAO(cemh.dbSession)
	existingSkus, _, err := skuDAO.GetAll(ctx, nil, cdbm.SkuFilterInput{
		SiteIDs: []uuid.UUID{siteID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(len(requestedSkuIDs)),
	})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SKUs from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SKUs due to DB error", nil)
	}
	existingSkuIDsMap := make(map[string]bool)
	for _, sku := range existingSkus {
		existingSkuIDsMap[sku.ID] = true
	}

	// Final checks: unicity of MAC, Serial, and existence of SKUs
	validationErrors = validation.Errors{}
	for i, req := range apiRequests {
		itemErrors := validation.Errors{}
		strIndex := strconv.Itoa(i) // index/key as string for validation errors map

		// Check MAC unicity
		if existingMacAddressMap[req.BmcMacAddress] {
			common.AddToValidationErrors(itemErrors, "bmcMacAddress", fmt.Errorf(
				"Expected Machine with BMC MAC Address: %s already exist", req.BmcMacAddress))
		}
		// Check Serial unicity
		if existingSerialMap[req.ChassisSerialNumber] {
			common.AddToValidationErrors(itemErrors, "chassisSerialNumber", fmt.Errorf(
				"Expected Machine with Chassis Serial Number: %s already exists", req.ChassisSerialNumber))
		}
		// Check SKU existence
		if req.SkuID != nil && !existingSkuIDsMap[*req.SkuID] {
			common.AddToValidationErrors(itemErrors, "skuID", fmt.Errorf(
				"the SkuID specified for Expected Machine does not exist in DB: %s", *req.SkuID))
		}

		// Collect errors
		if len(itemErrors) > 0 {
			validationErrors[strIndex] = itemErrors
		}
	}
	if len(validationErrors) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine update data", validationErrors)
	}

	// Build the inputs and a credentials lookup keyed by the ExpectedMachineID
	// we generate here. After CreateMultiple returns we look credentials up by
	// the DB record's ID rather than by slice index, so correlation doesn't
	// depend on the DAO preserving input order.
	credsByID := make(map[uuid.UUID]cdbm.ExpectedMachineCredentials, len(apiRequests))
	createInputs := make([]cdbm.ExpectedMachineCreateInput, 0, len(apiRequests))
	for _, machineReq := range apiRequests {
		id := uuid.New()
		credsByID[id] = cdbm.ExpectedMachineCredentials{
			Username: machineReq.DefaultBmcUsername,
			Password: machineReq.DefaultBmcPassword,
		}
		createInputs = append(createInputs, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID:        id,
			SiteID:                   site.ID,
			BmcMacAddress:            machineReq.BmcMacAddress,
			BmcIpAddress:             machineReq.BmcIpAddress,
			ChassisSerialNumber:      machineReq.ChassisSerialNumber,
			SkuID:                    machineReq.SkuID,
			FallbackDpuSerialNumbers: machineReq.FallbackDPUSerialNumbers,
			RackID:                   machineReq.RackID,
			Name:                     machineReq.Name,
			Manufacturer:             machineReq.Manufacturer,
			Model:                    machineReq.Model,
			Description:              machineReq.Description,
			SlotID:                   machineReq.SlotID,
			TrayIdx:                  machineReq.TrayIdx,
			HostID:                   machineReq.HostID,
			Labels:                   machineReq.Labels,
			IsDpfEnabled:             machineReq.IsDpfEnabled,
			HostLifecycleProfile:     machineReq.HostLifecycleProfile.ToDBModel(),
			CreatedBy:                dbUser.ID,
		})
	}

	createdExpectedMachines, err := cdb.WithTxResult(ctx, cemh.dbSession, func(tx *cdb.Tx) ([]cdbm.ExpectedMachine, error) {
		createdMachines, derr := emDAO.CreateMultiple(ctx, tx, createInputs)
		if derr != nil {
			logger.Error().Err(derr).Msg("error creating ExpectedMachine records in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to create Expected Machine due to DB error", nil)
		}

		workflowMachines := make([]*corev1.ExpectedMachine, 0, len(createdMachines))
		for i := range createdMachines {
			em := &createdMachines[i]
			creds, ok := credsByID[em.ID]
			if !ok {
				// CreateMultiple returned an ID we didn't ask it to create.
				// This shouldn't actually happen, so fail loudly instead of
				// attaching the wrong credentials to a machine.
				logger.Error().Str("ExpectedMachineID", em.ID.String()).Msg("CreateMultiple returned a machine with an unrecognized ID")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to correlate created Expected Machine to request", nil)
			}
			workflowMachines = append(workflowMachines, em.ToProto(creds))
		}

		logger.Info().Int("Count", len(workflowMachines)).Msg("triggering CreateExpectedMachines workflow on Site")

		// Create workflow request
		workflowRequest := &corev1.BatchExpectedMachineOperationRequest{
			ExpectedMachines:     &corev1.ExpectedMachineList{ExpectedMachines: workflowMachines},
			AcceptPartialResults: false,
		}

		// Create workflow options. Include a UUID suffix so concurrent batches
		// of the same size on the same Site don't collide on a single ID.
		workflowID := fmt.Sprintf("create-expected-machines-%s-%s", site.ID.String(), uuid.New().String())
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       workflowID,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Get the temporal client for the site we are working with
		stc, cerr := cemh.scp.GetClientByID(site.ID)
		if cerr != nil {
			logger.Error().Err(cerr).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Execute workflow and get results
		workflowRun, werr := stc.ExecuteWorkflow(ctx, workflowOptions, "CreateExpectedMachines", workflowRequest)
		if werr != nil {
			logger.Error().Err(werr).Msg("failed to schedule CreateExpectedMachines workflow on Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to schedule batch Expected Machine creation workflow on Site: %v", werr), nil)
		}

		workflowRunID := workflowRun.GetID()
		logger = logger.With().Str("WorkflowID", workflowRunID).Logger()
		logger.Info().Msg("executing CreateExpectedMachines workflow on Site")

		// Get workflow results
		var workflowResult corev1.BatchExpectedMachineOperationResponse

		werr = workflowRun.Get(ctx, &workflowResult)
		if werr != nil {
			logger.Error().Err(werr).Msg("error executing CreateExpectedMachines workflow on Site")
			// Workflow failed entirely - don't commit transaction
			return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute batch Expected Machine creation workflow on Site: %v", werr), nil)
		}

		// sanity checks since this is all-or-nothing
		if len(workflowResult.GetResults()) != len(createdMachines) {
			logger.Error().Msgf("workflow returned a different number of Expected Machines (expected %d but got %d)", len(createdMachines), len(workflowResult.GetResults()))
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to verify batch Expected Machine creation workflow results", nil)
		}

		return createdMachines, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to create Expected Machines due to DB transaction error")
	}

	logger.Info().
		Int("SuccessCount", len(createdExpectedMachines)).
		Msg("finishing CreateExpectedMachines API handler")

	// Return only successful machines
	return c.JSON(http.StatusCreated, createdExpectedMachines)
}

// ~~~~~ Batch Update Handler ~~~~~ //

// UpdateExpectedMachinesHandler is the API Handler for batch updating ExpectedMachines
type UpdateExpectedMachinesHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateExpectedMachinesHandler initializes and returns a new handler for batch updating ExpectedMachines
func NewUpdateExpectedMachinesHandler(dbSession *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateExpectedMachinesHandler {
	return UpdateExpectedMachinesHandler{
		dbSession: dbSession,
		scp:       scp,
		cfg:       cfg,
	}
}

type expectedMachineUpdateFieldSet struct {
	bmcMacAddress            bool
	defaultBmcUsername       bool
	defaultBmcPassword       bool
	chassisSerialNumber      bool
	fallbackDPUSerialNumbers bool
	skuID                    bool
	rackID                   bool
	name                     bool
	manufacturer             bool
	model                    bool
	description              bool
	slotID                   bool
	trayIdx                  bool
	hostID                   bool
	isDpfEnabled             bool
	labels                   bool
	hostLifecycleProfile     bool
}

// expectedMachineUpdateFields records field presence across the complete update
// request without comparing values. Keep it in sync with
// `model.APIExpectedMachineUpdateRequest`: `ID` selects the row, and
// `bmcIpAddress` is the sole documented per-machine exception.
func expectedMachineUpdateFields(req model.APIExpectedMachineUpdateRequest) expectedMachineUpdateFieldSet {
	return expectedMachineUpdateFieldSet{
		bmcMacAddress:            req.BmcMacAddress != nil,
		defaultBmcUsername:       req.DefaultBmcUsername != nil,
		defaultBmcPassword:       req.DefaultBmcPassword != nil,
		chassisSerialNumber:      req.ChassisSerialNumber != nil,
		fallbackDPUSerialNumbers: req.FallbackDPUSerialNumbers != nil,
		skuID:                    req.SkuID != nil,
		rackID:                   req.RackID != nil,
		name:                     req.Name != nil,
		manufacturer:             req.Manufacturer != nil,
		model:                    req.Model != nil,
		description:              req.Description != nil,
		slotID:                   req.SlotID != nil,
		trayIdx:                  req.TrayIdx != nil,
		hostID:                   req.HostID != nil,
		isDpfEnabled:             req.IsDpfEnabled != nil,
		labels:                   req.Labels != nil,
		hostLifecycleProfile:     req.HostLifecycleProfile.ToDBModelPtr() != nil,
	}
}

// Handle godoc
// @Summary Batch update ExpectedMachines
// @Description Update multiple ExpectedMachines in a single request. All machines must belong to the same site.
// @Tags ExpectedMachine
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body []model.APIExpectedMachineUpdateRequest true "ExpectedMachine UpdateExpectedMachines request"
// @Success 200 {object} model.APIExpectedMachineBatchResponse
// @Router /v2/org/{org}/nico/expected-machine/batch [patch]
func (uemh UpdateExpectedMachinesHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("ExpectedMachine", "UpdateMultiple", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate request
	// Bind request data to API model (array payload)
	apiRequests := []model.APIExpectedMachineUpdateRequest{}
	err := c.Bind(&apiRequests)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	if len(apiRequests) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Request data must contain at least 1 Expected Machine entry", nil)
	}

	if len(apiRequests) > model.ExpectedMachineMaxBatchItems {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, fmt.Sprintf("At most %d Expected Machine entries can be created in a batch request", model.ExpectedMachineMaxBatchItems), nil)
	}

	// Validate each item, also collect requested sku IDs
	// - ID is required and must be unique
	// - Serial Number is optional but must be unique
	// Note: this is early partial validation before we try to call the DB.
	validationErrors := validation.Errors{} //
	idMap := make(map[uuid.UUID]int)        // Map Expected Machine ID to its index in the request array
	serialMap := make(map[string]int)
	requestedSkuIDs := make(map[string]bool)
	// The public batch contract requires one update field set. Enforce it before
	// `UpdateMultiple` can apply its shared column list to rows that omitted a field.
	batchFields := expectedMachineUpdateFields(apiRequests[0])
	for i, req := range apiRequests {
		strIndex := strconv.Itoa(i) // index/key as string for validation errors map
		itemErrors := validation.Errors{}

		verr := req.Validate()
		if verr != nil {
			var ok bool
			itemErrors, ok = verr.(validation.Errors)
			if !ok {
				common.AddToValidationErrors(itemErrors, "validation", verr)
			}
		}

		// validation must accept nil ID for single update use case so we need to check for nil ID here
		if req.ID == nil {
			common.AddToValidationErrors(itemErrors, "id", fmt.Errorf("Missing required Expected Machine ID"))
		} else {
			// extract already validated UUID
			mid, _ := uuid.Parse(*req.ID)

			if prev, ok := idMap[mid]; ok {
				common.AddToValidationErrors(itemErrors, "id", fmt.Errorf(
					"duplicate Expected Machine ID '%s' found at indices %d and %d", *req.ID, prev, i))
			}
			idMap[mid] = i
		}

		if req.ChassisSerialNumber != nil {
			lowerSerial := strings.ToLower(*req.ChassisSerialNumber)
			if prev, ok := serialMap[lowerSerial]; ok {
				common.AddToValidationErrors(itemErrors, "chassisSerialNumber", fmt.Errorf(
					"duplicate chassis serial number '%s' found at indices %d and %d", *req.ChassisSerialNumber, prev, i))
			}
			serialMap[lowerSerial] = i
		}

		if req.SkuID != nil {
			requestedSkuIDs[*req.SkuID] = true
		}

		if expectedMachineUpdateFields(req) != batchFields {
			common.AddToValidationErrors(itemErrors, "fields", errors.New("must provide the same set of fields as batch item 0, except bmcIpAddress"))
		}

		if len(itemErrors) > 0 {
			validationErrors[strIndex] = itemErrors
		}
	}
	if len(validationErrors) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine update data", validationErrors)
	}

	logger.Info().Int("MachineCount", len(apiRequests)).Msg("processing UpdateExpectedMachines request")

	// Since we only have a list of Expected Machine ID as input we can only learn the SiteIDs involved by querying the DB
	// but we also want to retrieve full Expected Machines from Site to check for Serial uniqueness.
	// We will split into multiple queries:
	// 1. Retrieve SiteID and Site by loading the requested ExpectedMachine records
	// 2. Retrieve all Expected Machines for that Site to check for Serial uniqueness.
	// 3. Retrieve all SKUs for that Site to validate SKU IDs in the request.
	// All of these are pure validation reads and stay outside the WithTxResult call below.
	// TODO: now that we have a unique index on (mac,siteID) we should reconsider adding unique indices on (serial,siteID).
	//       At this time it is expected that existing serial data may not be unique so we
	//       cannot add such an index without cleaning existing data first.

	// Retrieve the requested Expected Machines so we can resolve the SiteID
	// and validate they all belong to the same Site. This is a pure
	// validation read, so it stays outside the write transaction.
	emDAO := cdbm.NewExpectedMachineDAO(uemh.dbSession)
	requestedExpectedMachine, _, err := emDAO.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{
		ExpectedMachineIDs: slices.Collect(maps.Keys(idMap)),
	}, paginator.PageInput{
		Limit: cutil.GetPtr(paginator.TotalLimit),
	}, []string{cdbm.SiteRelationName})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Expected Machines from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machines due to DB error", nil)
	}
	if len(requestedExpectedMachine) == 0 {
		logger.Warn().Msg("No Expected Machines found for provided IDs")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "No Expected Machines found for provided IDs", nil)
	}
	requestedEmMap := make(map[uuid.UUID]cdbm.ExpectedMachine)
	for i := range requestedExpectedMachine {
		em := &requestedExpectedMachine[i]
		requestedEmMap[em.ID] = *em
	}

	// Iterate on retrieved records to check siteID and IDs
	var foundSiteID *uuid.UUID
	validationErrors = validation.Errors{}
	for i, req := range apiRequests {
		strIndex := strconv.Itoa(i) // index/key as string for validation errors map
		itemErrors := validation.Errors{}

		// Check ID
		mid, _ := uuid.Parse(*req.ID) // we know the ID in request is valid from previous validation
		em, ok := requestedEmMap[mid]
		if !ok {
			common.AddToValidationErrors(itemErrors, "id", fmt.Errorf(
				"Expected Machine with ID %s not found in DB", *req.ID))
			validationErrors[strIndex] = itemErrors
			continue
		}

		// Check SiteID consistency
		if foundSiteID == nil {
			foundSiteID = &em.SiteID
		} else {
			if em.SiteID != *foundSiteID {
				common.AddToValidationErrors(itemErrors, "siteID", fmt.Errorf(
					"Expected Machine with ID %s does not belong to the same Site (%s) as other Expected Machines in request", *req.ID, *foundSiteID))
			}
		}

		if len(itemErrors) > 0 {
			validationErrors[strIndex] = itemErrors
		}
	}
	if len(validationErrors) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine update data", validationErrors)
	}

	// Get our unique Site ID and Site record
	siteID := requestedExpectedMachine[0].SiteID
	site := requestedExpectedMachine[0].Site // we get the site record from the relation loaded with our Expected Machine
	if site == nil {
		logger.Warn().Msg("No Site relation found for Expected Machines")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "No Site found for Expected Machines", nil)
	}

	// Scope tenant privilege to the common Site targeted by this batch.
	infrastructureProvider, tenant, apiError := common.IsProviderOrTenant(ctx, logger, uemh.dbSession, org, dbUser, false, &common.TenantPrivilegeScope{SiteID: &site.ID})
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Validate ProviderTenantSite relationship and state
	hasAccess, apiError := ValidateProviderOrTenantSiteAccess(ctx, logger, uemh.dbSession, site, infrastructureProvider, tenant)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}
	if !hasAccess {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Current org is not associated with the Site", nil)
	}

	// Validate the stored identity only after the caller's Site access is known.
	// Otherwise the error would reveal whether a submitted MAC matched a machine
	// the caller cannot access.
	validationErrors = validation.Errors{}
	for i, req := range apiRequests {
		mid, _ := uuid.Parse(*req.ID)
		em := requestedEmMap[mid]
		if !bmcMacUnchanged(em.BmcMacAddress, req.BmcMacAddress) {
			validationErrors[strconv.Itoa(i)] = bmcMacImmutableValidationError()
		}
	}
	if len(validationErrors) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine update data", validationErrors)
	}

	// Retrieve all ExpectedMachines on Site from DB to allow unicity checks at
	// Site level. Pure validation read, kept outside the write transaction.
	expectedMachinesOnSite, _, err := emDAO.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{
		SiteIDs: []uuid.UUID{siteID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(paginator.TotalLimit), // we want ALL records on site
	}, []string{})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving Expected Machines from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Expected Machines due to DB error", nil)
	}
	emMap := make(map[uuid.UUID]cdbm.ExpectedMachine)
	for i := range expectedMachinesOnSite {
		em := &expectedMachinesOnSite[i]
		emMap[em.ID] = *em
	}

	// Retrieve all SKUs on Site to validate existence of SKU IDs in request.
	// Pure validation read, kept outside the write transaction.
	skuDAO := cdbm.NewSkuDAO(uemh.dbSession)
	skus, _, err := skuDAO.GetAll(ctx, nil, cdbm.SkuFilterInput{
		SiteIDs: []uuid.UUID{siteID},
	}, paginator.PageInput{
		Limit: cutil.GetPtr(len(requestedSkuIDs)),
	})
	if err != nil {
		logger.Error().Err(err).Msg("error retrieving SKUs from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve SKUs due to DB error", nil)
	}
	uniqueSkuIDsOnSite := make(map[string]bool)
	for _, sku := range skus {
		uniqueSkuIDsOnSite[sku.ID] = true
	}

	// Verify Serial Number uniqueness with existing records on Site
	expectedMachineSerialNumberChecker := common.NewUniqueChecker[uuid.UUID]()

	// Load DB data into checkers
	for i := range expectedMachinesOnSite { // iterate on ALL Expected Machine on Site
		em := &expectedMachinesOnSite[i]
		if em.ChassisSerialNumber != "" {
			expectedMachineSerialNumberChecker.Update(em.ID, em.ChassisSerialNumber)
		}
	}

	// Apply Serial changes to the checker
	for _, req := range apiRequests {
		mid, _ := uuid.Parse(*req.ID)
		if req.ChassisSerialNumber != nil {
			expectedMachineSerialNumberChecker.Update(mid, *req.ChassisSerialNumber)
		}
	}

	// Final checks: Serial uniqueness and existence of SKUs
	validationErrors = validation.Errors{}
	for i, req := range apiRequests {
		itemErrors := validation.Errors{}
		strIndex := strconv.Itoa(i) // index/key as string for validation errors map
		mid, _ := uuid.Parse(*req.ID)

		// Check Serial unicity
		if req.ChassisSerialNumber != nil && expectedMachineSerialNumberChecker.DoesIDHaveConflict(mid) {
			common.AddToValidationErrors(itemErrors, "chassisSerialNumber", fmt.Errorf(
				"Expected Machine with Chassis Serial Number: %s already exists", *req.ChassisSerialNumber))
		}
		// Check SKU existence
		if req.SkuID != nil && !uniqueSkuIDsOnSite[*req.SkuID] {
			common.AddToValidationErrors(itemErrors, "skuID", fmt.Errorf(
				"the SkuID specified for Expected Machine does not exist in DB: %s", *req.SkuID))
		}

		// Collect errors
		if len(itemErrors) > 0 {
			validationErrors[strIndex] = itemErrors
		}
	}
	if len(validationErrors) > 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Expected Machine update data", validationErrors)
	}

	// Build the inputs and a credentials lookup keyed by the ExpectedMachineID
	// from each request. After UpdateMultiple returns we look credentials up
	// by the DB record's ID rather than by slice index, so correlation
	// doesn't depend on the DAO preserving input order.
	credsByID := make(map[uuid.UUID]cdbm.ExpectedMachineCredentials, len(apiRequests))
	bmcIPRequestsByID := make(map[uuid.UUID]*string, len(apiRequests))
	updateInputs := make([]cdbm.ExpectedMachineUpdateInput, 0, len(apiRequests))
	bmcIPClearIDs := make(map[uuid.UUID]struct{})
	for _, machineReq := range apiRequests {
		// APIExpectedMachineUpdateRequest must allow nil ID for single update use case. If present here, it has already been validated.
		if machineReq.ID == nil {
			logger.Error().Msg("Expected Machine ID cannot be nil")
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Expected Machine ID cannot be nil", nil)
		}

		emID, _ := uuid.Parse(*machineReq.ID)
		credsByID[emID] = cdbm.ExpectedMachineCredentials{
			Username: machineReq.DefaultBmcUsername,
			Password: machineReq.DefaultBmcPassword,
		}
		bmcIPRequestsByID[emID] = machineReq.BmcIpAddress
		bmcIPAddress := machineReq.BmcIpAddress
		if bmcIPAddress != nil && *bmcIPAddress == "" {
			bmcIPClearIDs[emID] = struct{}{}
			bmcIPAddress = nil
		}
		updateInputs = append(updateInputs, cdbm.ExpectedMachineUpdateInput{
			ExpectedMachineID:        emID,
			BmcIpAddress:             bmcIPAddress,
			ChassisSerialNumber:      machineReq.ChassisSerialNumber,
			SkuID:                    machineReq.SkuID,
			FallbackDpuSerialNumbers: machineReq.FallbackDPUSerialNumbers,
			RackID:                   machineReq.RackID,
			Name:                     machineReq.Name,
			Manufacturer:             machineReq.Manufacturer,
			Model:                    machineReq.Model,
			Description:              machineReq.Description,
			SlotID:                   machineReq.SlotID,
			TrayIdx:                  machineReq.TrayIdx,
			HostID:                   machineReq.HostID,
			Labels:                   machineReq.Labels,
			IsDpfEnabled:             machineReq.IsDpfEnabled,
			HostLifecycleProfile:     machineReq.HostLifecycleProfile.ToDBModelPtr(),
		})
	}
	// Update provided ExpectedMachines in DB
	updatedExpectedMachines, err := cdb.WithTxResult(ctx, uemh.dbSession, func(tx *cdb.Tx) ([]cdbm.ExpectedMachine, error) {
		// Lock the full target set before clearing any one row. Clear touches
		// only a subset, while UpdateMultiple writes every target; without this
		// canonical lock pass, overlapping batches can each hold a different
		// cleared row and deadlock during the bulk update.
		derr := emDAO.LockForUpdate(ctx, tx, slices.Collect(maps.Keys(idMap)))
		if derr != nil {
			if errors.Is(derr, cdb.ErrDoesNotExist) {
				return nil, cutil.NewAPIError(http.StatusConflict, "One or more Expected Machines were removed before the batch update completed", nil)
			}
			logger.Error().Err(derr).Msg("error locking ExpectedMachine records for batch update")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Machine due to DB error", nil)
		}

		// Clear first so UpdateMultiple's final SELECT sees nil. Its scoped
		// BMC IP update cannot restore cleared rows. Every target row is already
		// locked above, so the subset clear cannot introduce a second order.
		for _, input := range updateInputs {
			expectedMachineID := input.ExpectedMachineID
			if _, ok := bmcIPClearIDs[expectedMachineID]; !ok {
				continue
			}
			_, derr := emDAO.Clear(ctx, tx, cdbm.ExpectedMachineClearInput{
				ExpectedMachineID: expectedMachineID,
				BmcIpAddress:      true,
			})
			if derr != nil {
				logger.Error().Err(derr).Str("ExpectedMachineID", expectedMachineID.String()).Msg("error clearing ExpectedMachine BMC IP address in DB")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Machine due to DB error", nil)
			}
		}

		updatedMachines, derr := emDAO.UpdateMultiple(ctx, tx, updateInputs)
		if derr != nil {
			logger.Error().Err(derr).Msg("error updating ExpectedMachine records in DB")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to update Expected Machine due to DB error", nil)
		}

		workflowMachines := make([]*corev1.ExpectedMachine, 0, len(updatedMachines))
		for i := range updatedMachines {
			em := &updatedMachines[i]
			creds, ok := credsByID[em.ID]
			if !ok {
				// UpdateMultiple returned an ID we didn't ask it to create.
				// This shouldn't actually happen, so fail loudly instead of
				// attaching the wrong credentials to a machine.
				logger.Error().Str("ExpectedMachineID", em.ID.String()).Msg("UpdateMultiple returned a machine with an unrecognized ID")
				return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to correlate updated Expected Machine to request", nil)
			}
			workflowMachine := em.ToProto(creds)
			// Use the PATCH value at the workflow boundary instead of the REST
			// row, which may contain an address this request did not touch.
			workflowMachine.BmcIpAddress = bmcIPRequestsByID[em.ID]
			workflowMachines = append(workflowMachines, workflowMachine)
		}

		logger.Info().Int("Count", len(workflowMachines)).Msg("triggering Expected Machine update workflow")

		// Create workflow request
		workflowRequest := &corev1.BatchExpectedMachineOperationRequest{
			ExpectedMachines:     &corev1.ExpectedMachineList{ExpectedMachines: workflowMachines},
			AcceptPartialResults: false,
		}

		// Create workflow options. Include a UUID suffix so concurrent batches
		// of the same size on the same Site don't collide on a single ID.
		workflowID := fmt.Sprintf("expected-machines-update-batch-%s-%s", site.ID.String(), uuid.New().String())
		workflowOptions := tclient.StartWorkflowOptions{
			ID:                       workflowID,
			WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
			TaskQueue:                queue.SiteTaskQueue,
		}

		// Get the Temporal client for the site we are working with
		stc, cerr := uemh.scp.GetClientByID(site.ID)
		if cerr != nil {
			logger.Error().Err(cerr).Msg("failed to retrieve Temporal client for Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
		}

		// Execute workflow and get results
		workflowRun, werr := stc.ExecuteWorkflow(ctx, workflowOptions, "UpdateExpectedMachines", workflowRequest)
		if werr != nil {
			logger.Error().Err(werr).Msg("failed to schedule batch Expected Machine update workflow on Site")
			return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to schedule batch Expected Machine update workflow on Site: %v", werr), nil)
		}

		workflowRunID := workflowRun.GetID()
		logger = logger.With().Str("WorkflowID", workflowRunID).Logger()
		logger.Info().Msg("executing Expected Machine update workflow on Site")

		// Get workflow results
		var workflowResult corev1.BatchExpectedMachineOperationResponse

		werr = workflowRun.Get(ctx, &workflowResult)
		if werr != nil {
			logger.Error().Err(werr).Msg("error executing batch Expected Machine update workflow on Site")
			// Workflow failed entirely - don't commit transaction, changes will be rolled back
			return nil, cutil.NewAPIError(http.StatusInternalServerError, fmt.Sprintf("Failed to execute batch Expected Machine update workflow on Site: %v", werr), nil)
		}

		// sanity checks since this is all-or-nothing
		if len(workflowResult.GetResults()) != len(updatedMachines) {
			logger.Error().Msgf("workflow returned a different number of Expected Machines (expected %d but got %d)", len(updatedMachines), len(workflowResult.GetResults()))
			return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to verify batch Expected Machine update workflow results", nil)
		}

		return updatedMachines, nil
	})
	if err != nil {
		return common.HandleTxError(c, logger, err, "Failed to update Expected Machines due to DB transaction error")
	}

	logger.Info().
		Int("SuccessCount", len(updatedExpectedMachines)).
		Msg("finishing UpdateExpectedMachines API handler")

	// Return only successful machines
	return c.JSON(http.StatusOK, updatedExpectedMachines)
}
