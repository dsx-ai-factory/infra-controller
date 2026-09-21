// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"fmt"
	"net/http"

	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"go.opentelemetry.io/otel/attribute"
	tclient "go.temporal.io/sdk/client"
	tp "go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	auth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// ~~~~~ Create Handler ~~~~~ //

// CreateMachineValidationTestHandler is the API Handler for creating new MachineValidationTest
type CreateMachineValidationTestHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateMachineValidationTestHandler initializes and returns a new handler for creating MachineValidationTest
func NewCreateMachineValidationTestHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) CreateMachineValidationTestHandler {
	return CreateMachineValidationTestHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create a MachineValidationTest
// @Description Create a MachineValidationTest
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIMachineValidationTestCreateRequest true "MachineValidationTest creation request"
// @Success 201 {object} model.APIMachineValidationTest
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/test [post]
func (handler CreateMachineValidationTestHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationTest", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to create MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIMachineValidationTestCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating MachineValidationTest creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating MachineValidationTest creation request data", verr)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which this test is being created
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	createWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       "machine-validation-test-create-" + apiRequest.Name,
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf create request
	createProtoRequest := apiRequest.ToProto()

	logger.Info().Msg("triggering MachineValidationTest create workflow")

	createCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	createWorkflowRun, err := temporalClient.ExecuteWorkflow(createCtx, createWorkflowOptions, "AddMachineValidationTest", createProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to create MachineValidationTest")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to create MachineValidationTest on Site: %s", err), nil)
	}
	createWorkflowID := createWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", createWorkflowID).Msg("executed synchronous create MachineValidationTest workflow")

	// Execute create workflow synchronously
	var createProtoResponse *corev1.MachineValidationTestAddUpdateResponse
	err = createWorkflowRun.Get(createCtx, &createProtoResponse)

	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || createCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, createWorkflowID, err, "MachineValidationTest", "AddMachineValidationTest")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to create MachineValidationTest")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to create MachineValidationTest on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", createWorkflowID).Msg("completed synchronous create MachineValidationTest workflow")

	// get newly created test
	createdTest, err := getMachineValidationTest(ctx, c, logger, temporalClient, createProtoResponse.GetTestId(), createProtoResponse.GetVersion())
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationTest")
		return err
	}

	// Create response
	apiObject := model.NewAPIMachineValidationTest(createdTest)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiObject)
}

// UpdateMachineValidationTestHandler is the API Handler for update existing MachineValidationTest
type UpdateMachineValidationTestHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateMachineValidationTestHandler initializes and returns a new handler for updating MachineValidationTest
func NewUpdateMachineValidationTestHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateMachineValidationTestHandler {
	return UpdateMachineValidationTestHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update a MachineValidationTest
// @Description Update a MachineValidationTest
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIMachineValidationTestUpdateRequest true "MachineValidationTest update request"
// @Success 201 {object} model.APIMachineValidationTest
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/test/{id}/version/{version} [patch]
func (handler UpdateMachineValidationTestHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationTest", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// get ID of the test
	testID := c.Param("id")
	cotel.SetAttribute(handlerSpan, attribute.String("machine_validation_test_id", testID))
	testVersion := c.Param("version")
	cotel.SetAttribute(handlerSpan, attribute.String("machine_validation_test_version", testVersion))

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIMachineValidationTestUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which this test is being updated
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	updateWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-test-update-%s-%s", testID, testVersion),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf update request
	updateProtoRequest := apiRequest.ToProto(testID, testVersion)

	logger.Info().Msg("triggering MachineValidationTest update workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	updateWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, updateWorkflowOptions, "UpdateMachineValidationTest", updateProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to update MachineValidationTest")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to update MachineValidationTest on Site: %s", err), nil)
	}
	updateWorkflowID := updateWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", updateWorkflowID).Msg("executed synchronous update MachineValidationTest workflow")

	// Execute update workflow synchronously
	var updateProtoResponse *corev1.MachineValidationTestAddUpdateResponse
	err = updateWorkflowRun.Get(updateCtx, &updateProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, updateWorkflowID, err, "MachineValidationTest", "UpdateMachineValidationTest")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to update MachineValidationTest")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to update MachineValidationTest on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", updateWorkflowID).Msg("completed synchronous update MachineValidationTest workflow")

	// get updated test
	updatedTest, err := getMachineValidationTest(ctx, c, logger, temporalClient, updateProtoResponse.GetTestId(), updateProtoResponse.GetVersion())
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationTest")
		return err
	}

	// Create response
	apiObject := model.NewAPIMachineValidationTest(updatedTest)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiObject)
}

