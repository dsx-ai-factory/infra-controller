// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"slices"

	"github.com/google/uuid"
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

// GetNVLinkDomainHandler is the API Handler for getting an NVLink Domain by ID
type GetNVLinkDomainHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetNVLinkDomainHandler initializes and returns a new handler for getting an NVLink Domain
func NewGetNVLinkDomainHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetNVLinkDomainHandler {
	return GetNVLinkDomainHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get an NVLink Domain
// @Description Get an NVLink Domain by ID
// @Tags Domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "ID of NVLink Domain"
// @Param siteId query string true "ID of the Site"
// @Param includeComponents query boolean false "Include domain components in response"
// @Success 200 {object} model.APINVLinkDomain
// @Router /v2/org/{org}/nico/domain/nvlink/{id} [get]
func (gndh GetNVLinkDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "Get", c, gndh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APINVLinkDomainGetRequest
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, gndh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	domainID := c.Param("id")
	gndh.tracerSpan.SetAttribute(handlerSpan, attribute.String("domain_id", domainID), logger)

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, gndh.dbSession)
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

	stc, err := gndh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.GetNVLinkDomainRequest{
		Id:             domainID,
		WithComponents: apiRequest.IncludeComponents,
	}

	var flowResponse flowv1.GetNVLinkDomainResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, logger, stc,
		flowv1.Flow_GetNVLinkDomain_FullMethodName,
		flowRequest, &flowResponse,
		fmt.Sprintf("nvlink-domain-get-%s-%s-%t", site.ID, common.RequestHash(domainID), apiRequest.IncludeComponents), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	apiDomain := model.NewAPINVLinkDomain(flowResponse.GetDomain(), apiRequest.IncludeComponents)
	if apiDomain == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "NVLink Domain not found", nil)
	}

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiDomain)
}

// GetAllNVLinkDomainHandler is the API Handler for getting all NVLink Domains
type GetAllNVLinkDomainHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllNVLinkDomainHandler initializes and returns a new handler for getting all NVLink Domains
func NewGetAllNVLinkDomainHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllNVLinkDomainHandler {
	return GetAllNVLinkDomainHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get all NVLink Domains
// @Description Get all NVLink Domains with optional filters
// @Tags Domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param includeComponents query boolean false "Include domain components in response"
// @Param name query string false "Filter by domain name"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Param orderBy query string false "Order by field"
// @Success 200 {array} model.APINVLinkDomain
// @Router /v2/org/{org}/nico/domain/nvlink [get]
func (gandh GetAllNVLinkDomainHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "GetAll", c, gandh.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APINVLinkDomainGetAllRequest
	err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&apiRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	err = apiRequest.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, gandh.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, gandh.dbSession)
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

	pageRequest := pagination.PageRequest{}
	err = c.Bind(&pageRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if pageRequest.OrderByStr == nil {
		pageRequest.OrderByStr = cutil.GetPtr("NAME_ASC")
	}

	err = pageRequest.Validate([]string{"name"})
	if err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	var paginationProto *flowv1.Pagination
	if pageRequest.Offset != nil && pageRequest.Limit != nil {
		paginationProto = &flowv1.Pagination{
			Offset: int32(*pageRequest.Offset),
			Limit:  int32(*pageRequest.Limit),
		}
	}

	stc, err := gandh.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.GetListOfNVLinkDomainsRequest{
		Info:           apiRequest.ToQueryInfo(),
		WithComponents: apiRequest.IncludeComponents,
		Pagination:     paginationProto,
		OrderBy:        *pageRequest.OrderByStr,
	}

	workflowID := fmt.Sprintf("nvlink-domain-get-all-%s", common.QueryParamHash(apiRequest.QueryValues()))

	var flowResponse flowv1.GetListOfNVLinkDomainsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, logger, stc,
		flowv1.Flow_GetListOfNVLinkDomains_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	apiDomains := make([]*model.APINVLinkDomain, 0, len(flowResponse.GetDomains()))
	for _, domain := range flowResponse.GetDomains() {
		apiDomains = append(apiDomains, model.NewAPINVLinkDomain(domain, apiRequest.IncludeComponents))
	}

	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiDomains)).Int("Total", total).Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiDomains)
}

type nvLinkDomainOperationWorkflowIdentity struct {
	SiteID                 string   `json:"siteId"`
	NVLinkDomainIDs        []string `json:"domainIds"`
	State                  string   `json:"state,omitempty"`
	Version                *string  `json:"version,omitempty"`
	RuleID                 *string  `json:"ruleId,omitempty"`
	OverrideReadinessCheck bool     `json:"overrideReadinessCheck,omitempty"`
}

