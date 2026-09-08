// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
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

// prepareTaskRuleHandler runs the auth + site lookup + Flow-enabled check +
// Temporal client retrieval shared by every TaskRule handler.
func prepareTaskRuleHandler(
	c echo.Context,
	dbSession *cdb.Session,
	scp *sc.ClientPool,
	dbUser *cdbm.User,
	org string,
	siteIDStr string,
	logger zerolog.Logger,
	ctx context.Context,
) (*cdbm.Site, tClient.Client, *cutil.APIError) {
	if dbUser == nil {
		logger.Error().Msg("invalid User object found in request context")
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, siteIDStr, dbSession)
	if err != nil {
		switch {
		case errors.Is(err, common.ErrInvalidID):
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Failed to validate Site specified in request: invalid ID", nil)
		case errors.Is(err, cdb.ErrDoesNotExist):
			return nil, nil, cutil.NewAPIError(http.StatusBadRequest, "Site specified in request does not exist", nil)
		default:
			logger.Error().Err(err).Msg("error retrieving Site from DB")
			return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site specified in request due to DB error", nil)
		}
	}

	if site.InfrastructureProviderID != infrastructureProvider.ID {
		return nil, nil, cutil.NewAPIError(http.StatusForbidden, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}
	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return nil, nil, cutil.NewAPIError(http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	stc, err := scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return nil, nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	return site, stc, nil
}

// ~~~~~ Create Rule Handler ~~~~~ //

// CreateTaskRuleHandler is the API Handler for creating a new Operation Rule.
type CreateTaskRuleHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateTaskRuleHandler initializes and returns a new handler for creating a Rule.
func NewCreateTaskRuleHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateTaskRuleHandler {
	return CreateTaskRuleHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create an Operation Rule
// @Description Create a new Operation Rule on the target Site. The rule definition is validated server-side; on validation failure no state changes.
// @Tags rule
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APITaskRuleCreateRequest true "Create rule request"
// @Success 201 {object} model.APITaskRule
// @Router /v2/org/{org}/nico/task/rule [post]
func (h CreateTaskRuleHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRule", "Create", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APITaskRuleCreateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating create rule request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, verr.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest, ferr := apiRequest.ToProto()
	if ferr != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, ferr.Error(), nil)
	}

	// Dedicated workflow ID per request so Create is never deduped.
	workflowID := fmt.Sprintf("task-rule-create-%s", uuid.NewString())
	var flowResponse flowv1.CreateOperationRuleResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_CreateOperationRule_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Flow's CreateTaskRule returns only the new rule's ID; echo the
	// request back so the client gets the canonical view without an extra GET.
	created := &model.APITaskRule{
		ID:             flowResponse.GetId().GetId(),
		Name:           apiRequest.Name,
		Description:    apiRequest.Description,
		OperationType:  apiRequest.OperationType,
		OperationCode:  apiRequest.OperationCode,
		RuleDefinition: apiRequest.RuleDefinition,
	}

	logger.Info().Str("RuleID", created.ID).Msg("finishing API handler")
	return c.JSON(http.StatusCreated, created)
}

// ~~~~~ Get Rule Handler ~~~~~ //

// GetTaskRuleHandler is the API Handler for getting an Operation Rule by ID.
type GetTaskRuleHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetTaskRuleHandler initializes and returns a new handler for getting a Rule.
func NewGetTaskRuleHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetTaskRuleHandler {
	return GetTaskRuleHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get an Operation Rule
// @Description Get an Operation Rule by UUID
// @Tags rule
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Rule"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APITaskRule
// @Router /v2/org/{org}/nico/task/rule/{id} [get]
func (h GetTaskRuleHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRule", "Get", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	ruleID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("rule_id", ruleID), logger)
	if _, err := uuid.Parse(ruleID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Rule ID specified in URL", nil)
	}

	var apiRequest model.APITaskRuleGetRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest := &flowv1.GetOperationRuleRequest{
		RuleId: &flowv1.UUID{Id: ruleID},
	}
	workflowID := fmt.Sprintf("task-rule-get-%s", ruleID)
	var flowResponse flowv1.OperationRule
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetOperationRule_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	if flowResponse.GetId() == nil || flowResponse.GetId().GetId() == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Rule not found", nil)
	}

	apiRule := &model.APITaskRule{}
	if err := apiRule.FromProto(&flowResponse); err != nil {
		logger.Error().Err(err).Msg("failed to convert Flow rule to API model")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render Rule response", nil)
	}

	logger.Info().Str("RuleID", apiRule.ID).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiRule)
}

// ~~~~~ List Rules Handler ~~~~~ //