func getMachineValidationTest(ctx context.Context, echoCtx echo.Context, logger zerolog.Logger, temporalClient tclient.Client, testID string, testVersion string) (*corev1.MachineValidationTest, error) {
	// get newly created test to be returned
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-test-get-%s-%s", testID, testVersion),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}

	// build protobuf request
	getProtoRequest := &corev1.MachineValidationTestsGetRequest{
		TestId:  cutil.GetPtr(testID),
		Version: cutil.GetPtr(testVersion),
	}

	logger.Info().Msg("triggering MachineValidationTest get workflow")

	// Add context deadlines
	getCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(getCtx, getWorkflowOptions, "GetMachineValidationTests", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationTest")
		return nil, cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get MachineValidationTest on Site: %s", err), nil)
	}

	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationTest workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.MachineValidationTestsGetResponse
	err = getWorkflowRun.Get(getCtx, &getProtoResponse)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationTest")
		return nil, cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get MachineValidationTest on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationTest workflow")

	if len(getProtoResponse.Tests) != 1 {
		logger.Error().Err(err).Msgf("expected to get 1 MachineValidationTest and instead got %d", len(getProtoResponse.Tests))
		return nil, cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Expected to get 1 MachineValidationTest from Site and instead got %d", len(getProtoResponse.Tests)), nil)
	}
	return getProtoResponse.Tests[0], nil
}

// GetAllMachineValidationTestHandler is the API Handler to get all MachineValidationTests
type GetAllMachineValidationTestHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllMachineValidationTestHandler initializes and returns a new handler to get all MachineValidationTests
func NewGetAllMachineValidationTestHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllMachineValidationTestHandler {
	return GetAllMachineValidationTestHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all MachineValidationTests
// @Description Get all MachineValidationTests
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} []model.APIMachineValidationTest
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/test [get]
func (handler GetAllMachineValidationTestHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationTest", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// get filter query params
	filter := model.APIMachineValidationTestsFilter{}
	if err := c.Bind(&filter); err != nil {
		logger.Warn().Err(err).Msg("error binding query data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse query data, potentially invalid structure", nil)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which we query tests
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       "machine-validation-test-getall",
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf update request
	getProtoRequest := filter.ToProto()

	logger.Info().Msg("triggering MachineValidationTest get workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, getWorkflowOptions, "GetMachineValidationTests", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationTests")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get MachineValidationTests on Site: %s", err), nil)
	}
	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationTests workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.MachineValidationTestsGetResponse
	err = getWorkflowRun.Get(updateCtx, &getProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, getWorkflowID, err, "MachineValidationTest", "UpdateMachineValidationTest")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationTests")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get MachineValidationTests on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationTests workflow")

	// Create response
	var response []*model.APIMachineValidationTest
	for _, proto := range getProtoResponse.GetTests() {
		response = append(response, model.NewAPIMachineValidationTest(proto))
	}
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, response)
}

// GetMachineValidationTestHandler is the API Handler to get MachineValidationTest
type GetMachineValidationTestHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetMachineValidationTestHandler initializes and returns a new handler to get MachineValidationTest
func NewGetMachineValidationTestHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) GetMachineValidationTestHandler {
	return GetMachineValidationTestHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get MachineValidationTest
// @Description Get MachineValidationTest
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APIMachineValidationTest
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/test [get]
func (handler GetMachineValidationTestHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationTest", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// get ID of the test
	testID := c.Param("id")
	cotel.SetAttribute(handlerSpan, attribute.String("machine_validation_test_id", testID))
	testVersion := c.Param("version")
	cotel.SetAttribute(handlerSpan, attribute.String("machine_validation_test_version", testVersion))

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which we query tests
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-test-get-%s-%s", testID, testVersion),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf update request
	getProtoRequest := &corev1.MachineValidationTestsGetRequest{
		TestId:  &testID,
		Version: &testVersion,
	}

	logger.Info().Msg("triggering MachineValidationTest get workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, getWorkflowOptions, "GetMachineValidationTests", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationTests")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get MachineValidationTests on Site: %s", err), nil)
	}
	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationTests workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.MachineValidationTestsGetResponse
	err = getWorkflowRun.Get(updateCtx, &getProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, getWorkflowID, err, "MachineValidationTest", "UpdateMachineValidationTest")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationTests")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get MachineValidationTests on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationTests workflow")

	if len(getProtoResponse.Tests) == 0 {
		logger.Error().Err(err).Msg("expected to get 1 MachineValidationTest and instead got 0")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "No MachineValidationTest found", nil)
	} else if len(getProtoResponse.Tests) > 1 {
		logger.Error().Err(err).Msgf("expected to get 1 MachineValidationTest and instead got %d", len(getProtoResponse.Tests))
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "More than one MachineValidationTest found", nil)
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIMachineValidationTest(getProtoResponse.Tests[0]))
}

