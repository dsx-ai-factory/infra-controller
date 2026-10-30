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
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

const leakageEventType = string(flowtypes.EventTypeHardwareLeakDetected)

// ~~~~~ Get Site Leakage Policy Handler ~~~~~ //

// GetSiteLeakagePolicyHandler is the API Handler for getting the effective Site leakage policy.
type GetSiteLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetSiteLeakagePolicyHandler initializes and returns a new handler for getting the effective Site leakage policy.
func NewGetSiteLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) GetSiteLeakagePolicyHandler {
	return GetSiteLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Retrieve the effective Site leakage policy
// @Description Returns the enabled Site override or the built-in automatic-shutdown fallback.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/leakage/policy [get]
func (h GetSiteLeakagePolicyHandler) Handle(c echo.Context) error {
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

// ~~~~~ Put Site Leakage Policy Handler ~~~~~ //

// PutSiteLeakagePolicyHandler is the API Handler for creating or replacing the Site leakage-policy override.
type PutSiteLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewPutSiteLeakagePolicyHandler initializes and returns a new handler for replacing the Site leakage-policy override.
func NewPutSiteLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) PutSiteLeakagePolicyHandler {
	return PutSiteLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Create or replace the Site leakage policy override
// @Description Idempotently replaces the Site policy slot.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param body body model.APIPutLeakagePolicyRequest true "Leakage policy"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/leakage/policy [put]
func (h PutSiteLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "put-policy", c)
	if span != nil {
		defer span.End()
	}

	_, stc, apiErr := prepareLeakagePolicyHandler(c, h.dbSession, h.scp, dbUser, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	var body model.APIPutLeakagePolicyRequest
	if err := decodeLeakagePolicyRequest(c, &body); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := body.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
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

// ~~~~~ Delete Site Leakage Policy Handler ~~~~~ //

// DeleteSiteLeakagePolicyHandler is the API Handler for deleting the Site leakage-policy override.
type DeleteSiteLeakagePolicyHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteSiteLeakagePolicyHandler initializes and returns a new handler for deleting the Site leakage-policy override.
func NewDeleteSiteLeakagePolicyHandler(db *cdb.Session, scp *sc.ClientPool, cfg *config.Config) DeleteSiteLeakagePolicyHandler {
	return DeleteSiteLeakagePolicyHandler{dbSession: db, scp: scp, cfg: cfg}
}

// Handle godoc
// @Summary Remove the Site leakage policy override
// @Description Idempotently removes the Site override and reveals the built-in policy.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Success 204
// @Router /v2/org/{org}/nico/leakage/policy [delete]
func (h DeleteSiteLeakagePolicyHandler) Handle(c echo.Context) error {
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
// @Param siteId query string true "ID of the Site"
// @Param body body model.APIPutLeakagePolicyRequest true "Leakage policy"
// @Success 200 {object} model.APILeakagePolicy
// @Router /v2/org/{org}/nico/rack/{rackId}/leakage/policy [put]
func (h PutRackLeakagePolicyHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakagePolicy", "put-policy", c)
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

	var body model.APIPutLeakagePolicyRequest
	if err := decodeLeakagePolicyRequest(c, &body); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := body.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
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
// @Description Returns leakage policy-handling records for exactly one source using cursor pagination.
// @Tags leakage
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param sourceType query string true "Kind of inventory source"
// @Param sourceId query string true "Public component or Rack identifier"
// @Param limit query int false "Maximum number of records" minimum(1) maximum(100) default(1)
// @Param cursor query string false "Opaque continuation cursor"
// @Success 200 {object} model.APILeakageHandlingPage
// @Router /v2/org/{org}/nico/leakage/handling [get]
func (h GetLeakageHandlingHandler) Handle(c echo.Context) error {
	_, dbUser, ctx, logger, span := common.SetupHandler("LeakageHandling", "Get", c)
	if span != nil {
		defer span.End()
	}

	var query model.APILeakageHandlingQuery
	if err := common.ValidateKnownQueryParams(c.QueryParams(), query); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&query); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse leakage handling query", nil)
	}
	if err := query.Validate(); err != nil {
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

	var response flowv1.GetEventHandlingResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx,
		logger,
		stc,
		flowv1.Flow_GetEventHandling_FullMethodName,
		&flowv1.GetEventHandlingRequest{
			EventType:    leakageEventType,
			ResourceType: query.ProtoResourceType(),
			ResourceId:   query.SourceID,
			Limit:        query.EffectiveLimit(),
			Cursor:       query.Cursor,
		},
		&response,
		leakageHandlingWorkflowID(query),
		temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return cutil.NewAPIErrorResponse(c, proxyErr.Code, proxyErr.Message, nil)
	}

	items := make([]model.APILeakageHandling, 0, len(response.GetHandlings()))
	for _, value := range response.GetHandlings() {
		items = append(items, model.NewAPILeakageHandling(value))
	}

	result := model.APILeakageHandlingPage{Items: items}
	if response.GetNextCursor() != "" {
		result.NextCursor = cutil.GetPtr(response.GetNextCursor())
	}

	return c.JSON(http.StatusOK, result)
}

func leakageHandlingWorkflowID(query model.APILeakageHandlingQuery) string {
	// Hash the normalized Flow request identity so equivalent REST inputs, such
	// as an omitted limit and limit=1, share an in-flight workflow. SiteID is
	// excluded because the Temporal namespace already isolates workflows by Site.
	identity := struct {
		EventType    string
		ResourceType flowv1.EventResourceType
		ResourceID   string
		Limit        int32
		Cursor       string
	}{
		EventType:    leakageEventType,
		ResourceType: query.ProtoResourceType(),
		ResourceID:   query.SourceID,
		Limit:        query.EffectiveLimit(),
		Cursor:       query.Cursor,
	}

	return "leakage-handling-" + common.RequestHash(identity)
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
	if err := common.ValidateKnownQueryParams(c.QueryParams(), query); err != nil {
		return "", nil, cutil.NewAPIError(http.StatusBadRequest, err.Error(), nil)
	}
	if err := new(echo.DefaultBinder).BindQueryParams(c, &query); err != nil {
		return "", nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to parse query parameters", nil)
	}
	if err := query.Validate(); err != nil {
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
	if err := result.FromProto(rule); err != nil {
		logger.Error().Err(err).Msg("failed to render leakage policy")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render leakage policy", nil)
	}
	return c.JSON(http.StatusOK, result)
}

func decodeLeakagePolicyRequest(c echo.Context, destination *model.APIPutLeakagePolicyRequest) error {
	decoder := json.NewDecoder(c.Request().Body)
	var fields map[string]json.RawMessage
	if err := decoder.Decode(&fields); err != nil {
		return err
	}
	if err := decoder.Decode(&struct{}{}); err != io.EOF {
		return fmt.Errorf("request body must contain one JSON object")
	}
	if len(fields) != 3 {
		return fmt.Errorf("request body must contain exactly action, target, and onConflict")
	}
	action, actionFound := fields["action"]
	target, targetFound := fields["target"]
	conflict, conflictFound := fields["onConflict"]
	if !actionFound || !targetFound || !conflictFound {
		return fmt.Errorf("request body must contain action, target, and onConflict")
	}
	if err := json.Unmarshal(action, &destination.Action); err != nil {
		return fmt.Errorf("invalid action: %w", err)
	}
	if err := json.Unmarshal(target, &destination.Target); err != nil {
		return fmt.Errorf("invalid target: %w", err)
	}
	if err := json.Unmarshal(conflict, &destination.OnConflict); err != nil {
		return fmt.Errorf("invalid onConflict: %w", err)
	}
	return nil
}
