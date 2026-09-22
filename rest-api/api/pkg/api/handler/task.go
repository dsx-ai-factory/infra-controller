// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/http"

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

// ~~~~~ Get Task Handler ~~~~~ //

// GetTaskHandler is the API Handler for getting a Task by ID
type GetTaskHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetTaskHandler initializes and returns a new handler for getting a Task
func NewGetTaskHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetTaskHandler {
	return GetTaskHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Get a Task
// @Description Get a Task by UUID
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Task"
// @Param siteId query string true "ID of the Site"
// @Success 200 {object} model.APITask
// @Router /v2/org/{org}/nico/rack/task/{id} [get]
func (gth GetTaskHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Task", "Get", c, gth.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	var apiRequest model.APIGetTaskRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
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

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, gth.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	taskID := c.Param("id")
	if _, err := uuid.Parse(taskID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Task ID specified in URL", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, gth.dbSession)
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

	stc, err := gth.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.GetTasksByIDsRequest{
		TaskIds: []*flowv1.UUID{{Id: taskID}},
	}

	var flowResponse flowv1.GetTasksByIDsResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_GetTasksByIDs_FullMethodName,
		flowRequest, &flowResponse,
		fmt.Sprintf("task-get-%s", taskID), temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	tasks := flowResponse.GetTasks()
	if len(tasks) == 0 {
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "Task not found", nil)
	}

	apiTask := model.NewAPITask(tasks[0], model.WithTaskReport())

	logger.Info().Msg("finishing API handler")

	return c.JSON(http.StatusOK, apiTask)
}

// ~~~~~ Cancel Task Handler ~~~~~ //

// CancelTaskHandler is the API Handler for cancelling a Task by ID.
//
// Cancellation is best-effort and idempotent: tasks in non-terminal states
// (Pending, Running, Waiting) are marked Terminated and any underlying
// Temporal workflow is terminated. Already-Terminated tasks are returned
// unchanged. Tasks that have already finished (Succeeded or Failed) cannot
// be cancelled and yield an error from Flow. The handler returns 202 Accepted
// with the task as last reported by Flow.
type CancelTaskHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewCancelTaskHandler initializes and returns a new handler for cancelling a Task
func NewCancelTaskHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) CancelTaskHandler {
	return CancelTaskHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Cancel a Task
// @Description Cancel a Task by UUID. Best-effort and idempotent.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "UUID of the Task"
// @Param body body model.APICancelTaskRequest true "Cancel task request"
// @Success 202 {object} model.APITask
// @Router /v2/org/{org}/nico/rack/task/{id}/cancel [post]
func (cth CancelTaskHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Task", "Cancel", c, cth.tracerSpan)
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

	// Validate role, only Provider Admins are allowed to cancel a Task
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Get Infrastructure Provider for org
	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, cth.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Get task ID from URL param
	taskID := c.Param("id")
	cth.tracerSpan.SetAttribute(handlerSpan, attribute.String("task_id", taskID), logger)
	if _, err := uuid.Parse(taskID); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Invalid Task ID specified in URL", nil)
	}

	// Parse and validate request body
	apiRequest := model.APICancelTaskRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if verr := apiRequest.Validate(); verr != nil {
		logger.Warn().Err(verr).Msg("error validating cancel task request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate cancel task request data", verr)
	}

	// Validate the site
	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, cth.dbSession)
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

	// Verify the Site has NICo Flow enabled (Tasks only exist on Flow sites)
	siteConfig := &cdbm.SiteConfig{}
	if site.Config != nil {
		siteConfig = site.Config
	}
	if !siteConfig.Flow {
		logger.Warn().Msg("site does not have NICo Flow enabled")
		return cutil.NewAPIErrorResponse(c, http.StatusPreconditionFailed, "Site does not have NICo Flow enabled", nil)
	}

	// Get the temporal client for the site
	stc, err := cth.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.CancelTaskRequest{
		TaskId: &flowv1.UUID{Id: taskID},
	}

	workflowID := fmt.Sprintf("task-cancel-%s", taskID)
	var flowResponse flowv1.CancelTaskResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_CancelTask_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	apiTask := model.NewAPITask(flowResponse.GetTask(), model.WithTaskReport())

	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, apiTask)
}

