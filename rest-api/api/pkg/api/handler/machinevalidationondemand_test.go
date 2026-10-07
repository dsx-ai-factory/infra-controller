// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

type machineValidationOnDemandHandlerFixture struct {
	dbSession  *cdb.Session
	org        string
	machineID  string
	siteID     uuid.UUID
	user       interface{}
	handler    echo.HandlerFunc
	proxiedReq *grpcproxy.Request
}

func newMachineValidationOnDemandHandlerFixture(t *testing.T, response *corev1.MachineValidationOnDemandResponse) machineValidationOnDemandHandlerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)

	org := "test-org"
	user := common.TestBuildUser(t, dbSession, "test-starfleet-id", org, []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, dbSession, "Test Infrastructure Provider", org, user)
	site := common.TestBuildSite(t, dbSession, provider, "Test Site", user)
	siteDAO := cdbm.NewSiteDAO(dbSession)
	_, err := siteDAO.Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: site.ID,
		Status: cutil.GetPtr(cdbm.SiteStatusRegistered),
	})
	require.NoError(t, err)
	instanceType := common.TestBuildInstanceType(t, dbSession, "test-instance-type", cutil.GetPtr(site.ID), site, nil, user)
	machine := common.TestBuildMachine(t, dbSession, provider, site, &instanceType.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)

	proxiedReq := &grpcproxy.Request{}
	workflowRun := &tmocks.WorkflowRun{}
	workflowRun.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) {
		if response == nil {
			return
		}
		out := args.Get(1).(*grpcproxy.Response)
		responseJSON, err := protojson.Marshal(response)
		require.NoError(t, err)
		out.ResponseJSON = responseJSON
	}).Return(nil)

	siteTemporalClient := &tmocks.Client{}
	siteTemporalClient.On(
		"ExecuteWorkflow",
		mock.Anything,
		mock.Anything,
		grpcproxy.Core.WorkflowName,
		mock.MatchedBy(func(request grpcproxy.Request) bool {
			*proxiedReq = request
			return true
		}),
	).Return(workflowRun, nil)

	clientPool := sc.NewClientPool(nil)
	clientPool.IDClientMap[site.ID.String()] = siteTemporalClient

	handler := NewCreateMachineValidationRunHandler(dbSession, clientPool, common.GetTestConfig())
	return machineValidationOnDemandHandlerFixture{
		dbSession:  dbSession,
		org:        org,
		machineID:  machine.ID,
		siteID:     site.ID,
		user:       user,
		handler:    handler.Handle,
		proxiedReq: proxiedReq,
	}
}

func (f machineValidationOnDemandHandlerFixture) request(t *testing.T, body any) *httptest.ResponseRecorder {
	t.Helper()

	var requestBody string
	if body != nil {
		bodyBytes, err := json.Marshal(body)
		require.NoError(t, err)
		requestBody = string(bodyBytes)
	}

	e := echo.New()
	request := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(requestBody))
	if body != nil {
		request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	}
	recorder := httptest.NewRecorder()
	echoContext := e.NewContext(request, recorder)
	echoContext.SetParamNames("orgName", "id")
	echoContext.SetParamValues(f.org, f.machineID)
	echoContext.Set("user", f.user)

	require.NoError(t, f.handler(echoContext))
	return recorder
}

func TestCreateMachineValidationRunHandlerProxiesRequest(t *testing.T) {
	context := "OnDemand"
	fixture := newMachineValidationOnDemandHandlerFixture(t, &corev1.MachineValidationOnDemandResponse{
		ValidationId: &corev1.MachineValidationId{Value: "validation-1"},
		Run: &corev1.MachineValidationRun{
			ValidationId: &corev1.MachineValidationId{Value: "validation-1"},
			MachineId:    &corev1.MachineId{Id: "machine-1"},
			Name:         "Test_machine-1",
			Context:      &context,
		},
	})
	request := model.APIMachineValidationRunCreateRequest{
		Tags:               []string{"history"},
		AllowedTests:       []string{"GPU_BANDWIDTH"},
		RunUnverifiedTests: true,
		Contexts:           []string{"OnDemand"},
	}

	recorder := fixture.request(t, request)

	assert.Equal(t, http.StatusAccepted, recorder.Code)
	assert.Equal(t, corev1.Forge_OnDemandMachineValidation_FullMethodName, fixture.proxiedReq.FullMethod)
	assert.Empty(t, fixture.proxiedReq.EncryptedSecrets)

	var coreRequest corev1.MachineValidationOnDemandRequest
	require.NoError(t, protojson.Unmarshal(fixture.proxiedReq.RequestJSON, &coreRequest))
	assert.Equal(t, fixture.machineID, coreRequest.GetMachineId().GetId())
	assert.Equal(t, corev1.MachineValidationOnDemandRequest_Start, coreRequest.GetAction())
	assert.Equal(t, request.Tags, coreRequest.GetTags())
	assert.Equal(t, request.AllowedTests, coreRequest.GetAllowedTests())
	assert.True(t, coreRequest.GetRunUnverfiedTests())
	assert.Equal(t, request.Contexts, coreRequest.GetContexts())

	var apiResponse model.APIMachineValidationRun
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &apiResponse))
	assert.Equal(t, "validation-1", apiResponse.ValidationID)
	assert.Equal(t, "machine-1", apiResponse.MachineID)
	assert.Equal(t, "Test_machine-1", apiResponse.Name)
	assert.Equal(t, "OnDemand", apiResponse.Context)
}

