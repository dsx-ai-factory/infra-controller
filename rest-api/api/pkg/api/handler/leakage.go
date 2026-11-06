// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	temporalEnums "go.temporal.io/api/enums/v1"
	tClient "go.temporal.io/sdk/client"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

const leakageEventType = string(flowtypes.EventTypeHardwareLeakDetected)

// GetAllLeakagePolicyHandler lists the effective default and enabled Rack overrides.
type GetAllLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllLeakagePolicyHandler constructs the scoped leakage policy collection handler.
func NewGetAllLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetAllLeakagePolicyHandler {
	return GetAllLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary List leakage policies for a Site
// @Description Returns the effective default once and enabled explicit Rack overrides. Disabled and unbound rules are excluded.
// @Tags leakage
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param pageNumber query int false "Page number" minimum(1) default(1)
// @Param pageSize query int false "Records per page" minimum(1) maximum(100) default(20)
// @Param orderBy query string false "Only SCOPE_ASC is supported: default first, then public Rack ID ascending" default(SCOPE_ASC)
// @Header 200 {string} X-Pagination "Page number, page size, total matching policies, and ordering"
// @Success 200 {array} model.APILeakageScopedPolicy
// @Router /v2/org/{org}/nico/leakage/policy [get]
func (h GetAllLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "list-policy", c)
	if span != nil {
		defer span.End()
	}
	var query model.APILeakagePolicyQuery
	page := pagination.PageRequest{OrderByStr: cutil.GetPtr("SCOPE_ASC")}
	err := common.ValidateKnownQueryParams(c.QueryParams(), query, page)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	binder := new(echo.DefaultBinder)
	err = binder.BindQueryParams(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse policy query", nil)
	}
	err = query.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = binder.BindQueryParams(c, &page)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse pagination query", nil)
	}
	if (page.PageNumber != nil && *page.PageNumber == 0) || (page.PageSize != nil && *page.PageSize == 0) {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "pageNumber and pageSize must be greater than zero", nil)
	}
	if *page.OrderByStr != "SCOPE_ASC" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "orderBy must be SCOPE_ASC", nil)
	}
	err = page.Validate([]string{"scope"})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, c.Param("orgName"), query.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}
	request := query.ToProto(page)
	var response flowv1.ListScopedEventRulesResponse
	apiErr = common.ProxyFlowGRPC(ctx, logger, stc, flowv1.Flow_ListScopedEventRules_FullMethodName,
		request, &response, "leakage-policy-list-"+common.RequestHash(request), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, nil)
	}
	items := make([]model.APILeakageScopedPolicy, 0, len(response.GetPolicies()))
	for _, value := range response.GetPolicies() {
		item := model.APILeakageScopedPolicy{SiteID: query.SiteID}
		err = item.FromProto(value)
		if err != nil {
			logger.Error().Err(err).Msg("failed to render scoped leakage policy")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render leakage policy", nil)
		}
		items = append(items, item)
	}
	header, err := json.Marshal(pagination.NewPageResponse(*page.PageNumber, *page.PageSize, int(response.GetTotal()), page.OrderByStr))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(header))
	return c.JSON(http.StatusOK, items)
}

// ~~~~~ Get Default Leakage Policy Handler ~~~~~ //

