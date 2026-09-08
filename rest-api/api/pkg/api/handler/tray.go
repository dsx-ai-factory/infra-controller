// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"slices"
	"strings"

	"github.com/labstack/echo/v4"
	"go.opentelemetry.io/otel/attribute"
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
	temporalEnums "go.temporal.io/api/enums/v1"
)

// ~~~~~ Slot resolution helpers ~~~~~ //

// resolveTrayIDsBySlot enumerates trays for the given baseSpec through Flow's
// GetComponents and returns the external IDs of components at the requested slot.
//
// baseSpec is the OperationTargetSpec the request would otherwise have
// produced (rack scope, component-pinning IDs, or "all trays
// in site"); the resolver post-filters its result by slot. An empty result
// is not an error — callers decide whether to treat it as a no-op or
// surface 404.
//
// Flow has no by-slot component target shape; REST resolves slotId to
// external component IDs and drives downstream workflows with ComponentTargets.
//
// It returns the proxy's own APIError rather than a plain error so a slot
// filter cannot downgrade the status the endpoint would otherwise report: a
// timeout here means the same thing to a client as a timeout on the call the
// slot resolution precedes.
func resolveTrayIDsBySlot(
	ctx context.Context,
	stc tClient.Client,
	baseSpec *flowv1.OperationTargetSpec,
	slot model.RackComponentSlotMatcher,
) ([]string, *cutil.APIError) {
	flowReq := &flowv1.GetComponentsRequest{TargetSpec: baseSpec}

	workflowID := fmt.Sprintf("tray-resolve-by-slot-%s",
		common.RequestHash(flowReq))
	var resp flowv1.GetComponentsResponse
	apiErr := common.ExecuteFlowGRPC(
		ctx, stc,
		flowv1.Flow_GetComponents_FullMethodName,
		flowReq, &resp,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
		"",
	)
	if apiErr != nil {
		return nil, apiErr
	}

	ids := make([]string, 0, len(resp.GetComponents()))
	for _, comp := range resp.GetComponents() {
		if !slot.Matches(comp) {
			continue
		}
		if id := comp.GetComponentId(); id != "" {
			ids = append(ids, id)
		}
	}
	return ids, nil
}

// componentTargetSpecFromIDs builds an OperationTargetSpec that targets
// the given external component IDs. Returns nil for an empty slice, which Flow
// rejects; callers should short-circuit before calling.
func componentTargetSpecFromIDs(ids []string, componentType *string) *flowv1.OperationTargetSpec {
	if len(ids) == 0 {
		return nil
	}
	protoType := flowv1.ComponentType_COMPONENT_TYPE_UNKNOWN
	if componentType != nil {
		protoType = flowv1.ComponentType(flowv1.ComponentType_value[model.APIToProtoComponentTypeName[*componentType]])
	}
	targets := make([]*flowv1.ComponentTarget, 0, len(ids))
	for _, id := range ids {
		targets = append(targets, &flowv1.ComponentTarget{
			Identifier: &flowv1.ComponentTarget_External{
				External: &flowv1.ExternalRef{Type: protoType, Id: id},
			},
		})
	}
	return &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Components{
			Components: &flowv1.ComponentTargets{Targets: targets},
		},
	}
}

// ~~~~~ Get Tray Handler ~~~~~ //

// GetTrayHandler is the API Handler for getting a Tray by ID
type GetTrayHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetTrayHandler initializes and returns a new handler for getting a Tray
func NewGetTrayHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetTrayHandler {
	return GetTrayHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a Tray
// @Description Get a Tray by ID
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tray"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APITray
// @Router /v2/org/{org}/nico/tray/{id} [get]
func (gth GetTrayHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "Get", c, gth.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to access Tray data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, gth.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate siteId is provided
	siteStrID := c.QueryParam("siteId")
	if siteStrID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, siteStrID, gth.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
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

	// Get tray ID from URL param
	trayStrID := c.Param("id")
	gth.tracerSpan.SetAttribute(handlerSpan, attribute.String("tray_id", trayStrID), logger)

	// Get the temporal client for the site
	stc, err := gth.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build Flow request
	flowRequest := &flowv1.GetComponentInfoByIDRequest{
		Id: &flowv1.UUID{Id: trayStrID},
	}

	// Execute workflow
	//
	// Concurrent identical reads do not coalesce here, unlike the tray reads
	// below. Whether they should is a question about this endpoint rather than
	// about its transport, so the policy crosses to the proxy unchanged.
	var flowResponse flowv1.GetComponentInfoResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetComponentInfoByID_FullMethodName,
		flowRequest, &flowResponse,
		fmt.Sprintf("tray-get-%s", trayStrID), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Convert to API model
	apiTray := model.NewAPITray(flowResponse.GetComponent())
	if apiTray == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Tray not found", nil)
	}

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiTray)
}

