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
	"google.golang.org/protobuf/proto"

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

// prepareRunHandler runs the auth + site lookup + Flow-enabled check +
// Temporal client retrieval shared by every Run handler.
func prepareRunHandler(
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

// ~~~~~ Create Run Handler ~~~~~ //

// CreateTaskRunHandler is the API Handler for creating a Run.
type CreateTaskRunHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCreateTaskRunHandler initializes a new CreateTaskRunHandler.
func NewCreateTaskRunHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) CreateTaskRunHandler {
	return CreateTaskRunHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Create a Run
// @Description Create a Run: a phased, policy-gated execution of one operation across many racks. The configuration is validated server-side; on validation failure no state changes.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param body body model.APITaskRunCreateRequest true "Create run request"
// @Success 201 {object} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run [post]
func (h CreateTaskRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "Create", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	apiRequest := model.APITaskRunCreateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating create run request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, verr.Error(), nil)
	}

	_, stc, apiErr := prepareRunHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest := apiRequest.ToProto()

	// Dedicated workflow ID per request so Create is never deduped.
	workflowID := fmt.Sprintf("task-run-create-%s", uuid.NewString())

	var flowResponse flowv1.CreateOperationRunResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_CreateOperationRun_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED,
	)
	if proxyErr != nil {
		return proxyErr
	}

	// Flow's CreateOperationRun returns only the new run's ID. Echo the known
	// fields so the client gets the canonical identity without an extra GET; a
	// run always starts Pending. The operation type is firmware (the only
	// supported operation today).
	created := &model.APITaskRun{
		ID:            flowResponse.GetId().GetId(),
		Name:          apiRequest.Name,
		Description:   apiRequest.Description,
		OperationType: model.APIOperationTypeFirmwareControl,
		Status:        "Pending",
		StatusReason:  "None",
	}

	logger.Info().Str("RunID", created.ID).Msg("finishing API handler")
	return c.JSON(http.StatusCreated, created)
}

// ~~~~~ Get Run Handler ~~~~~ //

// GetTaskRunHandler is the API Handler for getting a Run by ID.
type GetTaskRunHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetTaskRunHandler initializes a new GetTaskRunHandler.
func NewGetTaskRunHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetTaskRunHandler {
	return GetTaskRunHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a Run
// @Description Get a Run by UUID. Set includeStats=true for derived per-phase outcome counts.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Run"
// @Param siteId query string true "ID of the Site"
// @Param includeStats query boolean false "Include derived per-phase outcome stats (default false)"
// @Success 200 {object} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run/{id} [get]
func (h GetTaskRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "Get", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	runID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("run_id", runID), logger)
	if _, err := uuid.Parse(runID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Run ID specified in URL", nil)
	}

	var apiRequest model.APITaskRunGetRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareRunHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	flowRequest := &flowv1.GetOperationRunRequest{
		Id:           &flowv1.UUID{Id: runID},
		IncludeStats: apiRequest.IncludeStats,
	}
	// IncludeStats is part of the workflow ID because the conflict policy
	// attaches to an in-flight execution with the same ID, which would
	// otherwise return a response whose stats presence contradicts the query.
	workflowID := fmt.Sprintf("task-run-get-%s-%t", runID, apiRequest.IncludeStats)

	var flowResponse flowv1.GetOperationRunResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetOperationRun_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	run := flowResponse.GetOperationRun()
	if run == nil || run.GetSummary() == nil || run.GetSummary().GetId().GetId() == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Run not found", nil)
	}

	apiRun := &model.APITaskRun{}
	apiRun.FromProto(run)
	logger.Info().Str("RunID", apiRun.ID).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiRun)
}

// ~~~~~ List Runs Handler ~~~~~ //