// ~~~~~ List Tasks Handlers ~~~~~ //

// GetAllTaskHandler is the API Handler for listing all Tasks in a Site.
type GetAllTaskHandler struct {
	dbSession  *cdb.Session
	scp        *sc.ClientPool
	tracerSpan *cutil.TracerSpan
}

// NewGetAllTaskHandler initializes a new GetAllTaskHandler.
func NewGetAllTaskHandler(dbSession *cdb.Session, scp *sc.ClientPool) GetAllTaskHandler {
	return GetAllTaskHandler{
		dbSession:  dbSession,
		scp:        scp,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve all Tasks
// @Description List all Tasks in the requested Site, with optional active-only and pagination filters.
// @Tags task
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param siteId query string true "UUID of the Site"
// @Param activeOnly query boolean false "Restrict to non-terminal Tasks"
// @Param includeReport query boolean false "Include the per-task execution report in each response (default false)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Success 200 {array} model.APITask
// @Router /v2/org/{org}/nico/task [get]
func (h GetAllTaskHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("Task", "GetAll", c, h.tracerSpan)
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

	var apiRequest model.APIGetTasksRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("site_id", apiRequest.SiteID), logger)

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, h.dbSession)
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
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if err := pageRequest.Validate(nil); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.ListTasksRequest{
		ActiveOnly: apiRequest.ActiveOnly,
		WithReport: apiRequest.IncludeReport,
	}
	if pageRequest.Offset != nil && pageRequest.Limit != nil {
		flowRequest.Pagination = &flowv1.Pagination{
			Offset: int32(*pageRequest.Offset),
			Limit:  int32(*pageRequest.Limit),
		}
	}

	workflowID := fmt.Sprintf("task-get-all-%s", common.QueryParamHash(apiRequest.QueryValues(pageRequest)))
	var flowResponse flowv1.ListTasksResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ListTasks_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	taskOpts := apiRequest.TaskOptions()
	apiTasks := make([]*model.APITask, 0, len(flowResponse.GetTasks()))
	for _, task := range flowResponse.GetTasks() {
		apiTasks = append(apiTasks, model.NewAPITask(task, taskOpts...))
	}

	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiTasks)).Int("Total", total).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiTasks)
}

// GetRackTasksHandler is the API Handler for listing Tasks targeting a Rack.
type GetRackTasksHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetRackTasksHandler initializes a new GetRackTasksHandler.
func NewGetRackTasksHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetRackTasksHandler {
	return GetRackTasksHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve all Tasks for a Rack
// @Description List Tasks targeting the given Rack, with optional active-only and pagination filters.
// @Tags rack
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Rack ID"
// @Param siteId query string true "ID of the Site"
// @Param activeOnly query boolean false "Restrict to non-terminal Tasks"
// @Param includeReport query boolean false "Include the per-task execution report in each response (default false)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Success 200 {array} model.APITask
// @Router /v2/org/{org}/nico/rack/{id}/task [get]
func (h GetRackTasksHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("RackTasks", "List", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	rackID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("rack_id", rackID), logger)

	var apiRequest model.APIGetTasksRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
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

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, h.dbSession)
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
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if err := pageRequest.Validate(nil); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.ListTasksRequest{
		RackId:     &flowv1.UUID{Id: rackID},
		ActiveOnly: apiRequest.ActiveOnly,
		WithReport: apiRequest.IncludeReport,
	}
	if pageRequest.Offset != nil && pageRequest.Limit != nil {
		flowRequest.Pagination = &flowv1.Pagination{
			Offset: int32(*pageRequest.Offset),
			Limit:  int32(*pageRequest.Limit),
		}
	}

	workflowID := fmt.Sprintf("tasks-rack-get-%s-%s", rackID, common.QueryParamHash(apiRequest.QueryValues(pageRequest)))
	var flowResponse flowv1.ListTasksResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ListTasks_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	taskOpts := apiRequest.TaskOptions()
	apiTasks := make([]*model.APITask, 0, len(flowResponse.GetTasks()))
	for _, t := range flowResponse.GetTasks() {
		apiTasks = append(apiTasks, model.NewAPITask(t, taskOpts...))
	}

	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiTasks)).Int("Total", total).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiTasks)
}