// ~~~~~ GetAll Trays Handler ~~~~~ //

// GetAllTrayHandler is the API Handler for getting all Trays
type GetAllTrayHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllTrayHandler initializes and returns a new handler for getting all Trays
func NewGetAllTrayHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllTrayHandler {
	return GetAllTrayHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all Trays
// @Description Get all Trays with optional filters
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param rackId query string false "Filter by Rack ID"
// @Param rackName query string false "Filter by Rack name"
// @Param type query string false "Filter by tray type (Compute, NVSwitch, PowerShelf)"
// @Param id query string false "Filter by component ID (use repeated params for multiple values)"
// @Param slotId query int false "Filter by rack slot ID (position.slotId). Requires rackId or rackName. Composes with other filters via AND."
// @Param orderBy query string false "Order by field (e.g. name_ASC, manufacturer_DESC)"
// @Param pageNumber query int false "Page number (1-based)"
// @Param pageSize query int false "Page size"
// @Success 200 {array} model.APITray
// @Router /v2/org/{org}/nico/tray [get]
func (gath GetAllTrayHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "GetAll", c, gath.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to access Tray data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, gath.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Bind and validate tray request from query params
	var apiRequest model.APITrayGetAllRequest
	pageRequest := pagination.PageRequest{}
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pageRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if apiRequest.SiteID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("invalid tray request parameters")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate request data", verr)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, gath.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
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

	// Validate pagination request (orderBy, pageNumber, pageSize)
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	err = pageRequest.Validate(slices.Collect(maps.Keys(model.TrayOrderByFieldMap)))
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	// Get the temporal client for the site
	stc, err := gath.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build Flow request from validated API request
	flowRequest := apiRequest.ToProto()

	// Set order and pagination on Flow request
	var orderBy *flowv1.OrderBy
	if pageRequest.OrderBy != nil {
		orderBy = model.GetProtoTrayOrderByFromQueryParam(pageRequest.OrderBy.Field, strings.ToUpper(pageRequest.OrderBy.Order))
	}
	flowRequest.OrderBy = orderBy
	// Slot filtering (slotId) is applied REST-side because Flow has no
	// slot filter. Push-down pagination would prune before post-filtering
	// and produce a wrong total, so disable it here when slotId is set.
	if pageRequest.Offset != nil && pageRequest.Limit != nil && !apiRequest.HasSlotFilter() {
		flowRequest.Pagination = &flowv1.Pagination{
			Offset: int32(*pageRequest.Offset),
			Limit:  int32(*pageRequest.Limit),
		}
	}

	hashValues := apiRequest.QueryValues()
	for _, key := range []string{"pageNumber", "pageSize", "orderBy"} {
		if v := c.QueryParam(key); v != "" {
			hashValues.Set(key, v)
		}
	}
	workflowID := fmt.Sprintf("tray-get-all-%s", common.QueryParamHash(hashValues))

	// Execute workflow
	//
	// As in GetTrayHandler, concurrent identical reads do not coalesce, and
	// changing that is a decision about the endpoint rather than part of moving
	// it onto the proxy.
	var flowResponse flowv1.GetComponentsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetComponents_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return proxyErr
	}

	components := flowResponse.GetComponents()
	total := int(flowResponse.GetTotal())

	// Apply REST-side slotId filter and re-paginate the filtered slice.
	if apiRequest.HasSlotFilter() {
		filtered := make([]*flowv1.Component, 0, len(components))
		for _, comp := range components {
			if apiRequest.MatchesSlot(comp) {
				filtered = append(filtered, comp)
			}
		}
		components = filtered
		total = len(components)

		if pageRequest.Offset != nil && pageRequest.Limit != nil {
			start := *pageRequest.Offset
			if start > total {
				start = total
			}
			end := start + *pageRequest.Limit
			if end > total {
				end = total
			}
			components = components[start:end]
		}
	}

	apiTrays := make([]*model.APITray, 0, len(components))
	for _, comp := range components {
		apiTray := model.NewAPITray(comp)
		if apiTray != nil {
			apiTrays = append(apiTrays, apiTray)
		}
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("count", len(apiTrays)).Int("Total", total).Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiTrays)
}