func getMachineForValidation(ctx context.Context, logger zerolog.Logger, dbSession *cdb.Session, provider *cdbm.InfrastructureProvider, machineID string) (*cdbm.Machine, *cutil.APIError) {
	machine, err := cdbm.NewMachineDAO(dbSession).GetByID(ctx, nil, machineID, []string{cdbm.SiteRelationName}, false)
	if err != nil {
		if errors.Is(err, cdb.ErrDoesNotExist) {
			return nil, cutil.NewAPIError(http.StatusNotFound, "Could not find Machine with specified ID", nil)
		}
		logger.Error().Err(err).Msg("failed to retrieve Machine details from DB")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Machine details, DB error", nil)
	}

	if machine.InfrastructureProviderID != provider.ID {
		logger.Error().Msg("Machine doesn't belong to org's Infrastructure provider")
		return nil, cutil.NewAPIError(http.StatusNotFound, "Could not find Machine with specified ID", nil)
	}

	if machine.Site == nil {
		logger.Error().Msg("Related Site was not returned for Machine DB entity")
		return nil, cutil.NewAPIError(http.StatusInternalServerError, "Failed to retrieve Site details for Machine, DB error", nil)
	}

	return machine, nil
}

// CreateMachineValidationRunHandler creates an on-demand validation run for a Machine.
type CreateMachineValidationRunHandler struct {
	dbSession *cdb.Session
	scp       *sc.ClientPool
}

// NewCreateMachineValidationRunHandler returns a new CreateMachineValidationRunHandler.
func NewCreateMachineValidationRunHandler(dbSession *cdb.Session, scp *sc.ClientPool, _ *config.Config) CreateMachineValidationRunHandler {
	return CreateMachineValidationRunHandler{
		dbSession: dbSession,
		scp:       scp,
	}
}