// GetDefaultLeakagePolicyHandler is the API Handler for getting the effective Site leakage policy.
type GetDefaultLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetDefaultLeakagePolicyHandler initializes and returns a new handler for getting the effective Site leakage policy.
func NewGetDefaultLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetDefaultLeakagePolicyHandler {
	return GetDefaultLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Retrieve the effective default leakage policy
// @Description Returns the enabled Site override or the built-in automatic-shutdown fallback.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/leakage/policy/default [get]
func (h GetDefaultLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "get-policy", c)
	if span != nil {
		defer span.End()
	}

	_, stc, apiErr := prepareLeakagePolicyHandler(c, h.dbSession, h.scp, dbUser, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEffectiveEventRuleForScope_FullMethodName,
		&flowv1.GetEffectiveEventRuleForScopeRequest{
			EventType: leakageEventType,
			Scope: &flowv1.EventRuleScopeSelector{
				Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
			},
		},
		&response,
		"leakage-policy-get-site",
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	return writeLeakagePolicy(c, logger, &response)
}

// ~~~~~ Put Default Leakage Policy Override Handler ~~~~~ //

// PutDefaultLeakagePolicyOverrideHandler is the API Handler for creating or replacing the Site leakage-policy override.
type PutDefaultLeakagePolicyOverrideHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewPutDefaultLeakagePolicyOverrideHandler initializes and returns a new handler for replacing the Site leakage-policy override.
func NewPutDefaultLeakagePolicyOverrideHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) PutDefaultLeakagePolicyOverrideHandler {
	return PutDefaultLeakagePolicyOverrideHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Create or replace the default leakage policy override
// @Description Idempotently replaces the Site policy slot.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIPutLeakagePolicyRequest true "Leakage policy"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/leakage/policy/default/override [put]
func (h PutDefaultLeakagePolicyOverrideHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "put-policy", c)
	if span != nil {
		defer span.End()
	}

	err := common.ValidateKnownQueryParams(c.QueryParams())
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	var body model.APIPutLeakagePolicyRequest
	err = decodeLeakagePolicyRequest(c, &body)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = body.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, c.Param("orgName"), body.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_SetEventRuleOverride_FullMethodName,
		&flowv1.SetEventRuleOverrideRequest{
			EventType: leakageEventType,
			Scope: &flowv1.EventRuleScopeSelector{
				Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
			},
			Name:        "Hardware leakage policy",
			Description: "Hardware leakage response policy managed through the REST API.",
			Actions:     body.EventRuleActions(),
		},
		&response,
		"leakage-policy-put-"+uuid.NewString(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	return writeLeakagePolicy(c, logger, &response)
}

// ~~~~~ Delete Default Leakage Policy Override Handler ~~~~~ //

// DeleteDefaultLeakagePolicyOverrideHandler is the API Handler for deleting the Site leakage-policy override.
type DeleteDefaultLeakagePolicyOverrideHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteDefaultLeakagePolicyOverrideHandler initializes and returns a new handler for deleting the Site leakage-policy override.
func NewDeleteDefaultLeakagePolicyOverrideHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteDefaultLeakagePolicyOverrideHandler {
	return DeleteDefaultLeakagePolicyOverrideHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Remove the default leakage policy override
// @Description Idempotently removes the Site override and reveals the built-in policy.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Success 204
// @Router /v2/org/{org}/nico/leakage/policy/default/override [delete]
func (h DeleteDefaultLeakagePolicyOverrideHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "delete-policy", c)
	if span != nil {
		defer span.End()
	}

	_, stc, apiErr := prepareLeakagePolicyHandler(c, h.dbSession, h.scp, dbUser, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_DeleteEventRuleOverride_FullMethodName,
		&flowv1.DeleteEventRuleOverrideRequest{
			EventType: leakageEventType,
			Scope: &flowv1.EventRuleScopeSelector{
				Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
			},
		},
		nil,
		"leakage-policy-delete-"+uuid.NewString(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Get Rack Leakage Policy Handler ~~~~~ //

// GetRackLeakagePolicyHandler is the API Handler for getting the effective Rack leakage policy.
type GetRackLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetRackLeakagePolicyHandler initializes and returns a new handler for getting the effective Rack leakage policy.
func NewGetRackLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetRackLeakagePolicyHandler {
	return GetRackLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Retrieve the effective Rack leakage policy
// @Description Returns the Rack override, Site override, or built-in automatic-shutdown fallback.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param rackId path string true "Public Rack identifier"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/rack/{rackId}/leakage/policy [get]
func (h GetRackLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "get-policy", c)
	if span != nil {
		defer span.End()
	}

	_, stc, apiErr := prepareLeakagePolicyHandler(c, h.dbSession, h.scp, dbUser, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	rackID := c.Param("rackId")
	if rackID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "rackId is required", nil)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEffectiveEventRuleForScope_FullMethodName,
		&flowv1.GetEffectiveEventRuleForScopeRequest{
			EventType: leakageEventType,
			Scope: &flowv1.EventRuleScopeSelector{
				Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &flowv1.EventRuleScopeSelector_RackExternalId{
					RackExternalId: rackID,
				},
			},
		},
		&response,
		"leakage-policy-get-rack-"+rackID,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	return writeLeakagePolicy(c, logger, &response)
}

// ~~~~~ Put Rack Leakage Policy Handler ~~~~~ //

// PutRackLeakagePolicyHandler is the API Handler for creating or replacing a Rack leakage-policy override.
type PutRackLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewPutRackLeakagePolicyHandler initializes and returns a new handler for replacing a Rack leakage-policy override.
func NewPutRackLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) PutRackLeakagePolicyHandler {
	return PutRackLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Create or replace a Rack leakage policy override
// @Description Idempotently replaces the Rack policy slot.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param rackId path string true "Public Rack identifier"
// @Param body body model.APIPutLeakagePolicyRequest true "Leakage policy"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/rack/{rackId}/leakage/policy [put]
func (h PutRackLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "put-policy", c)
	if span != nil {
		defer span.End()
	}

	err := common.ValidateKnownQueryParams(c.QueryParams())
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	rackID := c.Param("rackId")
	if rackID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "rackId is required", nil)
	}

	var body model.APIPutLeakagePolicyRequest
	err = decodeLeakagePolicyRequest(c, &body)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = body.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, c.Param("orgName"), body.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_SetEventRuleOverride_FullMethodName,
		&flowv1.SetEventRuleOverrideRequest{
			EventType: leakageEventType,
			Scope: &flowv1.EventRuleScopeSelector{
				Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &flowv1.EventRuleScopeSelector_RackExternalId{
					RackExternalId: rackID,
				},
			},
			Name:        "Hardware leakage policy",
			Description: "Hardware leakage response policy managed through the REST API.",
			Actions:     body.EventRuleActions(),
		},
		&response,
		"leakage-policy-put-"+uuid.NewString(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	return writeLeakagePolicy(c, logger, &response)
}

// ~~~~~ Delete Rack Leakage Policy Handler ~~~~~ //

// DeleteRackLeakagePolicyHandler is the API Handler for deleting a Rack leakage-policy override.
type DeleteRackLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteRackLeakagePolicyHandler initializes and returns a new handler for deleting a Rack leakage-policy override.
func NewDeleteRackLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteRackLeakagePolicyHandler {
	return DeleteRackLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Remove a Rack leakage policy override
// @Description Idempotently removes the Rack override and reveals the Site override or built-in fallback.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param rackId path string true "Public Rack identifier"
// @Param siteId query string true "ID of the Site"
// @Success 204
// @Router /v2/org/{org}/nico/rack/{rackId}/leakage/policy [delete]
func (h DeleteRackLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "delete-policy", c)
	if span != nil {
		defer span.End()
	}

	_, stc, apiErr := prepareLeakagePolicyHandler(c, h.dbSession, h.scp, dbUser, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	rackID := c.Param("rackId")
	if rackID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "rackId is required", nil)
	}

	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_DeleteEventRuleOverride_FullMethodName,
		&flowv1.DeleteEventRuleOverrideRequest{
			EventType: leakageEventType,
			Scope: &flowv1.EventRuleScopeSelector{
				Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
				RackIdentifier: &flowv1.EventRuleScopeSelector_RackExternalId{
					RackExternalId: rackID,
				},
			},
		},
		nil,
		"leakage-policy-delete-"+uuid.NewString(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Get Leakage Handling Handler ~~~~~ //

// GetLeakageHandlingHandler is the API Handler for getting leakage-event handling history.
type GetLeakageHandlingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetLeakageHandlingHandler initializes and returns a new handler for getting leakage-event handling history.
func NewGetLeakageHandlingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetLeakageHandlingHandler {
	return GetLeakageHandlingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Retrieve leakage handling for a component or Rack
// @Description Returns leakage events, captured policy snapshots, tasks, and handling results for exactly one source using pageNumber/pageSize pagination. Newest records are returned first. Dispatch failures are record states with HTTP 200.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param sourceType query string true "Kind of inventory source"
// @Param sourceId query string true "Public component or Rack identifier"
// @Param pageNumber query int false "Page number" minimum(1) default(1)
// @Param pageSize query int false "Records per page" minimum(1) maximum(100) default(1)
// @Param orderBy query string false "Ordering; only HANDLED_AT_DESC is supported" default(HANDLED_AT_DESC)
// @Header 200 {string} X-Pagination "Page number, page size, total matching records, and ordering"
// @Success 200 {array} model.APILeakageHandling
// @Router /v2/org/{org}/nico/leakage/handling [get]
func (h GetLeakageHandlingHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakageHandling", "Get", c)
	if span != nil {
		defer span.End()
	}

	var query model.APILeakageHandlingQuery
	pageRequest := pagination.PageRequest{PageSize: cutil.GetPtr(1), OrderByStr: cutil.GetPtr("HANDLED_AT_DESC")}
	err := common.ValidateKnownQueryParams(c.QueryParams(), query, pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = c.Bind(&query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse leakage handling query", nil)
	}
	err = query.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	err = c.Bind(&pageRequest)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse pagination query", nil)
	}
	if pageRequest.PageNumber != nil && *pageRequest.PageNumber == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "pageNumber must be greater than zero", nil)
	}
	if *pageRequest.PageSize == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "pageSize must be greater than zero", nil)
	}
	if *pageRequest.OrderByStr != "HANDLED_AT_DESC" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "orderBy must be HANDLED_AT_DESC", nil)
	}
	err = pageRequest.Validate([]string{"handled_at"})
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(
		c,
		h.dbSession,
		h.scp,
		dbUser,
		c.Param("orgName"),
		query.SiteID,
		logger,
		ctx,
	)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest := query.ToProto(pageRequest)
	var response flowv1.GetEventHandlingResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEventHandling_FullMethodName,
		flowRequest,
		&response,
		leakageHandlingWorkflowID(flowRequest),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	items := make([]model.APILeakageHandling, 0, len(response.GetHandlings()))
	for _, value := range response.GetHandlings() {
		var item model.APILeakageHandling
		err := item.FromProto(value)
		if err != nil {
			logger.Error().Err(err).Msg("failed to render leakage handling")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render leakage handling", nil)
		}
		items = append(items, item)
	}

	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, int(response.GetTotalCount()), pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))
	return c.JSON(http.StatusOK, items)
}