// GetAllTaskRunHandler is the API Handler for listing Runs on a Site.
type GetAllTaskRunHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllTaskRunHandler initializes a new GetAllTaskRunHandler.
func NewGetAllTaskRunHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllTaskRunHandler {
	return GetAllTaskRunHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary List Runs
// @Description List Runs (operation runs) on a Site, newest first, with optional status and operationType filters and pagination.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "ID of the Site"
// @Param status query string false "Filter by run status"
// @Param operationType query string false "Filter by operation type (PowerControl|FirmwareControl)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Success 200 {array} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run [get]
func (h GetAllTaskRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "List", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APITaskRunGetAllRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareRunHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
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

	workflowID := fmt.Sprintf("task-run-get-all-%s", common.QueryParamHash(apiRequest.QueryValues(pageRequest)))

	var flowResponse flowv1.ListOperationRunsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ListOperationRuns_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	apiRuns := make([]*model.APITaskRun, 0, len(flowResponse.GetOperationRuns()))
	for _, s := range flowResponse.GetOperationRuns() {
		apiRun := &model.APITaskRun{}
		apiRun.FromProtoSummary(s)
		apiRuns = append(apiRuns, apiRun)
	}

	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiRuns)).Int("Total", total).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiRuns)
}

// ~~~~~ List Run Targets Handler ~~~~~ //

// GetAllTaskRunTargetHandler is the API Handler for listing a Run's
// per-rack execution targets.
type GetAllTaskRunTargetHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetAllTaskRunTargetHandler initializes a new GetAllTaskRunTargetHandler.
func NewGetAllTaskRunTargetHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllTaskRunTargetHandler {
	return GetAllTaskRunTargetHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary List Run targets
// @Description List a Run's materialized per-rack execution targets, with optional status and phaseScope filters and pagination. Each target references its Task via taskId.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Run"
// @Param siteId query string true "ID of the Site"
// @Param status query string false "Filter by target status"
// @Param phaseScope query string false "Phase scope (currentPhase|completedPhases|currentAndCompletedPhases; default currentPhase)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Success 200 {array} model.APITaskRunTarget
// @Router /v2/org/{org}/nico/task/run/{id}/target [get]
func (h GetAllTaskRunTargetHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRunTargets", "List", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	runID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("run_id", runID), logger)
	if _, err := uuid.Parse(runID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Run ID specified in URL", nil)
	}

	var apiRequest model.APITaskRunTargetGetAllRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	_, stc, apiErr := prepareRunHandler(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, logger, ctx)
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

	flowRequest := apiRequest.ToProto(runID, pageRequest)
	workflowID := fmt.Sprintf("task-run-target-get-all-%s-%s", runID, common.QueryParamHash(apiRequest.QueryValues(pageRequest)))

	var flowResponse flowv1.ListOperationRunTargetsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ListOperationRunTargets_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	apiTargets := make([]*model.APITaskRunTarget, 0, len(flowResponse.GetTargets()))
	for _, t := range flowResponse.GetTargets() {
		apiTarget := &model.APITaskRunTarget{}
		apiTarget.FromProto(t)
		apiTargets = append(apiTargets, apiTarget)
	}

	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiTargets)).Int("Total", total).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiTargets)
}

// ~~~~~ Lifecycle Handlers (pause/resume/advance/cancel) ~~~~~ //

// executeRunLifecycleAction proxies one OperationRun-returning lifecycle call to
// Flow and renders the resulting run. It centralizes the auth/site prep,
// dispatch, and error handling shared by pause/resume/advance/cancel.
func executeRunLifecycleAction(
	c echo.Context,
	dbSession *cdb.Session,
	scp *sc.ClientPool,
	dbUser *cdbm.User,
	org, siteID, runID, action, fullMethod string,
	flowRequest proto.Message,
	logger zerolog.Logger,
	ctx context.Context,
) error {
	_, stc, apiErr := prepareRunHandler(c, dbSession, scp, dbUser, org, siteID, logger, ctx)
	if apiErr != nil {
		return cutil.NewAPIErrorResponse(c, apiErr.Code, apiErr.Message, apiErr.Data)
	}

	workflowID := fmt.Sprintf("task-run-%s-%s", action, runID)

	var flowResponse flowv1.OperationRun
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		fullMethod,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	apiRun := &model.APITaskRun{}
	apiRun.FromProto(&flowResponse)
	logger.Info().Str("RunID", apiRun.ID).Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, apiRun)
}