// GetTrayTasksHandler is the API Handler for listing Tasks targeting a Tray.
type GetTrayTasksHandler struct {
	dbSession  *cdb.Session
	tc         tClient.Client
	scp        *sc.ClientPool
	cfg        *config.Config
	tracerSpan *cutil.TracerSpan
}

// NewGetTrayTasksHandler initializes a new GetTrayTasksHandler.
func NewGetTrayTasksHandler(dbSession *cdb.Session, tc tClient.Client, scp *sc.ClientPool, cfg *config.Config) GetTrayTasksHandler {
	return GetTrayTasksHandler{
		dbSession:  dbSession,
		tc:         tc,
		scp:        scp,
		cfg:        cfg,
		tracerSpan: cutil.NewTracerSpan(),
	}
}

// Handle godoc
// @Summary Retrieve all Tasks for a Tray
// @Description List Tasks targeting the given Tray by component ID, with optional active-only and pagination filters.
// @Tags tray
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param id path string true "Component ID"
// @Param siteId query string true "ID of the Site"
// @Param activeOnly query boolean false "Restrict to non-terminal Tasks"
// @Param includeReport query boolean false "Include the per-task execution report in each response (default false)"
// @Param pageNumber query integer false "Page number of results returned"
// @Param pageSize query integer false "Number of results per page"
// @Success 200 {array} model.APITask
// @Router /v2/org/{org}/nico/tray/{id}/task [get]
func (h GetTrayTasksHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("TrayTasks", "List", c, h.tracerSpan)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	trayID := c.Param("id")
	h.tracerSpan.SetAttribute(handlerSpan, attribute.String("tray_id", trayID), logger)

	var apiRequest model.APIGetTasksRequest
	if err := common.ValidateKnownQueryParams(c.QueryParams(), apiRequest, pagination.PageRequest{}); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, err.Error(), nil)
	}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data", nil)
	}
	if err := apiRequest.Validate(); err != nil {
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

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	infrastructureProvider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	site, err := common.GetSiteFromIDString(ctx, nil, apiRequest.SiteID, h.dbSession)
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
	if err := c.Bind(&pageRequest); err != nil {
		logger.Warn().Err(err).Msg("error binding pagination request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request pagination data", nil)
	}
	if err := pageRequest.Validate(nil); err != nil {
		logger.Warn().Err(err).Msg("error validating pagination request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to validate pagination request data", err)
	}

	stc, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	flowRequest := &flowv1.ListTasksRequest{
		ComponentId: &flowv1.UUID{Id: trayID},
		ActiveOnly:  apiRequest.ActiveOnly,
		WithReport:  apiRequest.IncludeReport,
	}
	if pageRequest.Offset != nil && pageRequest.Limit != nil {
		flowRequest.Pagination = &flowv1.Pagination{
			Offset: int32(*pageRequest.Offset),
			Limit:  int32(*pageRequest.Limit),
		}
	}

	workflowID := fmt.Sprintf("tasks-tray-get-%s-%s", trayID, common.QueryParamHash(apiRequest.QueryValues(pageRequest)))
	var flowResponse flowv1.ListTasksResponse
	proxyErr := common.ProxyFlowGRPC(
		ctx, c, logger, stc,
		flowv1.Flow_ListTasks_FullMethodName,
		flowRequest, &flowResponse,
		workflowID, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_USE_EXISTING,
	)
	if proxyErr != nil {
		return proxyErr
	}

	taskOpts := apiRequest.TaskOptions()
	apiTasks := make([]*model.APITask, 0, len(flowResponse.GetTasks()))
	for _, t := range flowResponse.GetTasks() {
		apiTasks = append(apiTasks, model.NewAPITask(t, taskOpts...))
	}

	total := int(flowResponse.GetTotal())
	pageResponse := pagination.NewPageResponse(*pageRequest.PageNumber, *pageRequest.PageSize, total, pageRequest.OrderByStr)
	pageHeader, err := json.Marshal(pageResponse)
	if err != nil {
		logger.Error().Err(err).Msg("error marshaling pagination response")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to create pagination response", nil)
	}
	c.Response().Header().Set(pagination.ResponseHeaderName, string(pageHeader))

	logger.Info().Int("Count", len(apiTasks)).Int("Total", total).Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiTasks)
}