func newNVLinkDomainOperationWorkflowIdentity(
	siteID string,
	nvLinkDomainIDs []string,
	ruleID *string,
) (nvLinkDomainOperationWorkflowIdentity, error) {
	parsedSiteID, err := uuid.Parse(siteID)
	if err != nil {
		return nvLinkDomainOperationWorkflowIdentity{}, errors.New("site ID must be a UUID")
	}

	canonicalNVLinkDomainIDs := make([]string, len(nvLinkDomainIDs))
	for i, nvLinkDomainID := range nvLinkDomainIDs {
		err := model.ValidateNVLinkDomainID(nvLinkDomainID)
		if err != nil {
			return nvLinkDomainOperationWorkflowIdentity{}, fmt.Errorf("NVLink Domain ID at index %d: %w", i, err)
		}
		canonicalNVLinkDomainIDs[i] = nvLinkDomainID
	}
	slices.Sort(canonicalNVLinkDomainIDs)

	var canonicalRuleID *string
	if ruleID != nil && *ruleID != "" {
		parsedRuleID, err := uuid.Parse(*ruleID)
		if err != nil {
			return nvLinkDomainOperationWorkflowIdentity{}, errors.New("rule ID must be a UUID")
		}
		canonical := parsedRuleID.String()
		canonicalRuleID = &canonical
	}

	return nvLinkDomainOperationWorkflowIdentity{
		SiteID:          parsedSiteID.String(),
		NVLinkDomainIDs: canonicalNVLinkDomainIDs,
		RuleID:          canonicalRuleID,
	}, nil
}

func (identity nvLinkDomainOperationWorkflowIdentity) powerWorkflowID(
	state string,
	overrideReadinessCheck bool,
) string {
	workflowToken := model.PowerControlStateWorkflowToken(state)
	identity.State = workflowToken
	identity.OverrideReadinessCheck = overrideReadinessCheck

	return fmt.Sprintf("nvlink-domain-power-state-update-%s-%s", workflowToken, common.RequestHash(identity))
}

func (identity nvLinkDomainOperationWorkflowIdentity) firmwareWorkflowID(
	version *string,
	overrideReadinessCheck bool,
) string {
	if version != nil && *version == "" {
		version = nil
	}
	identity.Version = version
	identity.OverrideReadinessCheck = overrideReadinessCheck

	return fmt.Sprintf("nvlink-domain-firmware-update-%s", common.RequestHash(identity))
}