// PauseTaskRunHandler pauses a running Run.
type PauseTaskRunHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewPauseTaskRunHandler initializes a new PauseTaskRunHandler.
func NewPauseTaskRunHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) PauseTaskRunHandler {
	return PauseTaskRunHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Pause a Run
// @Description Pause a running Run. In-flight target tasks continue; no new targets are claimed until resumed.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Run"
// @Param body body model.APITaskRunSiteRequest true "Pause run request"
// @Success 202 {object} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run/{id}/pause [post]
func (h PauseTaskRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "Pause", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	runID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("run_id", runID), logger)
	if _, err := uuid.Parse(runID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Run ID specified in URL", nil)
	}

	apiRequest := model.APITaskRunSiteRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	flowRequest := &flowv1.PauseOperationRunRequest{Id: &flowv1.UUID{Id: runID}}
	return executeRunLifecycleAction(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, runID, "pause", flowv1.Flow_PauseOperationRun_FullMethodName, flowRequest, logger, ctx)
}

// ResumeTaskRunHandler resumes an operator-paused Run.
type ResumeTaskRunHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewResumeTaskRunHandler initializes a new ResumeTaskRunHandler.
func NewResumeTaskRunHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) ResumeTaskRunHandler {
	return ResumeTaskRunHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Resume a Run
// @Description Resume an operator-paused Run. A Run paused at a phase gate must be advanced with the advance endpoint instead.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Run"
// @Param body body model.APITaskRunSiteRequest true "Resume run request"
// @Success 202 {object} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run/{id}/resume [post]
func (h ResumeTaskRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "Resume", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	runID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("run_id", runID), logger)
	if _, err := uuid.Parse(runID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Run ID specified in URL", nil)
	}

	apiRequest := model.APITaskRunSiteRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	flowRequest := &flowv1.ResumeOperationRunRequest{Id: &flowv1.UUID{Id: runID}}
	return executeRunLifecycleAction(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, runID, "resume", flowv1.Flow_ResumeOperationRun_FullMethodName, flowRequest, logger, ctx)
}

// AdvanceTaskRunPhaseHandler opens the next phase of a phase-gated Run.
type AdvanceTaskRunPhaseHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewAdvanceTaskRunPhaseHandler initializes a new AdvanceTaskRunPhaseHandler.
func NewAdvanceTaskRunPhaseHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) AdvanceTaskRunPhaseHandler {
	return AdvanceTaskRunPhaseHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Advance a Run to its next phase
// @Description Open the next phase of a Run paused at a phase gate. Optionally guard with expectedPhaseIndex so a stale client cannot advance the wrong phase.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Run"
// @Param body body model.APITaskRunAdvanceRequest true "Advance run request"
// @Success 202 {object} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run/{id}/advance [post]
func (h AdvanceTaskRunPhaseHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "AdvancePhase", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	runID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("run_id", runID), logger)
	if _, err := uuid.Parse(runID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Run ID specified in URL", nil)
	}

	apiRequest := model.APITaskRunAdvanceRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	flowRequest := &flowv1.AdvanceOperationRunPhaseRequest{
		Id:                 &flowv1.UUID{Id: runID},
		ExpectedPhaseIndex: apiRequest.ExpectedPhaseIndex,
	}
	return executeRunLifecycleAction(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, runID, "advance", flowv1.Flow_AdvanceOperationRunPhase_FullMethodName, flowRequest, logger, ctx)
}

// CancelTaskRunHandler cancels a Run and its in-flight targets.
type CancelTaskRunHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCancelTaskRunHandler initializes a new CancelTaskRunHandler.
func NewCancelTaskRunHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) CancelTaskRunHandler {
	return CancelTaskRunHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Cancel a Run
// @Description Cancel a Run. Best-effort cancellation cascades to the current phase's in-flight target tasks.
// @Tags run
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Run"
// @Param body body model.APITaskRunCancelRequest true "Cancel run request"
// @Success 202 {object} model.APITaskRun
// @Router /v2/org/{org}/nico/task/run/{id}/cancel [post]
func (h CancelTaskRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TaskRun", "Cancel", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	runID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("run_id", runID), logger)
	if _, err := uuid.Parse(runID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Run ID specified in URL", nil)
	}

	apiRequest := model.APITaskRunCancelRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}

	flowRequest := &flowv1.CancelOperationRunRequest{
		Id:     &flowv1.UUID{Id: runID},
		Reason: apiRequest.Reason,
	}
	return executeRunLifecycleAction(c, h.dbSession, h.scp, dbUser, org, apiRequest.SiteID, runID, "cancel", flowv1.Flow_CancelOperationRun_FullMethodName, flowRequest, logger, ctx)
}
