// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"fmt"
	"io"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	temporalEnums "go.temporal.io/api/enums/v1"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// ~~~~~ Get All Event Policy Handler ~~~~~ //

// GetAllEventPolicyHandler is the API Handler for listing event policies.
type GetAllEventPolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllEventPolicyHandler initializes and returns a new handler for listing event policies.
func NewGetAllEventPolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetAllEventPolicyHandler {
	return GetAllEventPolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary List event policies
// @Description Returns persisted and built-in policies, including disabled and unbound policies. Filters combine with AND before pagination.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param eventType query string false "Registered event type; omission includes all supported types"
// @Param enabled query bool false "Filter by enabled state"
// @Param bound query bool false "Filter by explicit binding presence"
// @Param pageNumber query int false "Page number" minimum(1) default(1)
// @Param pageSize query int false "Records per page" minimum(1) maximum(100) default(20)
// @Param orderBy query string false "Only ID_ASC is supported" default(ID_ASC)
// @Header 200 {string} X-Pagination "Page number, page size, total matching records, and ordering"
// @Success 200 {array} model.APIEventPolicy
// @Router /v2/org/{org}/nico/event/policy [get]
func (h GetAllEventPolicyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicy", "List", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventPolicyQuery
	page := pagination.PageRequest{OrderByStr: cutil.GetPtr("ID_ASC")}
	err := bindEventQuery(c, &query, page)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	err = bindEventPage(c, &page, "ID_ASC", "id")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := query.ToProto(&page)
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}
	var response flowv1.ListEventRulesResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_ListEventRules_FullMethodName,
		request,
		&response,
		"event-list-policies-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	items := make([]model.APIEventPolicy, 0, len(response.GetRules()))
	for _, value := range response.GetRules() {
		var item model.APIEventPolicy
		err = item.FromProto(value)
		if err != nil {
			logger.Error().Err(err).Msg("failed to render event resource")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render event resource", nil)
		}

		items = append(items, item)
	}

	return writeEventPage(c, page, int(response.GetTotal()), items)
}

// ~~~~~ Create Event Policy Handler ~~~~~ //