// UpdateNVLinkDomainPowerStateHandler power controls one NVLink Domain identified by ID.
type UpdateNVLinkDomainPowerStateHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewUpdateNVLinkDomainPowerStateHandler initializes an NVLink Domain power-control handler.
func NewUpdateNVLinkDomainPowerStateHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) UpdateNVLinkDomainPowerStateHandler {
	return UpdateNVLinkDomainPowerStateHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Power control an NVLink Domain
// @Description Power control an NVLink Domain identified by ID.
// @Tags domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the NVLink Domain"
// @Param body body model.APIUpdatePowerStateRequest true "NVLink Domain power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/domain/nvlink/{id}/power [patch]
func (h UpdateNVLinkDomainPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "PowerControl", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	nvLinkDomainID := c.Param("id")
	if err := model.ValidateNVLinkDomainID(nvLinkDomainID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	request := model.APIUpdatePowerStateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating NVLink Domain power control request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate NVLink Domain power control request", err)
	}

	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, []string{nvLinkDomainID}, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("nvlink_domain_id", identity.NVLinkDomainIDs[0]), logger)

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, proxyErr := common.ExecutePowerControlWorkflow(
		ctx,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.State,
		identity.RuleID,
		request.OverrideReadinessCheck,
		identity.powerWorkflowID(request.State, request.OverrideReadinessCheck),
		"NVLink Domain",
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	logger.Info().Str("State", request.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// BatchUpdateNVLinkDomainPowerStateHandler power controls one or more NVLink Domains.
type BatchUpdateNVLinkDomainPowerStateHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewBatchUpdateNVLinkDomainPowerStateHandler initializes a batch NVLink Domain
// power-control handler.
func NewBatchUpdateNVLinkDomainPowerStateHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) BatchUpdateNVLinkDomainPowerStateHandler {
	return BatchUpdateNVLinkDomainPowerStateHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Power control NVLink Domains
// @Description Power control one or more NVLink Domains identified by ID.
// @Tags domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchUpdateNVLinkDomainPowerStateRequest true "Batch NVLink Domain power control request"
// @Success 200 {object} model.APIUpdatePowerStateResponse
// @Router /v2/org/{org}/nico/domain/nvlink/power [patch]
func (h BatchUpdateNVLinkDomainPowerStateHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "PowerControlBatch", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	request := model.APIBatchUpdateNVLinkDomainPowerStateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating batch NVLink Domain power control request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate batch NVLink Domain power control request", err)
	}
	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, request.NVLinkDomainIDs, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	h.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("nvlink_domain_ids", identity.NVLinkDomainIDs), logger)

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, proxyErr := common.ExecutePowerControlWorkflow(
		ctx,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.State,
		identity.RuleID,
		request.OverrideReadinessCheck,
		identity.powerWorkflowID(request.State, request.OverrideReadinessCheck),
		"NVLink Domains",
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	logger.Info().Str("State", request.State).Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdatePowerStateResponse(flowResp))
}

// UpdateNVLinkDomainFirmwareHandler updates firmware on one NVLink Domain identified by
// UUID.
type UpdateNVLinkDomainFirmwareHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewUpdateNVLinkDomainFirmwareHandler initializes an NVLink Domain firmware-update handler.
func NewUpdateNVLinkDomainFirmwareHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) UpdateNVLinkDomainFirmwareHandler {
	return UpdateNVLinkDomainFirmwareHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Firmware update an NVLink Domain
// @Description Update firmware on an NVLink Domain identified by ID.
// @Tags domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the NVLink Domain"
// @Param body body model.APINVLinkDomainFirmwareUpdateRequest true "NVLink Domain firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/domain/nvlink/{id}/firmware [patch]
func (h UpdateNVLinkDomainFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "FirmwareUpdate", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	nvLinkDomainID := c.Param("id")
	if err := model.ValidateNVLinkDomainID(nvLinkDomainID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	request := model.APINVLinkDomainFirmwareUpdateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating NVLink Domain firmware update request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate NVLink Domain firmware update request", err)
	}
	if request.Version != nil && *request.Version == "" {
		request.Version = nil
	}

	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, []string{nvLinkDomainID}, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("nvlink_domain_id", identity.NVLinkDomainIDs[0]), logger)

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, proxyErr := common.ExecuteFirmwareUpdateWorkflow(
		ctx,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.Version,
		nil,
		nil,
		identity.SiteID,
		identity.RuleID,
		request.OverrideReadinessCheck,
		request.OverrideVersionCheck,
		identity.firmwareWorkflowID(request.Version, request.OverrideReadinessCheck),
		"NVLink Domain",
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}

// BatchUpdateNVLinkDomainFirmwareHandler updates firmware on one or more NVLink Domains.
type BatchUpdateNVLinkDomainFirmwareHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewBatchUpdateNVLinkDomainFirmwareHandler initializes a batch NVLink Domain
// firmware-update handler.
func NewBatchUpdateNVLinkDomainFirmwareHandler(
	dbSession *cdb.Session,
	scp *sc.ClientPool,
) BatchUpdateNVLinkDomainFirmwareHandler {
	return BatchUpdateNVLinkDomainFirmwareHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Firmware update NVLink Domains
// @Description Update firmware on one or more NVLink Domains identified by ID.
// @Tags domain
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIBatchNVLinkDomainFirmwareUpdateRequest true "Batch NVLink Domain firmware update request"
// @Success 200 {object} model.APIUpdateFirmwareResponse
// @Router /v2/org/{org}/nico/domain/nvlink/firmware [patch]
func (h BatchUpdateNVLinkDomainFirmwareHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("NVLinkDomain", "FirmwareUpdateBatch", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	request := model.APIBatchNVLinkDomainFirmwareUpdateRequest{}
	if err := c.Bind(&request); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := request.Validate(); err != nil {
		logger.Warn().Err(err).Msg("error validating batch NVLink Domain firmware update request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate batch NVLink Domain firmware update request", err)
	}
	if request.Version != nil && *request.Version == "" {
		request.Version = nil
	}
	identity, err := newNVLinkDomainOperationWorkflowIdentity(request.SiteID, request.NVLinkDomainIDs, request.RuleID)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	h.tracerSpan.SetAttribute(handlerSpan, attribute.StringSlice("nvlink_domain_ids", identity.NVLinkDomainIDs), logger)

	site, err := common.GetSiteFromIDString(ctx, nil, identity.SiteID, h.dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}
	if site.Status != cdbm.SiteStatusRegistered {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site is not in Registered state, unable to execute operation on Site", nil)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowResp, proxyErr := common.ExecuteFirmwareUpdateWorkflow(
		ctx,
		logger,
		stc,
		model.NVLinkDomainTargetSpec(identity.NVLinkDomainIDs),
		request.Version,
		nil,
		nil,
		identity.SiteID,
		identity.RuleID,
		request.OverrideReadinessCheck,
		request.OverrideVersionCheck,
		identity.firmwareWorkflowID(request.Version, request.OverrideReadinessCheck),
		"NVLink Domains",
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIUpdateFirmwareResponse(flowResp))
}
