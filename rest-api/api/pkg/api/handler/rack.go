// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"slices"
	"strings"

	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
	temporalEnums "go.temporal.io/api/enums/v1"
	tClient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// ~~~~~ Get Rack Handler ~~~~~ //

// GetRackHandler is the API Handler for getting a Rack by ID
type GetRackHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetRackHandler initializes and returns a new handler for getting a Rack
func NewGetRackHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetRackHandler {
	return GetRackHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a Rack
// @Description Get a Rack by ID from Flow
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Rack"
// @Param siteId query string true "ID of the Site"
// @Param includeComponents query boolean false "Include rack components in response"
// @Success 200 {object} model.APIRack
// @Router /v2/org/{org}/nico/rack/{id} [get]
func (grh GetRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "Get", c, grh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APIRackGetRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to access Rack data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, grh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get rack ID from URL param
	rackStrID := c.Param("id")
	grh.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_id", rackStrID), logger)

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, grh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	// Get the temporal client for the site
	stc, err := grh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build Flow request
	flowRequest := &flowv1.GetRackInfoByIDRequest{
		Id:             &flowv1.UUID{Id: rackStrID},
		WithComponents: apiRequest.IncludeComponents,
	}

	// Execute workflow
	var flowResponse flowv1.GetRackInfoResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetRackInfoByID_FullMethodName,
		flowRequest, &flowResponse,
		fmt.Sprintf("rack-get-%s", rackStrID), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Convert to API model
	protoRack := flowResponse.GetRack()
	apiRack := model.NewAPIRack(protoRack, apiRequest.IncludeComponents)
	if apiRack == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Rack not found", nil)
	}

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiRack)
}

// ~~~~~ GetAll Racks Handler ~~~~~ //

// GetAllRackHandler is the API Handler for getting all Racks
type GetAllRackHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllRackHandler initializes and returns a new handler for getting all Racks
func NewGetAllRackHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllRackHandler {
	return GetAllRackHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all Racks
// @Description Get all Racks from Flow with optional filters
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param includeComponents query boolean false "Include rack components in response"
// @Param name query string false "Filter by rack name"
// @Param manufacturer query string false "Filter by manufacturer"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} model.APIRack
// @Router /v2/org/{org}/nico/rack [get]
func (garh GetAllRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "GetAll", c, garh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APIRackGetAllRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to access Rack data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, garh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, garh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	// Validate pagination request
	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}

	// Validate pagination attributes
	err = pageRequest.Validate(slices.Collect(maps.Keys(model.RackOrderByFieldMap)))
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Build OrderBy from pagination
	var orderBy *flowv1.OrderBy
	if pageRequest.OrderBy != nil {
		orderBy = model.GetProtoRackOrderByFromQueryParam(pageRequest.OrderBy.Field, strings.ToUpper(pageRequest.OrderBy.Order))
	}

	// Build Pagination
	var paginationProto *flowv1.Pagination
	if pageRequest.Offset != nil && pageRequest.Limit != nil {
		paginationProto = &flowv1.Pagination{
			Offset: int32(*pageRequest.Offset),
			Limit:  int32(*pageRequest.Limit),
		}
	}

	// Get the temporal client for the site
	stc, err := garh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build Flow request from validated params
	flowRequest := &flowv1.GetListOfRacksRequest{
		Filters:        apiRequest.ToFilters(),
		WithComponents: apiRequest.IncludeComponents,
		Pagination:     paginationProto,
		OrderBy:        orderBy,
	}

	workflowID := fmt.Sprintf("rack-get-all-%s", common.QueryParamHash(apiRequest.QueryValues()))

	// Execute workflow
	var flowResponse flowv1.GetListOfRacksResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetListOfRacks_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Convert to API model
	apiRacks := make([]*model.APIRack, 0, len(flowResponse.GetRacks()))
	for _, rack := range flowResponse.GetRacks() {
		apiRacks = append(apiRacks, model.NewAPIRack(rack, apiRequest.IncludeComponents))
	}

	// Create pagination response header
	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiRacks)).Int("Total", total).Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiRacks)
}

// ~~~~~ Validate Rack Handler ~~~~~ //