// CreateEventPolicyHandler is the API Handler for creating an event policy.
type CreateEventPolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateEventPolicyHandler initializes and returns a new handler for creating an event policy.
func NewCreateEventPolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) CreateEventPolicyHandler {
	return CreateEventPolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Create an event policy
// @Description Creates an immutable policy with its initial enabled state atomically. Enabled defaults to true. An unbound policy does not affect selection.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param body body model.APIEventPolicyCreateRequest true "Event policy"
// @Success 201 {object} model.APIEventPolicy
// @Router /v2/org/{org}/nico/event/policy [post]
func (h CreateEventPolicyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicy", "Create", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	err := common.ValidateKnownQueryParams(c.QueryParams())
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	var body model.APIEventPolicyCreateRequest
	err = decodeEventRequest(c, &body)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = body.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, body.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_CreateEventRule_FullMethodName,
		body.ToProto(),
		&response,
		"event-create-policy-"+uuid.NewString(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return writeEventPolicy(c, logger, &response, http.StatusCreated)
}

// ~~~~~ Get Event Policy Handler ~~~~~ //

// GetEventPolicyHandler is the API Handler for getting an event policy by UUID.
type GetEventPolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetEventPolicyHandler initializes and returns a new handler for getting an event policy by UUID.
func NewGetEventPolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetEventPolicyHandler {
	return GetEventPolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Get an event policy
// @Description Returns the complete policy definition and enabled state.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param policyId path string true "UUID of the policy"
// @Success 200 {object} model.APIEventPolicy
// @Router /v2/org/{org}/nico/event/policy/{policyId} [get]
func (h GetEventPolicyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicy", "Get", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventPolicyGetRequest
	err := bindEventQuery(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	policyID, err := eventPolicyID(c)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEventRule_FullMethodName,
		&flowv1.GetEventRuleRequest{RuleId: policyID},
		&response,
		"event-get-policy-"+policyID.GetId(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return writeEventPolicy(c, logger, &response, http.StatusOK)
}

// ~~~~~ Update Event Policy Handler ~~~~~ //

// UpdateEventPolicyHandler is the API Handler for setting an event policy enabled state.
type UpdateEventPolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateEventPolicyHandler initializes and returns a new handler for setting an event policy enabled state.
func NewUpdateEventPolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) UpdateEventPolicyHandler {
	return UpdateEventPolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Set event policy enabled state
// @Description Accepts only siteId and enabled. Disabled policies retain their bindings but are skipped during policy selection.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param policyId path string true "UUID of the policy"
// @Param body body model.APIEventPolicyUpdateRequest true "Desired enabled state"
// @Success 200 {object} model.APIEventPolicy
// @Router /v2/org/{org}/nico/event/policy/{policyId} [patch]
func (h UpdateEventPolicyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicy", "Update", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	err := common.ValidateKnownQueryParams(c.QueryParams())
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	var body model.APIEventPolicyUpdateRequest
	err = decodeEventRequest(c, &body)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = body.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	policyID, err := eventPolicyID(c)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, body.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRule
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_SetEventRuleEnabled_FullMethodName,
		&flowv1.SetEventRuleEnabledRequest{RuleId: policyID, Enabled: body.Enabled},
		&response,
		fmt.Sprintf("event-set-policy-enabled-%s-%t", policyID.GetId(), *body.Enabled),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return writeEventPolicy(c, logger, &response, http.StatusOK)
}

// ~~~~~ Delete Event Policy Handler ~~~~~ //

// DeleteEventPolicyHandler is the API Handler for deleting an unbound event policy.
type DeleteEventPolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteEventPolicyHandler initializes and returns a new handler for deleting an unbound event policy.
func NewDeleteEventPolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteEventPolicyHandler {
	return DeleteEventPolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Delete an unbound event policy
// @Description Deletes an unbound persisted policy. A policy with explicit bindings returns 409. Built-in policies cannot be deleted.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param policyId path string true "UUID of the policy"
// @Success 204
// @Router /v2/org/{org}/nico/event/policy/{policyId} [delete]
func (h DeleteEventPolicyHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicy", "Delete", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventPolicyDeleteRequest
	err := bindEventQuery(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	policyID, err := eventPolicyID(c)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_DeleteEventRule_FullMethodName,
		&flowv1.DeleteEventRuleRequest{RuleId: policyID},
		nil,
		"event-delete-policy-"+policyID.GetId(),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Get Site Event Policy Binding Handler ~~~~~ //

// GetSiteEventPolicyBindingHandler is the API Handler for getting an explicit Site event-policy binding.
type GetSiteEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetSiteEventPolicyBindingHandler initializes and returns a new handler for getting an explicit Site event-policy binding.
func NewGetSiteEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetSiteEventPolicyBindingHandler {
	return GetSiteEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Get the Site event policy binding
// @Description Returns the explicit assignment, including disabled policies. An absent binding returns 404 without resolving an inherited or built-in policy.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param eventType query string true "Registered event type"
// @Success 200 {object} model.APIEventPolicyBinding
// @Router /v2/org/{org}/nico/event/policy/binding/site [get]
func (h GetSiteEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "Get", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventPolicyBindingQuery
	err := bindEventQuery(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.EventRuleBinding
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEventRuleBinding_FullMethodName,
		&flowv1.GetEventRuleBindingRequest{EventType: query.EventType, Selector: model.EventScopeSelector("")},
		&response,
		"event-get-site-binding-"+query.EventType,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return writeEventPolicyBinding(c, logger, query.SiteID, &response)
}

// ~~~~~ Get Rack Event Policy Binding Handler ~~~~~ //

// GetRackEventPolicyBindingHandler is the API Handler for getting an explicit Rack event-policy binding.
type GetRackEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetRackEventPolicyBindingHandler initializes and returns a new handler for getting an explicit Rack event-policy binding.
func NewGetRackEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetRackEventPolicyBindingHandler {
	return GetRackEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Get the Rack event policy binding
// @Description Returns the explicit assignment, including disabled policies. An absent binding returns 404 without resolving an inherited or built-in policy.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param rackId path string true "External inventory Rack ID"
// @Param eventType query string true "Registered event type"
// @Success 200 {object} model.APIEventPolicyBinding
// @Router /v2/org/{org}/nico/event/policy/binding/rack/{rackId} [get]
func (h GetRackEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "Get", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	rackID := c.Param("rackId")
	if rackID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "rackId is required", nil)
	}

	var query model.APIEventPolicyBindingQuery
	err := bindEventQuery(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := &flowv1.GetEventRuleBindingRequest{
		EventType: query.EventType,
		Selector:  model.EventScopeSelector(rackID),
	}
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}
	var response flowv1.EventRuleBinding
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEventRuleBinding_FullMethodName,
		request,
		&response,
		"event-get-rack-binding-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return writeEventPolicyBinding(c, logger, query.SiteID, &response)
}

// ~~~~~ Put Site Event Policy Binding Handler ~~~~~ //

// PutSiteEventPolicyBindingHandler is the API Handler for assigning an event policy to a Site.
type PutSiteEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewPutSiteEventPolicyBindingHandler initializes and returns a new handler for assigning an event policy to a Site.
func NewPutSiteEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) PutSiteEventPolicyBindingHandler {
	return PutSiteEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Put the Site event policy binding
// @Description Atomically creates or replaces an explicit assignment without changing the referenced policy. Repeating an assignment is idempotent.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param body body model.APISiteEventPolicyBindingRequest true "Event policy assignment"
// @Success 200 {object} model.APIEventPolicyBinding
// @Success 201 {object} model.APIEventPolicyBinding
// @Router /v2/org/{org}/nico/event/policy/binding/site [put]
func (h PutSiteEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "Put", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	err := common.ValidateKnownQueryParams(c.QueryParams())
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	var body model.APISiteEventPolicyBindingRequest
	err = decodeEventRequest(c, &body)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = body.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, body.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var response flowv1.PutEventRuleBindingResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_PutEventRuleBinding_FullMethodName,
		body.ToProto(),
		&response,
		"event-put-site-binding-"+body.EventType+"-"+body.PolicyID,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	status := http.StatusOK
	if response.GetCreated() {
		status = http.StatusCreated
	}
	return writeEventPolicyBindingStatus(c, logger, body.SiteID, response.GetBinding(), status)
}

// ~~~~~ Put Rack Event Policy Binding Handler ~~~~~ //

// PutRackEventPolicyBindingHandler is the API Handler for assigning an event policy to a Rack.
type PutRackEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewPutRackEventPolicyBindingHandler initializes and returns a new handler for assigning an event policy to a Rack.
func NewPutRackEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) PutRackEventPolicyBindingHandler {
	return PutRackEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Put the Rack event policy binding
// @Description Atomically creates or replaces an explicit assignment without changing the referenced policy. Repeating an assignment is idempotent.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param rackId path string true "External inventory Rack ID"
// @Param body body model.APIRackEventPolicyBindingRequest true "Event policy assignment"
// @Success 200 {object} model.APIEventPolicyBinding
// @Success 201 {object} model.APIEventPolicyBinding
// @Router /v2/org/{org}/nico/event/policy/binding/rack/{rackId} [put]
func (h PutRackEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "Put", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	err := common.ValidateKnownQueryParams(c.QueryParams())
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	var body model.APIRackEventPolicyBindingRequest
	err = decodeEventRequest(c, &body)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	body.RackID = c.Param("rackId")
	err = body.Validate()
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, body.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := body.ToProto()
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}

	var response flowv1.PutEventRuleBindingResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_PutEventRuleBinding_FullMethodName,
		request,
		&response,
		"event-put-rack-binding-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	status := http.StatusOK
	if response.GetCreated() {
		status = http.StatusCreated
	}
	return writeEventPolicyBindingStatus(c, logger, body.SiteID, response.GetBinding(), status)
}

// ~~~~~ Delete Site Event Policy Binding Handler ~~~~~ //

// DeleteSiteEventPolicyBindingHandler is the API Handler for removing an explicit Site event-policy binding.
type DeleteSiteEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteSiteEventPolicyBindingHandler initializes and returns a new handler for removing an explicit Site event-policy binding.
func NewDeleteSiteEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteSiteEventPolicyBindingHandler {
	return DeleteSiteEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Delete the Site event policy binding
// @Description Removes the assignment while preserving the policy definition. An absent binding returns 404.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param eventType query string true "Registered event type"
// @Success 204
// @Router /v2/org/{org}/nico/event/policy/binding/site [delete]
func (h DeleteSiteEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "Delete", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventPolicyBindingQuery
	err := bindEventQuery(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_DeleteEventRuleBinding_FullMethodName,
		&flowv1.DeleteEventRuleBindingRequest{
			EventType: query.EventType,
			Selector:  model.EventScopeSelector(""),
		},
		nil,
		"event-delete-site-binding-"+query.EventType,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Delete Rack Event Policy Binding Handler ~~~~~ //

// DeleteRackEventPolicyBindingHandler is the API Handler for removing an explicit Rack event-policy binding.
type DeleteRackEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteRackEventPolicyBindingHandler initializes and returns a new handler for removing an explicit Rack event-policy binding.
func NewDeleteRackEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteRackEventPolicyBindingHandler {
	return DeleteRackEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Delete the Rack event policy binding
// @Description Removes the assignment while preserving the policy definition. An absent binding returns 404.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param rackId path string true "External inventory Rack ID"
// @Param eventType query string true "Registered event type"
// @Success 204
// @Router /v2/org/{org}/nico/event/policy/binding/rack/{rackId} [delete]
func (h DeleteRackEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "Delete", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	rackID := c.Param("rackId")
	if rackID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "rackId is required", nil)
	}

	var query model.APIEventPolicyBindingQuery
	err := bindEventQuery(c, &query)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := &flowv1.DeleteEventRuleBindingRequest{
		EventType: query.EventType,
		Selector:  model.EventScopeSelector(rackID),
	}
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_DeleteEventRuleBinding_FullMethodName,
		request,
		nil,
		"event-delete-rack-binding-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Get All Event Policy Binding Handler ~~~~~ //

// GetAllEventPolicyBindingHandler is the API Handler for listing explicit event policy bindings.
type GetAllEventPolicyBindingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllEventPolicyBindingHandler initializes and returns a new handler for listing explicit event policy bindings.
func NewGetAllEventPolicyBindingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetAllEventPolicyBindingHandler {
	return GetAllEventPolicyBindingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary List event policy bindings
// @Description Lists explicit assignments with filters combined with AND, including disabled policies. Orders by scope, canonical Rack UUID, event type, and binding UUID before pagination.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param eventType query string false "Registered event type; omission includes all supported types"
// @Param scopeType query string false "Site or Rack scope"
// @Param rackId query string false "External inventory Rack ID"
// @Param policyId query string false "UUID of the referenced policy"
// @Param pageNumber query int false "Page number" minimum(1) default(1)
// @Param pageSize query int false "Records per page" minimum(1) maximum(100) default(20)
// @Param orderBy query string false "Only SCOPE_ASC is supported" default(SCOPE_ASC)
// @Header 200 {string} X-Pagination "Page number, page size, total matching records, and ordering"
// @Success 200 {array} model.APIEventPolicyBinding
// @Router /v2/org/{org}/nico/event/policy/binding [get]
func (h GetAllEventPolicyBindingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventPolicyBinding", "List", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventPolicyBindingListRequest
	page := pagination.PageRequest{OrderByStr: cutil.GetPtr("SCOPE_ASC")}
	err := bindEventQuery(c, &query, page)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = bindEventPage(c, &page, "SCOPE_ASC", "scope")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := query.ToProto(&page)
	var response flowv1.ListEventRuleBindingsResponse
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_ListEventRuleBindings_FullMethodName,
		request,
		&response,
		"event-list-bindings-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return writeEventProxyError(c, proxyErr)
	}

	items := make([]model.APIEventPolicyBinding, 0, len(response.GetBindings()))
	for _, value := range response.GetBindings() {
		item := model.APIEventPolicyBinding{SiteID: query.SiteID}
		err = item.FromProto(value)
		if err != nil {
			logger.Error().Err(err).Msg("failed to render event resource")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render event resource", nil)
		}
		items = append(items, item)
	}
	return writeEventPage(c, page, int(response.GetTotal()), items)
}

// ~~~~~ Get All Event Handling Handler ~~~~~ //

// GetAllEventHandlingHandler is the API Handler for listing event handlings.
type GetAllEventHandlingHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllEventHandlingHandler initializes and returns a new handler for listing event handlings.
func NewGetAllEventHandlingHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetAllEventHandlingHandler {
	return GetAllEventHandlingHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary List event handlings
// @Description Returns Flow-computed handling results, captured event and policy metadata, and Task IDs and statuses. Filters combine with AND before pagination. An empty Task list does not imply success.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param eventType query string false "Registered event type; omission includes all supported types"
// @Param policyId query string false "Captured event-policy UUID"
// @Param rackId query string false "External inventory ID of the captured source Rack"
// @Param sourceType query string false "Component or Rack; requires sourceId"
// @Param sourceId query string false "Exact source identifier; requires sourceType"
// @Param pageNumber query int false "Page number" minimum(1) default(1)
// @Param pageSize query int false "Records per page" minimum(1) maximum(100) default(20)
// @Param orderBy query string false "Only HANDLED_AT_DESC is supported" default(HANDLED_AT_DESC)
// @Header 200 {string} X-Pagination "Page number, page size, total matching records, and ordering"
// @Success 200 {array} model.APIEventHandling
// @Router /v2/org/{org}/nico/event/handling [get]
func (h GetAllEventHandlingHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventHandling", "List", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventHandlingQuery
	page := pagination.PageRequest{OrderByStr: cutil.GetPtr("HANDLED_AT_DESC")}
	err := bindEventQuery(c, &query, page)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = bindEventPage(c, &page, "HANDLED_AT_DESC", "handled_at")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := query.ToProto(&page)
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}
	var response flowv1.ListEventHandlingsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_ListEventHandlings_FullMethodName,
		request,
		&response,
		"event-list-handlings-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		if proxyErr.Code == http.StatusNotFound && (query.RackID != nil || query.SourceID != nil) {
			proxyErr.Code = http.StatusBadRequest
		}
		return writeEventProxyError(c, proxyErr)
	}

	items := make([]model.APIEventHandling, 0, len(response.GetHandlings()))
	for _, value := range response.GetHandlings() {
		var item model.APIEventHandling
		err = item.FromProto(value)
		if err != nil {
			logger.Error().Err(err).Msg("failed to render event resource")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render event resource", nil)
		}
		items = append(items, item)
	}

	return writeEventPage(c, page, int(response.GetTotalCount()), items)
}

// ~~~~~ Get All Event Handling Task Handler ~~~~~ //

// GetAllEventHandlingTaskHandler is the API Handler for listing Tasks triggered by event handling.
type GetAllEventHandlingTaskHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllEventHandlingTaskHandler initializes and returns a new handler for listing Tasks triggered by event handling.
func NewGetAllEventHandlingTaskHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetAllEventHandlingTaskHandler {
	return GetAllEventHandlingTaskHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary List event-triggered Tasks
// @Description Lists Tasks using captured event-policy and Rack associations, including terminal Tasks by default. Filters combine with AND before pagination.
// @Tags event
// @Accept json
// @Produce json
// @Security JWTBearerToken
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param eventType query string false "Registered event type; omission includes all supported types"
// @Param policyId query string false "Captured event-policy UUID"
// @Param rackId query string false "External inventory ID of the captured source Rack"
// @Param activeOnly query bool false "Restrict to waiting, pending, and running Tasks"
// @Param pageNumber query int false "Page number" minimum(1) default(1)
// @Param pageSize query int false "Records per page" minimum(1) maximum(100) default(20)
// @Param orderBy query string false "Only CREATED_DESC is supported" default(CREATED_DESC)
// @Header 200 {string} X-Pagination "Page number, page size, total matching records, and ordering"
// @Success 200 {array} model.APITask
// @Router /v2/org/{org}/nico/event/handling/task [get]
func (h GetAllEventHandlingTaskHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, span := common.SetupHandler("EventHandlingTask", "List", c)
	if span != nil {
		defer span.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	var query model.APIEventHandlingTaskQuery
	page := pagination.PageRequest{OrderByStr: cutil.GetPtr("CREATED_DESC")}
	err := bindEventQuery(c, &query, page)
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	err = bindEventPage(c, &page, "CREATED_DESC", "created")
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareFlowHandler(ctx, h.dbSession, h.scp, dbUser, org, query.SiteID, logger)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	request := query.ToProto(&page)
	requestHash, err := common.RequestSHA256(request)
	if err != nil {
		logger.Error().Err(err).Msg("failed to hash event request")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to prepare event request", nil)
	}
	var response flowv1.ListTasksResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_ListEventTasks_FullMethodName,
		request,
		&response,
		"event-list-tasks-"+requestHash,
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		if proxyErr.Code == http.StatusNotFound && query.RackID != nil {
			proxyErr.Code = http.StatusBadRequest
		}
		return writeEventProxyError(c, proxyErr)
	}

	items := make([]*model.APITask, 0, len(response.GetTasks()))
	for _, value := range response.GetTasks() {
		items = append(items, model.NewAPITask(value))
	}
	return writeEventPage(c, page, int(response.GetTotal()), items)
}

// bindEventQuery accepts only selectors declared by the resource's query model.
// Supplying a page also permits the standard pagination query parameters.
func bindEventQuery(c echo.Context, query interface{ Validate() error }, pages ...pagination.PageRequest) error {
	models := []any{query}
	for _, page := range pages {
		models = append(models, page)
	}
	err := common.ValidateKnownQueryParams(c.QueryParams(), models...)
	if err != nil {
		return err
	}
	err = new(echo.DefaultBinder).BindQueryParams(c, query)
	if err != nil {
		return fmt.Errorf("failed to parse event query")
	}
	return query.Validate()
}

func bindEventPage(c echo.Context, page *pagination.PageRequest, order, field string) error {
	err := new(echo.DefaultBinder).BindQueryParams(c, page)
	if err != nil {
		return fmt.Errorf("failed to parse pagination")
	}
	if (page.PageNumber != nil && *page.PageNumber == 0) || (page.PageSize != nil && *page.PageSize == 0) {
		return fmt.Errorf("pageNumber and pageSize must be greater than zero")
	}
	if *page.OrderByStr != order {
		return fmt.Errorf("unsupported orderBy")
	}
	return page.Validate([]string{field})
}

func eventPolicyID(c echo.Context) (*flowv1.UUID, error) {
	id := c.Param("policyId")
	_, err := uuid.Parse(id)
	if err != nil {
		return nil, fmt.Errorf("invalid policy ID")
	}
	return &flowv1.UUID{Id: id}, nil
}

// writeEventProxyError hides unexpected backend details while preserving validation
// errors and public status codes. ProxyFlowGRPC has already logged the full error.
func writeEventProxyError(c echo.Context, apiErr *cutil.APIError) error {
	message := apiErr.Message
	if apiErr.Code == http.StatusInternalServerError {
		message = "Failed to process event request"
	}
	return cutil.NewAPIErrorResponse(c, apiErr.Code, message, nil)
}

func writeEventPolicy(c echo.Context, logger zerolog.Logger, rule *flowv1.EventRule, statusCode int) error {
	var result model.APIEventPolicy
	err := result.FromProto(rule)
	if err != nil {
		logger.Error().Err(err).Msg("failed to render event resource")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render event resource", nil)
	}
	return c.JSON(statusCode, result)
}

func writeEventPolicyBinding(c echo.Context, logger zerolog.Logger, siteID string, binding *flowv1.EventRuleBinding) error {
	return writeEventPolicyBindingStatus(c, logger, siteID, binding, http.StatusOK)
}

func writeEventPolicyBindingStatus(c echo.Context, logger zerolog.Logger, siteID string, binding *flowv1.EventRuleBinding, status int) error {
	result := model.APIEventPolicyBinding{SiteID: siteID}
	err := result.FromProto(binding)
	if err != nil {
		logger.Error().Err(err).Msg("failed to render event resource")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render event resource", nil)
	}
	return c.JSON(status, result)
}

func writeEventPage(c echo.Context, page pagination.PageRequest, total int, items any) error {
	header, err := json.Marshal(pagination.NewPageResponse(*page.PageNumber, *page.PageSize, total, page.OrderByStr))
	if err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(header))
	return c.JSON(http.StatusOK, items)
}

func decodeEventRequest(c echo.Context, body any) error {
	decoder := json.NewDecoder(c.Request().Body)
	decoder.DisallowUnknownFields()
	err := decoder.Decode(body)
	if err != nil {
		return err
	}
	err = decoder.Decode(&struct{}{})
	if err != io.EOF {
		return fmt.Errorf("request body must contain one JSON object")
	}
	return nil
}