// ~~~~~ Validate Tray Handler ~~~~~ //

// ValidateTrayHandler is the API Handler for validating a single Tray's components
type ValidateTrayHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewValidateTrayHandler initializes and returns a new handler for validating a Tray
func NewValidateTrayHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) ValidateTrayHandler {
	return ValidateTrayHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Validate a Tray
// @Description Validate a Tray by comparing expected vs actual state via Flow
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of the Tray"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APIRackValidationResult
// @Router /v2/org/{org}/nico/tray/{id}/validation [get]
func (vth ValidateTrayHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "Validate", c, vth.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to access Tray data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, vth.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get tray ID from URL param
	trayStrID := c.Param("id")
	vth.tracerSpan.SetAttribute(handlerSpan, attribute.String("tray_id", trayStrID), logger)

	// Get site ID from query param (required)
	siteStrID := c.QueryParam("siteId")
	if siteStrID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "siteId query parameter is required", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, siteStrID, vth.dbSession)
	if err != nil {
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
	stc, err := vth.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build Flow request - target the specific tray by ID
	flowRequest := &flowv1.ValidateComponentsRequest{
		TargetSpec: &flowv1.OperationTargetSpec{
			Targets: &flowv1.OperationTargetSpec_Components{
				Components: &flowv1.ComponentTargets{
					Targets: []*flowv1.ComponentTarget{
						{
							Identifier: &flowv1.ComponentTarget_External{External: &flowv1.ExternalRef{Id: trayStrID}},
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
		fmt.Sprintf("tray-validate-%s", trayStrID), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Convert to API model
	apiResult := model.NewAPIRackValidationResult(&flowResponse)

	logger.Info().Int32("TotalDiffs", flowResponse.GetTotalDiffs()).Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiResult)
}

// ~~~~~ Validate Trays Handler ~~~~~ //

// ValidateTraysHandler is the API Handler for validating Trays with optional filters.
// If no filter is specified, validates all trays in the Site.
type ValidateTraysHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewValidateTraysHandler initializes and returns a new handler for validating Trays
func NewValidateTraysHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) ValidateTraysHandler {
	return ValidateTraysHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Validate Trays
// @Description Validate Tray components by comparing expected vs actual state via Flow. If no filter is specified, validates all trays in the Site. Use rackId/rackName to scope to a specific rack, and name/manufacturer/type to filter by tray attributes.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param rackId query string false "Scope to a specific Rack by ID (mutually exclusive with rackName)"
// @Param rackName query string false "Scope to a specific Rack by name (mutually exclusive with rackId)"
// @Param name query string false "Filter trays by name"
// @Param manufacturer query string false "Filter trays by manufacturer"
// @Param type query string false "Filter trays by type (Compute, NVSwitch, PowerShelf)"
// @Param id query string false "Filter by component ID (mutually exclusive with rackId/rackName; use repeated params for multiple values)"
// @Param slotId query int false "Validate only trays at this rack slot (position.slotId). Requires rackId or rackName. Composes via AND."
// @Success 200 {object} model.APIRackValidationResult
// @Router /v2/org/{org}/nico/tray/validation [get]
func (vtsh ValidateTraysHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "ValidateTrays", c, vtsh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APITrayValidateAllRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("invalid tray validate request parameters")
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

	// Validate role, only Provider Admins are allowed to access Tray data
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, vtsh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, vtsh.dbSession)
	if err != nil {
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
	stc, err := vtsh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Resolve slotId (when set) to external component IDs before building
	// the flow request: Flow has no by-slot component target shape.
	targetSpec := apiRequest.ToTargetSpec()
	if apiRequest.HasSlotFilter() {
		ids, resolveErr := resolveTrayIDsBySlot(ctx, stc, targetSpec,
			model.RackComponentSlotMatcher{SlotID: apiRequest.SlotID})
		if resolveErr != nil {
			logger.Error().Err(resolveErr.Diagnosis()).Msg("failed to resolve trays by slot")
			return cutil.NewAPIErrorResponse(c, resolveErr.Code, resolveErr.Message, nil)
		}
		if len(ids) == 0 {
			logger.Info().Msg("no trays match slot filter; returning empty validation result")
			return c.JSON(http.StatusOK, model.NewAPIRackValidationResult(&flowv1.ValidateComponentsResponse{}))
		}
		targetSpec = componentTargetSpecFromIDs(ids, apiRequest.Type)
	}

	flowRequest := &flowv1.ValidateComponentsRequest{
		TargetSpec: targetSpec,
		Filters:    apiRequest.ToFilters(),
	}

	workflowID := fmt.Sprintf("tray-validate-all-%s", common.QueryParamHash(apiRequest.QueryValues()))

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

// ~~~~~ Update Tray Power State Handler ~~~~~ //

// UpdateTrayPowerStateHandler is the API Handler for power controlling a single Tray by ID
type UpdateTrayPowerStateHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateTrayPowerStateHandler initializes and returns a new handler for power controlling a Tray
func NewUpdateTrayPowerStateHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateTrayPowerStateHandler {
	return UpdateTrayPowerStateHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Power control a Tray
// @Description Power control a Tray identified by component ID (on, off, cycle, forceoff, forcecycle)
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of Tray"
// @Param body body model.APIUpdatePowerStateRequest true "Power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/tray/{id}/power [patch]
func (pcth UpdateTrayPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "PowerControl", c, pcth.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to power control Tray
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, pcth.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get tray ID from URL param
	trayStrID := c.Param("id")
	pcth.tracerSpan.SetAttribute(handlerSpan, attribute.String("tray_id", trayStrID), logger)

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

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, pcth.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := pcth.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec for single tray by ID
	targetSpec := &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Components{
			Components: &flowv1.ComponentTargets{
				Targets: []*flowv1.ComponentTarget{
					{
						Identifier: &flowv1.ComponentTarget_External{External: &flowv1.ExternalRef{Id: trayStrID}},
					},
				},
			},
		},
	}

	flowResp, err := common.ExecutePowerControlWorkflow(ctx, c, logger, stc, targetSpec, apiRequest.State,
		apiRequest.RuleID, apiRequest.OverrideReadinessCheck, fmt.Sprintf("tray-power-state-update-%s-%s", apiRequest.State, trayStrID), "Tray")
	if err != nil {
		return err
	}

	logger.Info().Str("State", apiRequest.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// ~~~~~ Batch Update Tray Power State Handler ~~~~~ //

// BatchUpdateTrayPowerStateHandler is the API Handler for power controlling Trays with optional filters
type BatchUpdateTrayPowerStateHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewBatchUpdateTrayPowerStateHandler initializes and returns a new handler for batch power controlling Trays
func NewBatchUpdateTrayPowerStateHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) BatchUpdateTrayPowerStateHandler {
	return BatchUpdateTrayPowerStateHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Power control Trays
// @Description Power control Trays with optional filters (on, off, cycle, forceoff, forcecycle). If no filter is specified, targets all trays in the Site.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchUpdateTrayPowerStateRequest true "Batch tray power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/tray/power [patch]
func (pctbh BatchUpdateTrayPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "PowerControlBatch", c, pctbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Bind and validate the JSON body
	var request model.APIBatchUpdateTrayPowerStateRequest
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := request.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch tray power control request")
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

	// Validate role, only Provider Admins are allowed to power control Tray
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, pctbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, request.SiteID, pctbh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := pctbh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec from filter (nil filter = all trays). When the
	// filter pins a rack slot, Flow has no by-slot target shape, so we
	// resolve to external component IDs first.
	targetSpec := request.Filter.ToTargetSpec()
	if request.Filter.HasSlotFilter() {
		ids, resolveErr := resolveTrayIDsBySlot(ctx, stc, targetSpec,
			model.RackComponentSlotMatcher{SlotID: request.Filter.SlotID})
		if resolveErr != nil {
			logger.Error().Err(resolveErr.Diagnosis()).Msg("failed to resolve trays by slot")
			return cutil.NewAPIErrorResponse(c, resolveErr.Code, resolveErr.Message, nil)
		}
		if len(ids) == 0 {
			logger.Info().Msg("no trays match slot filter; returning empty task list")
			return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(nil))
		}
		targetSpec = componentTargetSpecFromIDs(ids, request.Filter.Type)
	}

	flowResp, err := common.ExecutePowerControlWorkflow(ctx, c, logger, stc, targetSpec, request.State,
		request.RuleID, request.OverrideReadinessCheck, fmt.Sprintf("tray-power-state-batch-update-%s-%s", request.State, common.RequestHash(request.Filter)), "Tray")
	if err != nil {
		return err
	}

	logger.Info().Str("State", request.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// ~~~~~ Update Tray Firmware Handler ~~~~~ //

// UpdateTrayFirmwareHandler is the API Handler for upgrading firmware on a single Tray by ID
type UpdateTrayFirmwareHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateTrayFirmwareHandler initializes and returns a new handler for firmware upgrading a Tray
func NewUpdateTrayFirmwareHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateTrayFirmwareHandler {
	return UpdateTrayFirmwareHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Firmware update a Tray
// @Description Update firmware on a Tray identified by component ID.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Component ID"
// @Param body body model.APIUpdateFirmwareRequest true "Firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/tray/{id}/firmware [patch]
func (futh UpdateTrayFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "FirmwareUpdate", c, futh.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to firmware update Tray
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, futh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get tray ID from URL param
	trayStrID := c.Param("id")
	futh.tracerSpan.SetAttribute(handlerSpan, attribute.String("tray_id", trayStrID), logger)

	// Parse and validate request body
	apiRequest := model.APIUpdateFirmwareRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating firmware update request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate firmware update request data", verr)
	}

	// Retrieve the Site from the DB
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, futh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := futh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	targetSpec := &flowv1.OperationTargetSpec{
		Targets: &flowv1.OperationTargetSpec_Components{
			Components: &flowv1.ComponentTargets{
				Targets: []*flowv1.ComponentTarget{
					{
						Identifier: &flowv1.ComponentTarget_External{External: &flowv1.ExternalRef{Id: trayStrID}},
					},
				},
			},
		},
	}

	flowResp, err := common.ExecuteFirmwareUpdateWorkflow(ctx, c, logger, stc, targetSpec, apiRequest.Version,
		apiRequest.Targets, apiRequest.AuthenticationData.ToProto(), apiRequest.SiteID,
		apiRequest.RuleID, apiRequest.OverrideReadinessCheck, fmt.Sprintf("tray-firmware-update-%s", trayStrID), "Tray")
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}

// ~~~~~ Batch Update Tray Firmware Handler ~~~~~ //

// BatchUpdateTrayFirmwareHandler is the API Handler for firmware upgrading Trays with optional filters
type BatchUpdateTrayFirmwareHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewBatchUpdateTrayFirmwareHandler initializes and returns a new handler for batch firmware upgrading Trays
func NewBatchUpdateTrayFirmwareHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) BatchUpdateTrayFirmwareHandler {
	return BatchUpdateTrayFirmwareHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Firmware update Trays
// @Description Update firmware on Trays with optional filters. If no filter is specified, targets all trays in the Site.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchTrayFirmwareUpdateRequest true "Batch tray firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/tray/firmware [patch]
func (futbh BatchUpdateTrayFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Tray", "FirmwareUpdateBatch", c, futbh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	// Bind and validate the JSON body
	var request model.APIBatchTrayFirmwareUpdateRequest
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := request.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating batch tray firmware update request")
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

	// Validate role, only Provider Admins are allowed to firmware update Tray
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, futbh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, request.SiteID, futbh.dbSession)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		}
		logger.Error().Err(err).Msg("error retrieving Site from DB")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site due to DB error", nil)
	}

	// Verify site belongs to the org's Infrastructure Provider
	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site
	stc, err := futbh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// Build TargetSpec from filter (nil filter = all trays). When the
	// filter pins a rack slot, resolve to external component IDs first; Flow has
	// no by-slot target shape.
	targetSpec := request.Filter.ToTargetSpec()
	if request.Filter.HasSlotFilter() {
		ids, resolveErr := resolveTrayIDsBySlot(ctx, stc, targetSpec,
			model.RackComponentSlotMatcher{SlotID: request.Filter.SlotID})
		if resolveErr != nil {
			logger.Error().Err(resolveErr.Diagnosis()).Msg("failed to resolve trays by slot")
			return cutil.NewAPIErrorResponse(c, resolveErr.Code, resolveErr.Message, nil)
		}
		if len(ids) == 0 {
			logger.Info().Msg("no trays match slot filter; returning empty task list")
			return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(nil))
		}
		targetSpec = componentTargetSpecFromIDs(ids, request.Filter.Type)
	}

	flowResp, err := common.ExecuteFirmwareUpdateWorkflow(ctx, c, logger, stc, targetSpec, request.Version,
		request.Targets, request.AuthenticationData.ToProto(), request.SiteID, request.RuleID,
		request.OverrideReadinessCheck, fmt.Sprintf("tray-firmware-batch-update-%s", common.RequestHash(request.Filter)), "Tray")
	if err != nil {
		return err
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}