// ValidateRackHandler is the API Handler for validating a Rack's components
type ValidateRackHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewValidateRackHandler initializes and returns a new handler for validating a Rack
func NewValidateRackHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) ValidateRackHandler {
	return ValidateRackHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Validate a Rack
// @Description Validate a Rack's components by comparing expected vs actual state via Flow
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of the Rack"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APIRackValidationResult
// @Router /v2/org/{org}/nico/rack/{id}/validation [get]
func (vrh ValidateRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "Validate", c, vrh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to access Rack data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, vrh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get rack ID from URL param
	rackStrID := c.Param("id")
	vrh.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_id", rackStrID), logger)

	// Get site ID from query param (required)
	siteStrID := c.QueryParam("siteId")
	if siteStrID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, siteStrID, vrh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	// Get the temporal client for the site
	stc, err := vrh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build Flow request - target the specific rack by ID
	flowRequest := &flowv1.ValidateComponentsRequest{
		TargetSpec: &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Racks{
				Racks: &flowv1.RackTargets{
					Targets: []*flowv1.RackTarget{
						{
							Identifier: &flowv1.RackTarget_ExternalId{ExternalId: rackStrID},
						},
					},
				},
			},
		},
	}

	// Execute workflow
	var flowResponse flowv1.ValidateComponentsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ValidateComponents_FullMethodName,
		flowRequest, &flowResponse,
		fmt.Sprintf("rack-validate-%s", rackStrID), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Convert to API model
	apiResult := model.NewAPIRackValidationResult(&flowResponse)

	logger.Info().Int32("TotalDiffs", flowResponse.GetTotalDiffs()).Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiResult)
}

// ~~~~~ Validate Racks Handler ~~~~~ //

// ValidateRacksHandler is the API Handler for validating Racks with optional filters.
// If no filter is specified, validates all racks in the Site.
type ValidateRacksHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewValidateRacksHandler initializes and returns a new handler for validating Racks
func NewValidateRacksHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) ValidateRacksHandler {
	return ValidateRacksHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Validate Racks
// @Description Validate Rack components by comparing expected vs actual state via Flow. If no filter is specified, validates all racks in the Site.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param name query string false "Filter racks by name"
// @Param manufacturer query string false "Filter racks by manufacturer"
// @Success 200 {object} model.APIRackValidationResult
// @Router /v2/org/{org}/nico/rack/validation [get]
func (vrsh ValidateRacksHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "ValidateRacks", c, vrsh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APIRackValidateAllRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to access Rack data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, vrsh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, vrsh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}

	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	// Get the temporal client for the site
	stc, err := vrsh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.ValidateComponentsRequest{
		Filters: apiRequest.ToFilters(),
	}

	workflowID := fmt.Sprintf("rack-validate-all-%s", common.QueryParamHash(apiRequest.QueryValues()))

	// Execute workflow
	var flowResponse flowv1.ValidateComponentsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ValidateComponents_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Convert to API model
	apiResult := model.NewAPIRackValidationResult(&flowResponse)

	logger.Info().Int32("TotalDiffs", flowResponse.GetTotalDiffs()).Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiResult)
}

// ~~~~~ Update Rack Power State Handler ~~~~~ //