// Handle godoc
// @Summary Create an on-demand Machine validation run
// @Description Create an on-demand validation run for a Machine.
// @Tags Machine Validation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Param request body model.APIMachineValidationRunCreateRequest false "On-demand Machine validation options"
// @Success 202 {object} model.APIMachineValidationRun
// @Router /v2/org/{org}/nico/machine/{machineId}/validation/run [post]
func (h CreateMachineValidationRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationRun", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}

	if dbUser == nil {
		logger.Error().Msg("Invalid User object found in request context")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}

	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("Error validating org membership for User in request")
		} else {
			logger.Warn().Msg("Could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	if !auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole) {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	machineID := c.Param("id")
	if machineID == "" {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine ID was not specified in URL", nil)
	}

	apiRequest := model.APIMachineValidationRunCreateRequest{}
	if err := c.Bind(&apiRequest); err != nil {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	if err := apiRequest.Validate(); err != nil {
		logger.Warn().Err(err).Msg("Error validating Machine Validation Run creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine Validation Run creation request data", err)
	}

	provider, err := common.GetInfrastructureProviderForOrg(ctx, nil, h.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	machine, apiError := getMachineForValidation(ctx, logger, h.dbSession, provider, machineID)
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	if machine.IsMissingOnSite {
		logger.Error().Msg("Machine is missing on site, unable to start on-demand validation")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Machine is missing on site, unable to start on-demand validation", nil)
	}

	site := machine.Site
	if site.Status != cdbm.SiteStatusRegistered {
		logger.Warn().Msg("Site specified in request data is not in Registered state")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request data is not in Registered state, cannot execute admin operation", nil)
	}

	siteClient, err := h.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve workflow client for Site", nil)
	}

	logger.Info().Str("machine_id", machineID).Str("site_id", site.ID.String()).Msg("Starting on-demand Machine validation via Core gRPC proxy")

	coreResponse := &corev1.MachineValidationOnDemandResponse{}
	apiError = common.ExecuteCoreGRPC(
		ctx,
		siteClient,
		corev1.Forge_OnDemandMachineValidation_FullMethodName,
		apiRequest.ToProto(machineID),
		coreResponse,
		site.ID.String(),
	)
	if apiError != nil {
		logAPIError(logger, apiError, "Failed to start on-demand Machine validation via Core gRPC proxy")
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, nil)
	}

	return c.JSON(http.StatusAccepted, model.NewAPIMachineValidationRunFromOnDemandResponse(coreResponse))
}

func resolveMachineValidationTarget(ctx context.Context, c echo.Context, logger zerolog.Logger, dbSession *cdb.Session, provider *cdbm.InfrastructureProvider) (string, *cdbm.Site, *cutil.APIError) {
	machineID := c.Param("id")
	if machineID != "" {
		machine, apiError := getMachineForValidation(ctx, logger, dbSession, provider, machineID)
		if apiError != nil {
			return machineID, nil, apiError
		}
		return machineID, machine.Site, nil
	}

	machineID = c.Param("machineID")
	siteID := c.Param("siteID")
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return machineID, nil, cutil.NewAPIError(http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	if site.InfrastructureProviderID != provider.ID {
		return machineID, nil, cutil.NewAPIError(http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	return machineID, site, nil
}

// GetMachineValidationResultsHandler is the API Handler to get MachineValidationResults
type GetMachineValidationResultsHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetMachineValidationResultsHandler initializes and returns a new handler to get MachineValidationResults
func NewGetMachineValidationResultsHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) GetMachineValidationResultsHandler {
	return GetMachineValidationResultsHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get Machine validation results
// @Description Get Machine validation results
// @Tags Machine Validation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Success 200 {object} []model.APIMachineValidationResult
// @Router /v2/org/{org}/nico/machine/{machineId}/validation/result [get]
func (handler GetMachineValidationResultsHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationResult", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to retrieve Machine validation results
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	machineID, site, apiError := resolveMachineValidationTarget(ctx, c, logger, handler.dbSession, ip)
	cotel.SetAttribute(handlerSpan, attribute.String("machine_id", machineID))
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       "machine-validation-result-get-" + machineID,
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf request
	getProtoRequest := &corev1.MachineValidationGetRequest{
		MachineId:      &corev1.MachineId{Id: machineID},
		IncludeHistory: true,
	}

	logger.Info().Msg("triggering MachineValidationResult get workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, getWorkflowOptions, "GetMachineValidationResults", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationResults")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get MachineValidationResults on Site: %s", err), nil)
	}
	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationResults workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.MachineValidationResultList
	err = getWorkflowRun.Get(updateCtx, &getProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, getWorkflowID, err, "MachineValidationResult", "GetMachineValidationResults")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationResults")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get MachineValidationResults on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationResults workflow")

	// Create response
	response := make([]*model.APIMachineValidationResult, 0, len(getProtoResponse.GetResults()))
	for _, proto := range getProtoResponse.GetResults() {
		response = append(response, model.NewAPIMachineValidationResult(proto))
	}
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, response)
}

// GetAllMachineValidationRunHandler is the API Handler to get all MachineValidationRuns
type GetAllMachineValidationRunHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllMachineValidationRunHandler initializes and returns a new handler to get all MachineValidationRuns
func NewGetAllMachineValidationRunHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllMachineValidationRunHandler {
	return GetAllMachineValidationRunHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get Machine validation runs
// @Description Get Machine validation runs
// @Tags Machine Validation
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param machineId path string true "ID of Machine"
// @Success 200 {object} []model.APIMachineValidationRun
// @Router /v2/org/{org}/nico/machine/{machineId}/validation/run [get]
func (handler GetAllMachineValidationRunHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationRun", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to retrieve Machine validation runs
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	machineID, site, apiError := resolveMachineValidationTarget(ctx, c, logger, handler.dbSession, ip)
	cotel.SetAttribute(handlerSpan, attribute.String("machine_id", machineID))
	if apiError != nil {
		return cutil.NewAPIErrorResponse(c, apiError.Code, apiError.Message, apiError.Data)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       "machine-validation-run-get-" + machineID,
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf request
	getProtoRequest := &corev1.MachineValidationRunListGetRequest{
		MachineId:      &corev1.MachineId{Id: machineID},
		IncludeHistory: true,
	}

	logger.Info().Msg("triggering MachineValidationRun get workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, getWorkflowOptions, "GetMachineValidationRuns", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationRuns")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get MachineValidationRuns on Site: %s", err), nil)
	}
	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationRuns workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.MachineValidationRunList
	err = getWorkflowRun.Get(updateCtx, &getProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, getWorkflowID, err, "MachineValidationRun", "GetMachineValidationRuns")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationRuns")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get MachineValidationRuns on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationRuns workflow")

	// Create response
	response := make([]*model.APIMachineValidationRun, 0, len(getProtoResponse.GetRuns()))
	for _, proto := range getProtoResponse.GetRuns() {
		response = append(response, model.NewAPIMachineValidationRun(proto))
	}
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, response)
}