func TestCreateMachineValidationRunHandlerAcceptsEmptyOptions(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, &corev1.MachineValidationOnDemandResponse{
		ValidationId: &corev1.MachineValidationId{Value: "validation-1"},
	})

	recorder := fixture.request(t, nil)

	assert.Equal(t, http.StatusAccepted, recorder.Code)
	assert.Equal(t, corev1.Forge_OnDemandMachineValidation_FullMethodName, fixture.proxiedReq.FullMethod)

	var apiResponse model.APIMachineValidationRun
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &apiResponse))
	assert.Equal(t, "validation-1", apiResponse.ValidationID)
}

func TestCreateMachineValidationRunHandlerRequiresProviderAdmin(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, nil)
	fixture.user = common.TestBuildUser(t, fixture.dbSession, "viewer-starfleet-id", fixture.org, []string{authz.ProviderViewerRole})

	recorder := fixture.request(t, nil)

	assert.Equal(t, http.StatusForbidden, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestCreateMachineValidationRunHandlerRejectsInvalidOptions(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, nil)

	recorder := fixture.request(t, model.APIMachineValidationRunCreateRequest{Tags: []string{""}})

	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Contains(t, recorder.Body.String(), "Error validating Machine Validation Run creation request data")
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestCreateMachineValidationRunHandlerRejectsUnknownMachine(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, nil)
	fixture.machineID = "missing-machine"

	recorder := fixture.request(t, nil)

	assert.Equal(t, http.StatusNotFound, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestCreateMachineValidationRunHandlerRejectsMissingMachine(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, nil)
	_, err := cdbm.NewMachineDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.MachineUpdateInput{
		MachineID:       fixture.machineID,
		IsMissingOnSite: cutil.GetPtr(true),
	})
	require.NoError(t, err)

	recorder := fixture.request(t, nil)

	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestCreateMachineValidationRunHandlerRejectsUnregisteredSite(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, nil)
	_, err := cdbm.NewSiteDAO(fixture.dbSession).Update(context.Background(), nil, cdbm.SiteUpdateInput{
		SiteID: fixture.siteID,
		Status: cutil.GetPtr(cdbm.SiteStatusError),
	})
	require.NoError(t, err)

	recorder := fixture.request(t, nil)

	assert.Equal(t, http.StatusBadRequest, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}

func TestCreateMachineValidationRunHandlerHidesForeignMachine(t *testing.T) {
	fixture := newMachineValidationOnDemandHandlerFixture(t, nil)
	otherOrg := "other-org"
	otherUser := common.TestBuildUser(t, fixture.dbSession, "other-starfleet-id", otherOrg, []string{authz.ProviderAdminRole})
	otherProvider := common.TestBuildInfrastructureProvider(t, fixture.dbSession, "Other Infrastructure Provider", otherOrg, otherUser)
	otherSite := common.TestBuildSite(t, fixture.dbSession, otherProvider, "Other Site", otherUser)
	otherInstanceType := common.TestBuildInstanceType(t, fixture.dbSession, "other-instance-type", cutil.GetPtr(otherSite.ID), otherSite, nil, otherUser)
	otherMachine := common.TestBuildMachine(t, fixture.dbSession, otherProvider, otherSite, &otherInstanceType.ID, cutil.GetPtr("test-controller-machine-type"), cdbm.MachineStatusReady)
	fixture.machineID = otherMachine.ID

	recorder := fixture.request(t, nil)

	assert.Equal(t, http.StatusNotFound, recorder.Code)
	assert.Empty(t, fixture.proxiedReq.FullMethod)
}