// GetAllTaskRuleHandler is the API Handler for listing Operation Rules.
type GetAllTaskRuleHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllTaskRuleHandler initializes a new GetAllTaskRuleHandler.
func NewGetAllTaskRuleHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllTaskRuleHandler {
	return GetAllTaskRuleHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary List Operation Rules
// @Description List Operation Rules on a Site, with optional operationType filter and pagination.
// @Tags rule
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param operationType query string false "Filter by operation type (PowerControl|FirmwareControl)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Success 200 {array} model.APITaskRule
// @Router /v2/org/{org}/nico/task/rule [get]
func (h GetAllTaskRuleHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRule", "List", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APITaskRuleGetAllRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	pageRequest := pagination.PageRequest{}
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if err := pageRequest.Validate(nil); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	flowRequest, ferr := apiRequest.ToProto(pageRequest)
	if ferr != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, ferr.Error(), nil)
	}

	workflowID := fmt.Sprintf("task-rule-get-all-%s", common.QueryParamHash(apiRequest.QueryValues(pageRequest)))
	var flowResponse flowv1.ListOperationRulesResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ListOperationRules_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	apiRules := make([]*model.APITaskRule, 0, len(flowResponse.GetRules()))
	for _, pbRule := range flowResponse.GetRules() {
		r := &model.APITaskRule{}
		if err := r.FromProto(pbRule); err != nil {
			logger.Error().Err(err).Msg("failed to convert Flow rule to API model")
			return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to render Rule response", nil)
		}
		apiRules = append(apiRules, r)
	}

	total := int(flowResponse.GetTotalCount())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiRules)).Int("Total", total).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiRules)
}

// ~~~~~ Update Rule Handler ~~~~~ //

// UpdateTaskRuleHandler is the API Handler for updating an Operation Rule.
type UpdateTaskRuleHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewUpdateTaskRuleHandler initializes a new UpdateTaskRuleHandler.
func NewUpdateTaskRuleHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateTaskRuleHandler {
	return UpdateTaskRuleHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Update an Operation Rule
// @Description Patch a Rule's mutable fields (name, description, ruleDefinition). operationType and operationCode are immutable; create a new rule to change them.
// @Tags rule
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Rule"
// @Param body body model.APITaskRuleUpdateRequest true "Update rule request"
// @Success 204 "No Content"
// @Router /v2/org/{org}/nico/task/rule/{id} [patch]
func (h UpdateTaskRuleHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRule", "Update", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	ruleID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("rule_id", ruleID), logger)
	if _, err := uuid.Parse(ruleID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Rule ID specified in URL", nil)
	}

	apiRequest := model.APITaskRuleUpdateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating update rule request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, verr.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest, ferr := apiRequest.ToProto(ruleID)
	if ferr != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, ferr.Error(), nil)
	}

	// Dedicated workflow ID per request so concurrent updates to one rule stay
	// separate executions rather than the later one reading the earlier result.
	workflowID := fmt.Sprintf("task-rule-update-%s-%s", ruleID, uuid.NewString())
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_UpdateOperationRule_FullMethodName,
		flowRequest, nil,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return proxyErr
	}

	logger.Info().Str("RuleID", ruleID).Msg("finishing API handler")
	return c.NoContent(http.StatusNoContent)
}

// ~~~~~ Delete Rule Handler ~~~~~ //

// DeleteTaskRuleHandler is the API Handler for deleting an Operation Rule.
//
// Flow rejects deletion of rules that are still associated with racks or that
// are the active default for an operation. The caller must dissociate first;
// this handler surfaces the Flow error verbatim via UnwrapWorkflowError so the
// client gets a meaningful 4xx.
type DeleteTaskRuleHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewDeleteTaskRuleHandler initializes a new DeleteTaskRuleHandler.
func NewDeleteTaskRuleHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteTaskRuleHandler {
	return DeleteTaskRuleHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Delete an Operation Rule
// @Description Delete an Operation Rule by UUID. Rules associated with a rack or active as a default must be dissociated first.
// @Tags rule
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Rule"
// @Param siteId query string true "ID of the Site"
// @Success 204 "No Content"
// @Router /v2/org/{org}/nico/task/rule/{id} [delete]
func (h DeleteTaskRuleHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRule", "Delete", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	ruleID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("rule_id", ruleID), logger)
	if _, err := uuid.Parse(ruleID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Rule ID specified in URL", nil)
	}

	var apiRequest model.APITaskRuleDeleteRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareTaskRuleHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest := &flowv1.DeleteOperationRuleRequest{
		RuleId: &flowv1.UUID{Id: ruleID},
	}
	workflowID := fmt.Sprintf("task-rule-delete-%s", ruleID)
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_DeleteOperationRule_FullMethodName,
		flowRequest, nil,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	logger.Info().Str("RuleID", ruleID).Msg("finishing API handler")
	return c.NoContent(http.StatusNoContent)
}