// GetAllMachineValidationExternalConfigHandler is the API Handler to get all MachineValidationExternalConfigs
type GetAllMachineValidationExternalConfigHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetAllMachineValidationExternalConfigHandler initializes and returns a new handler to get all MachineValidationExternalConfigs
func NewGetAllMachineValidationExternalConfigHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) GetAllMachineValidationExternalConfigHandler {
	return GetAllMachineValidationExternalConfigHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get all MachineValidationExternalConfigs
// @Description Get all MachineValidationExternalConfigs
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} []model.APIMachineValidationExternalConfig
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/external-config [get]
func (handler GetAllMachineValidationExternalConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationExternalConfig", "GetAll", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which we query tests
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       "machine-validation-ext-config-get-all",
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf request
	getProtoRequest := &corev1.GetMachineValidationExternalConfigsRequest{}

	logger.Info().Msg("triggering MachineValidationRun get workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, getWorkflowOptions, "GetMachineValidationExternalConfigs", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationExternalConfigs")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get Machine Validation External Configs on Site: %s", err), nil)
	}
	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationExternalConfigs workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.GetMachineValidationExternalConfigsResponse
	err = getWorkflowRun.Get(updateCtx, &getProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, getWorkflowID, err, "MachineValidationExternalConfig", "GetMachineValidationExternalConfigs")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationExternalConfigs")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get Machine Validation External Config on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationExternalConfigs workflow")

	// Create response
	var response []*model.APIMachineValidationExternalConfig
	for _, proto := range getProtoResponse.GetConfigs() {
		response = append(response, model.NewAPIMachineValidationExternalConfig(proto))
	}
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, response)
}

// GetMachineValidationExternalConfigHandler is the API Handler to get MachineValidationExternalConfig
type GetMachineValidationExternalConfigHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewGetMachineValidationExternalConfigHandler initializes and returns a new handler to get MachineValidationTest
func NewGetMachineValidationExternalConfigHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) GetMachineValidationExternalConfigHandler {
	return GetMachineValidationExternalConfigHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Get MachineValidationExternalConfig
// @Description Get MachineValidationExternalConfig
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 200 {object} model.APIMachineValidationExternalConfig
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/external-config/{name} [get]
func (handler GetMachineValidationExternalConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationExternalConfig", "Get", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// get ID of the test
	cfgName := c.Param("cfgName")
	cotel.SetAttribute(handlerSpan, attribute.String("external_config_name", cfgName))

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which we query tests
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-ext-cfg-get-%s", cfgName),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf update request
	getProtoRequest := &corev1.GetMachineValidationExternalConfigsRequest{
		Names: []string{cfgName},
	}

	logger.Info().Msg("triggering MachineValidationExternalConfig get workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, getWorkflowOptions, "GetMachineValidationExternalConfigs", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get Machine Validation External Config on Site: %s", err), nil)
	}
	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationExternalConfig workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.GetMachineValidationExternalConfigsResponse
	err = getWorkflowRun.Get(updateCtx, &getProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, getWorkflowID, err, "MachineValidationExternalConfig", "GetMachineValidationExternalConfigs")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get Machine Validation External Config on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationExternalConfig workflow")

	if len(getProtoResponse.Configs) == 0 {
		logger.Error().Err(err).Msg("expected to get 1 MachineValidationExternalConfig and instead got 0")
		return cutil.NewAPIErrorResponse(c, http.StatusNotFound, "No Machine Validation External Config found", nil)
	} else if len(getProtoResponse.Configs) > 1 {
		logger.Error().Err(err).Msgf("expected to get 1 MachineValidationExternalConfig and instead got %d", len(getProtoResponse.Configs))
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "More than one Machine Validation External Config found", nil)
	}

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, model.NewAPIMachineValidationExternalConfig(getProtoResponse.Configs[0]))
}