func leakageHandlingWorkflowID(request *flowv1.GetEventHandlingRequest) string {
	// Source selectors and normalized offset/limit identify one in-flight read.
	// The Temporal namespace already isolates workflows by Site.
	return "leakage-handling-" + common.RequestHash(request)
}

func prepareLeakagePolicyHandler(
	c echo.Context,
	dbSession *cdb.Session,
	scp *sc.ClientPool,
	dbUser *cdbm.User,
	logger zerolog.Logger,
	ctx context.Context,
) (string, tClient.Client, *cutil.APIError) {
	var query model.APILeakagePolicyQuery
	err := common.ValidateKnownQueryParams(c.QueryParams(), query)
	if err != nil {
		return "", nil, cutil.NewAPIError(http.StatusBadRequest, err.Error(), nil)
	}
	err = new(echo.DefaultBinder).BindQueryParams(c, &query)
	if err != nil {
		return "", nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to parse query parameters", nil)
	}
	err = query.Validate()
	if err != nil {
		return "", nil, cutil.NewAPIError(http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(
		c,
		dbSession,
		scp,
		dbUser,
		c.Param("orgName"),
		query.SiteID,
		logger,
		ctx,
	)
	return query.SiteID, stc, apiErr
}

func writeLeakagePolicy(c echo.Context, logger zerolog.Logger, rule *flowv1.EventRule) error {
	var result model.APILeakagePolicy
	err := result.FromProto(rule)
	if err != nil {
		logger.Error().Err(err).Msg("failed to render leakage policy")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render leakage policy", nil)
	}
	return c.JSON(http.StatusOK, result)
}

func decodeLeakagePolicyRequest(c echo.Context, destination *model.APIPutLeakagePolicyRequest) error {
	decoder := json.NewDecoder(c.Request().Body)
	var fields map[string]json.RawMessage
	err := decoder.Decode(&fields)
	if err != nil {
		return err
	}
	err = decoder.Decode(&struct{}{})
	if err != io.EOF {
		return fmt.Errorf("request body must contain one JSON object")
	}
	if len(fields) != 4 {
		return fmt.Errorf("request body must contain exactly siteId, action, targetStrategy, and onConflict")
	}
	siteID, siteIDFound := fields["siteId"]
	action, actionFound := fields["action"]
	targetStrategy, targetStrategyFound := fields["targetStrategy"]
	conflict, conflictFound := fields["onConflict"]
	if !siteIDFound || !actionFound || !targetStrategyFound || !conflictFound {
		return fmt.Errorf("request body must contain siteId, action, targetStrategy, and onConflict")
	}
	err = json.Unmarshal(siteID, &destination.SiteID)
	if err != nil {
		return fmt.Errorf("invalid siteId: %w", err)
	}
	err = json.Unmarshal(action, &destination.Action)
	if err != nil {
		return fmt.Errorf("invalid action: %w", err)
	}
	err = json.Unmarshal(targetStrategy, &destination.TargetStrategy)
	if err != nil {
		return fmt.Errorf("invalid targetStrategy: %w", err)
	}
	err = json.Unmarshal(conflict, &destination.OnConflict)
	if err != nil {
		return fmt.Errorf("invalid onConflict: %w", err)
	}
	return nil
}