// UpdateRackPowerStateHandler is the API Handler for power controlling a single Rack by ID
type UpdateRackPowerStateHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateRackPowerStateHandler initializes and returns a new handler for power controlling a Rack
func NewUpdateRackPowerStateHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateRackPowerStateHandler {
	return UpdateRackPowerStateHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Power control a Rack
// @Description Power control a Rack identified by Rack ID (on, off, cycle, forceoff, forcecycle)
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Rack"
// @Param body body model.APIUpdatePowerStateRequest true "Power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/rack/{id}/power [patch]
func (pcrh UpdateRackPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "PowerControl", c, pcrh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to power control Rack
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, pcrh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get rack ID from URL param
	rackStrID := c.Param("id")
	pcrh.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_id", rackStrID), logger)

	// Parse and validate request body
	apiRequest := model.APIUpdatePowerStateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating power control request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate power control request data", verr)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, pcrh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := pcrh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec for single rack by ID
	targetSpec := &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Racks{
			Racks: &flowv1.RackTargets{
				Targets: []*flowv1.RackTarget{
					{
						Identifier: &flowv1.RackTarget_ExternalId{ExternalId: rackStrID},
					},
				},
			},
		},
	}

	flowResp, err := common.ExecutePowerControlWorkflow(ctx, c, logger, stc, targetSpec, apiRequest.State,
		apiRequest.RuleID, apiRequest.OverrideReadinessCheck, fmt.Sprintf("rack-power-state-update-%s-%s", apiRequest.State, rackStrID), "Rack")
	if err != nil {
		return err
	}

	logger.Info().Str("State", apiRequest.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// ~~~~~ Batch Update Rack Power State Handler ~~~~~ //

// BatchUpdateRackPowerStateHandler is the API Handler for power controlling Racks with optional filters
type BatchUpdateRackPowerStateHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewBatchUpdateRackPowerStateHandler initializes and returns a new handler for batch power controlling Racks
func NewBatchUpdateRackPowerStateHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) BatchUpdateRackPowerStateHandler {
	return BatchUpdateRackPowerStateHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Power control Racks
// @Description Power control Racks with optional filters (on, off, cycle, forceoff, forcecycle). If no filter is specified, targets all racks in the Site.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchUpdateRackPowerStateRequest true "Batch rack power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/rack/power [patch]
func (pcrbh BatchUpdateRackPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "PowerControlBatch", c, pcrbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Bind and validate the JSON body
	var request model.APIBatchUpdateRackPowerStateRequest
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := request.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch rack power control request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to power control Rack
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, pcrbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, request.SiteID, pcrbh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := pcrbh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec from filter (nil filter = all racks)
	targetSpec := request.Filter.ToTargetSpec()

	flowResp, err := common.ExecutePowerControlWorkflow(ctx, c, logger, stc, targetSpec, request.State,
		request.RuleID, request.OverrideReadinessCheck, fmt.Sprintf("rack-power-state-batch-update-%s-%s", request.State, common.RequestHash(request.Filter)), "Rack")
	if err != nil {
		return err
	}

	logger.Info().Str("State", request.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// ~~~~~ Update Rack Firmware Handler ~~~~~ //

// UpdateRackFirmwareHandler is the API Handler for upgrading firmware on a single Rack by ID
type UpdateRackFirmwareHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateRackFirmwareHandler initializes and returns a new handler for firmware upgrading a Rack
func NewUpdateRackFirmwareHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateRackFirmwareHandler {
	return UpdateRackFirmwareHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Firmware update a Rack
// @Description Update firmware on a Rack identified by Rack ID.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Rack ID"
// @Param body body model.APIUpdateFirmwareRequest true "Firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/rack/{id}/firmware [patch]
func (furh UpdateRackFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "FirmwareUpdate", c, furh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to firmware update Rack
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, furh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get rack ID from URL param
	rackStrID := c.Param("id")
	furh.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_id", rackStrID), logger)

	// Parse and validate request body
	apiRequest := model.APIUpdateFirmwareRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating firmware update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate firmware update request data", verr)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, furh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := furh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	targetSpec := &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Racks{
			Racks: &flowv1.RackTargets{
				Targets: []*flowv1.RackTarget{
					{
						Identifier: &flowv1.RackTarget_ExternalId{ExternalId: rackStrID},
					},
				},
			},
		},
	}

	flowResp, err := common.ExecuteFirmwareUpdateWorkflow(ctx, c, logger, stc, targetSpec, apiRequest.Version,
		nil, apiRequest.AuthenticationData.ToProto(), apiRequest.SiteID, apiRequest.RuleID,
		apiRequest.OverrideReadinessCheck, fmt.Sprintf("rack-firmware-update-%s", rackStrID), "Rack")
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}

// ~~~~~ Batch Update Rack Firmware Handler ~~~~~ //

// BatchUpdateRackFirmwareHandler is the API Handler for firmware upgrading Racks with optional filters
type BatchUpdateRackFirmwareHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewBatchUpdateRackFirmwareHandler initializes and returns a new handler for batch firmware upgrading Racks
func NewBatchUpdateRackFirmwareHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) BatchUpdateRackFirmwareHandler {
	return BatchUpdateRackFirmwareHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Firmware update Racks
// @Description Update firmware on Racks with optional name filter. If no filter is specified, targets all racks in the Site.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchRackFirmwareUpdateRequest true "Batch rack firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/rack/firmware [patch]
func (furbh BatchUpdateRackFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "FirmwareUpdateBatch", c, furbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Bind and validate the JSON body
	var request model.APIBatchRackFirmwareUpdateRequest
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := request.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch rack firmware update request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to firmware update Rack
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, furbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, request.SiteID, furbh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := furbh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec from filter (nil filter = all racks)
	targetSpec := request.Filter.ToTargetSpec()

	flowResp, err := common.ExecuteFirmwareUpdateWorkflow(ctx, c, logger, stc, targetSpec, request.Version,
		nil, request.AuthenticationData.ToProto(), request.SiteID, request.RuleID,
		request.OverrideReadinessCheck, fmt.Sprintf("rack-firmware-batch-update-%s", common.RequestHash(request.Filter)), "Rack")
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}

// ~~~~~ Bring Up Rack Handler ~~~~~ //

// BringUpRackHandler is the API Handler for bringing up a single Rack by ID
type BringUpRackHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewBringUpRackHandler initializes and returns a new handler for bringing up a Rack
func NewBringUpRackHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) BringUpRackHandler {
	return BringUpRackHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Bring up a Rack
// @Description Bring up a Rack identified by Rack ID
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Rack ID"
// @Param body body model.APIBringUpRackRequest true "Bring up request"
// @Success 200 {object} model.APIBringUpRackResponse
// @Router /v2/org/{org}/nico/rack/{id}/bringup [post]
func (burh BringUpRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "BringUp", c, burh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to bring up Rack
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, burh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get rack ID from URL param
	rackStrID := c.Param("id")
	burh.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_id", rackStrID), logger)

	// Parse and validate request body
	apiRequest := model.APIBringUpRackRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating bring up request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate bring up request data", verr)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, burh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := burh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec for single rack by ID
	targetSpec := &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Racks{
			Racks: &flowv1.RackTargets{
				Targets: []*flowv1.RackTarget{
					{
						Identifier: &flowv1.RackTarget_ExternalId{ExternalId: rackStrID},
					},
				},
			},
		},
	}

	description := apiRequest.Description
	if description == "" {
		description = fmt.Sprintf("API bring up Rack %s", rackStrID)
	}

	flowResp, err := common.ExecuteBringUpRackWorkflow(ctx, c, logger, stc, targetSpec, description,
		apiRequest.RuleID, apiRequest.OverrideReadinessCheck, fmt.Sprintf("rack-bringup-%s", rackStrID), "Rack")
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIBringUpRackResponse(flowResp))
}

// ~~~~~ Batch Bring Up Rack Handler ~~~~~ //

// BatchBringUpRackHandler is the API Handler for bringing up Racks with optional filters
type BatchBringUpRackHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewBatchBringUpRackHandler initializes and returns a new handler for batch bringing up Racks
func NewBatchBringUpRackHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) BatchBringUpRackHandler {
	return BatchBringUpRackHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Bring up Racks
// @Description Bring up Racks with optional filters. If no filter is specified, targets all racks in the Site.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchBringUpRackRequest true "Batch rack bring up request"
// @Success 200 {object} model.APIBringUpRackResponse
// @Router /v2/org/{org}/nico/rack/bringup [post]
func (bbuh BatchBringUpRackHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Rack", "BringUpBatch", c, bbuh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Bind and validate the JSON body
	var request model.APIBatchBringUpRackRequest
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := request.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch rack bring up request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// Is DB user missing?
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	// Validate org membership
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to bring up Rack
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, bbuh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, request.SiteID, bbuh.dbSession)
	if err != nil {
		if errors.Is(err, common.ErrInvalidID) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		}
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := bbuh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec from filter (nil filter = all racks)
	targetSpec := request.Filter.ToTargetSpec()

	description := request.Description
	if description == "" {
		description = "API batch bring up Racks"
	}

	flowResp, err := common.ExecuteBringUpRackWorkflow(ctx, c, logger, stc, targetSpec, description,
		request.RuleID, request.OverrideReadinessCheck, fmt.Sprintf("rack-bringup-batch-%s", common.RequestHash(request.Filter)), "Rack")
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIBringUpRackResponse(flowResp))
}