// CreateMachineValidationExternalConfigHandler is the API Handler for creating new MachineValidationExternalConfig
type CreateMachineValidationExternalConfigHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewCreateMachineValidationExternalConfigHandler initializes and returns a new handler for creating MachineValidationExternalConfig
func NewCreateMachineValidationExternalConfigHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) CreateMachineValidationExternalConfigHandler {
	return CreateMachineValidationExternalConfigHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Create a MachineValidationExternalConfig
// @Description Create a MachineValidationExternalConfig
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIMachineValidationExternalConfigCreateRequest true "MachineValidationExternalConfig creation request"
// @Success 201 {object} model.APIMachineValidationExternalConfig
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/external-config [post]
func (handler CreateMachineValidationExternalConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationExternalConfig", "Create", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to create MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// Validate request
	// Bind request data to API model
	apiRequest := model.APIMachineValidationExternalConfigCreateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}
	// Validate request attributes
	verr := apiRequest.Validate()
	if verr != nil {
		logger.Warn().Err(verr).Msg("error validating MachineValidationExternalConfig creation request data")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error validating Machine Validation External Config creation request data", verr)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which this test is being created
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	createWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       "machine-validation-ext-cfg-create-" + apiRequest.Name,
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf create request
	createProtoRequest := apiRequest.ToProto()

	logger.Info().Msg("triggering MachineValidationExternalConfig create workflow")

	createCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	createWorkflowRun, err := temporalClient.ExecuteWorkflow(createCtx, createWorkflowOptions, "AddUpdateMachineValidationExternalConfig", createProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to create MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to create Machine Validation External Config on Site: %s", err), nil)
	}
	createWorkflowID := createWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", createWorkflowID).Msg("executed synchronous create MachineValidationExternalConfig workflow")

	// Execute create workflow synchronously
	err = createWorkflowRun.Get(createCtx, nil)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || createCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, createWorkflowID, err, "MachineValidationExternalConfig", "AddUpdateMachineValidationExternalConfig")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to create MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to create Machine Validation External Config on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", createWorkflowID).Msg("completed synchronous create MachineValidationExternalConfig workflow")

	// get newly created test
	createdExtCfg, err := getMachineValidationExtCfg(ctx, c, logger, temporalClient, apiRequest.Name)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get created MachineValidationExternalConfig")
		return err
	}

	// Create response
	apiObject := model.NewAPIMachineValidationExternalConfig(createdExtCfg)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusCreated, apiObject)
}

// UpdateMachineValidationExternalConfigHandler is the API Handler for update existing MachineValidationExternalConfig
type UpdateMachineValidationExternalConfigHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewUpdateMachineValidationExternalConfigHandler initializes and returns a new handler for updating MachineValidationExternalConfig
func NewUpdateMachineValidationExternalConfigHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) UpdateMachineValidationExternalConfigHandler {
	return UpdateMachineValidationExternalConfigHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Update a MachineValidationExternalConfig
// @Description Update a MachineValidationExternalConfig
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Param message body model.APIMachineValidationExternalConfigUpdateRequest true "MachineValidationExternalConfig update request"
// @Success 200 {object} model.APIMachineValidationExternalConfig
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/external-config/{name} [patch]
func (handler UpdateMachineValidationExternalConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationExternalConfig", "Update", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// get name
	extCfgName := c.Param("cfgName")
	cotel.SetAttribute(handlerSpan, attribute.String("external_config_name", extCfgName))

	// Bind request data to API model
	apiRequest := model.APIMachineValidationExternalConfigUpdateRequest{}
	err = c.Bind(&apiRequest)
	if err != nil {
		logger.Warn().Err(err).Msg("error binding request data into API model")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to parse request data, potentially invalid structure", nil)
	}

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which this test is being updated
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// get existing external config
	currentExtCfgProto, err := getMachineValidationExtCfg(ctx, c, logger, temporalClient, extCfgName)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get existing MachineValidationExternalConfig")
		return err
	}

	// create workflow
	updateWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-ext-cfg-update-%s", extCfgName),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}
	// build protobuf update request
	updateProtoRequest := &corev1.AddUpdateMachineValidationExternalConfigRequest{
		Name:        extCfgName,
		Description: currentExtCfgProto.Description,
		Config:      currentExtCfgProto.Config,
	}
	if apiRequest.Description != nil {
		updateProtoRequest.Description = apiRequest.Description
	}
	if apiRequest.Config != nil {
		updateProtoRequest.Config = apiRequest.Config
	}

	logger.Info().Msg("triggering MachineValidationExternalConfig update workflow")

	updateCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	updateWorkflowRun, err := temporalClient.ExecuteWorkflow(updateCtx, updateWorkflowOptions, "AddUpdateMachineValidationExternalConfig", updateProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to update MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to update Machine Validation External Config on Site: %s", err), nil)
	}
	updateWorkflowID := updateWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", updateWorkflowID).Msg("executed synchronous update MachineValidationExternalConfig workflow")

	// Execute update workflow synchronously
	var updateProtoResponse *corev1.MachineValidationTestAddUpdateResponse
	err = updateWorkflowRun.Get(updateCtx, &updateProtoResponse)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || updateCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, updateWorkflowID, err, "MachineValidationExternalConfig", "AddUpdateMachineValidationExternalConfig")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to update MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to update Machine Validation External Config on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", updateWorkflowID).Msg("completed synchronous update MachineValidationExternalConfig workflow")

	// get updated external config
	updatedExtCfgProto, err := getMachineValidationExtCfg(ctx, c, logger, temporalClient, extCfgName)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get updated MachineValidationExternalConfig")
		return err
	}

	// Create response
	apiObject := model.NewAPIMachineValidationExternalConfig(updatedExtCfgProto)
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusOK, apiObject)
}

func getMachineValidationExtCfg(ctx context.Context, echoCtx echo.Context, logger zerolog.Logger, temporalClient tclient.Client, cfgName string) (*corev1.MachineValidationExternalConfig, error) {
	// get newly created test to be returned
	getWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-ext-cfg-get-%s", cfgName),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}

	// build protobuf request
	getProtoRequest := &corev1.GetMachineValidationExternalConfigsRequest{
		Names: []string{cfgName},
	}

	logger.Info().Msg("triggering MachineValidationExternalConfig get workflow")

	// Add context deadlines
	getCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	getWorkflowRun, err := temporalClient.ExecuteWorkflow(getCtx, getWorkflowOptions, "GetMachineValidationExternalConfigs", getProtoRequest)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to get MachineValidationExternalConfig")
		return nil, cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to get Machine Validation External Config on Site: %s", err), nil)
	}

	getWorkflowID := getWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", getWorkflowID).Msg("executed synchronous get MachineValidationExternalConfig workflow")

	// Execute get workflow synchronously
	var getProtoResponse *corev1.GetMachineValidationExternalConfigsResponse
	err = getWorkflowRun.Get(getCtx, &getProtoResponse)
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to get MachineValidationExternalConfig")
		return nil, cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to get Machine Validation External Config on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", getWorkflowID).Msg("completed synchronous get MachineValidationExternalConfig workflow")

	if len(getProtoResponse.Configs) != 1 {
		logger.Error().Err(err).Msgf("expected to get 1 MachineValidationTest and instead got %d", len(getProtoResponse.Configs))
		return nil, cutil.NewAPIErrorResponse(echoCtx, http.StatusInternalServerError, fmt.Sprintf("Expected to get 1 Machine Validation External Config from Site and instead got %d", len(getProtoResponse.Configs)), nil)
	}
	return getProtoResponse.Configs[0], nil
}

// DeleteMachineValidationExternalConfigHandler is the API Handler for delete existing MachineValidationExternalConfig
type DeleteMachineValidationExternalConfigHandler struct {
	dbSession *cdb.Session
	tc        tclient.Client
	scp       *sc.ClientPool
	cfg       *config.Config
}

// NewDeleteMachineValidationExternalConfigHandler initializes and returns a new handler for updating MachineValidationExternalConfig
func NewDeleteMachineValidationExternalConfigHandler(dbSession *cdb.Session, tc tclient.Client, scp *sc.ClientPool, cfg *config.Config) DeleteMachineValidationExternalConfigHandler {
	return DeleteMachineValidationExternalConfigHandler{
		dbSession: dbSession,
		tc:        tc,
		scp:       scp,
		cfg:       cfg,
	}
}

// Handle godoc
// @Summary Delete a MachineValidationExternalConfig
// @Description Delete a MachineValidationExternalConfig
// @Tags MachineValidationTest
// @Accept json
// @Produce json
// @Security ApiKeyAuth
// @Param org path string true "Name of NGC organization"
// @Success 202
// @Router /v2/org/{org}/nico/site/{site}/machine-validation/external-config/{name} [delete]
func (handler DeleteMachineValidationExternalConfigHandler) Handle(c echo.Context) error {
	org, dbUser, ctx, logger, handlerSpan := common.SetupHandler("MachineValidationExternalConfig", "Delete", c)
	if handlerSpan != nil {
		defer handlerSpan.End()
	}
	if dbUser == nil {
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve current user", nil)
	}
	siteID := c.Param("siteID")

	// Validate org
	ok, err := auth.ValidateOrgMembership(dbUser, org)
	if !ok {
		if err != nil {
			logger.Error().Err(err).Msg("error validating org membership for User in request")
		} else {
			logger.Warn().Msg("could not validate org membership for user, access denied")
		}
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, fmt.Sprintf("Failed to validate membership for org: %s", org), nil)
	}

	// Validate role, only Provider Admins are allowed to update MachineValidationTest
	ok = auth.ValidateUserRoles(dbUser, org, nil, auth.ProviderAdminRole)
	if !ok {
		logger.Warn().Msg("user does not have Provider Admin role, access denied")
		return cutil.NewAPIErrorResponse(c, http.StatusForbidden, "User does not have Provider Admin role with org", nil)
	}

	// get name
	extCfgName := c.Param("cfgName")
	cotel.SetAttribute(handlerSpan, attribute.String("external_config_name", extCfgName))

	// Check that infrastructureProvider exists in org
	ip, err := common.GetInfrastructureProviderForOrg(ctx, nil, handler.dbSession, org)
	if err != nil {
		logger.Warn().Err(err).Msg("error getting infrastructure provider for org")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Failed to retrieve Infrastructure Provider for org", nil)
	}

	// Validate the site for which this test is being updated
	site, err := common.GetSiteFromIDString(ctx, nil, siteID, handler.dbSession)
	if err != nil {
		logger.Warn().Err(err).Str("Site ID", siteID).Msg("error getting site from request")
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Error retrieving Site in request", nil)
	}
	// verify site's infrastructure provider matches org's infrastructure provider
	if site.InfrastructureProviderID != ip.ID {
		return cutil.NewAPIErrorResponse(c, http.StatusBadRequest, "Site specified in request doesn't belong to current org's Provider", nil)
	}

	// Get the temporal client for the site we are working with
	temporalClient, err := handler.scp.GetClientByID(site.ID)
	if err != nil {
		logger.Error().Err(err).Msg("failed to retrieve Temporal client for Site")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, "Failed to retrieve client for Site", nil)
	}

	// create workflow
	deleteWorkflowOptions := tclient.StartWorkflowOptions{
		ID:                       fmt.Sprintf("machine-validation-ext-cfg-delete-%s", extCfgName),
		WorkflowExecutionTimeout: cutil.WorkflowExecutionTimeout,
		TaskQueue:                queue.SiteTaskQueue,
	}

	logger.Info().Msg("triggering MachineValidationExternalConfig delete workflow")

	deleteCtx, cancel := context.WithTimeout(ctx, cutil.WorkflowContextTimeout)
	defer cancel()

	// Trigger Site workflow
	deleteWorkflowRun, err := temporalClient.ExecuteWorkflow(deleteCtx, deleteWorkflowOptions, "RemoveMachineValidationExternalConfig")
	if err != nil {
		logger.Error().Err(err).Msg("failed to synchronously start Temporal workflow to delete update MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to start sync workflow to delete Machine Validation External Config on Site: %s", err), nil)
	}
	deleteWorkflowID := deleteWorkflowRun.GetID()
	logger.Info().Str("Workflow ID", deleteWorkflowID).Msg("executed synchronous delete MachineValidationExternalConfig workflow")

	// Execute update workflow synchronously
	err = deleteWorkflowRun.Get(deleteCtx, nil)
	if err != nil {
		var timeoutErr *tp.TimeoutError
		if errors.As(err, &timeoutErr) || errors.Is(err, context.DeadlineExceeded) || deleteCtx.Err() != nil {
			return common.TerminateWorkflowOnTimeOut(c, logger, temporalClient, deleteWorkflowID, err, "MachineValidationExternalConfig", "RemoveMachineValidationExternalConfig")
		}
		logger.Error().Err(err).Msg("failed to synchronously execute Temporal workflow to delete MachineValidationExternalConfig")
		return cutil.NewAPIErrorResponse(c, http.StatusInternalServerError, fmt.Sprintf("Failed to execute sync workflow to delete Machine Validation External Config on Site: %s", err), nil)
	}

	logger.Info().Str("Workflow ID", deleteWorkflowID).Msg("completed synchronous delete MachineValidationExternalConfig workflow")

	// Create response
	logger.Info().Msg("finishing API handler")
	return c.JSON(http.StatusAccepted, model.NewAPIDeletionAcceptedResponse())
}
