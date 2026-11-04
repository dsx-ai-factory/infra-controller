// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	temporalEnums "go.temporal.io/api/enums/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func TestDecommissionTrayHandler_Handle(t *testing.T) {
	db := common.TestInitDB(t)
	t.Cleanup(db.Close)
	common.TestSetupSchema(t, db)
	org := "provider-org"
	user := common.TestBuildUser(t, db, uuid.NewString(), org, []string{authz.ProviderAdminRole})
	provider := common.TestBuildInfrastructureProvider(t, db, "Provider", org, user)
	site := common.TestBuildSite(t, db, provider, "Site", user)
	_, err := cdbm.NewSiteDAO(db).Update(context.Background(), nil, cdbm.SiteUpdateInput{SiteID: site.ID, Status: cutil.GetPtr(cdbm.SiteStatusRegistered)})
	require.NoError(t, err)
	unregistered := common.TestBuildSite(t, db, provider, "Unregistered", user)
	otherOrg := "other-provider-org"
	otherUser := common.TestBuildUser(t, db, uuid.NewString(), otherOrg, []string{authz.ProviderAdminRole})
	otherProvider := common.TestBuildInfrastructureProvider(t, db, "Other Provider", otherOrg, otherUser)
	otherSite := common.TestBuildSite(t, db, otherProvider, "Other Site", otherUser)
	body := `{"siteId":"` + site.ID.String() + `","type":"NVSwitch"}`
	cases := []struct {
		name        string
		body        string
		user        *cdbm.User
		id          string
		resultError error
		wantStatus  int
		proxied     bool
		method      string
		expected    proto.Message
	}{
		{name: "accepted switch", wantStatus: http.StatusAccepted, proxied: true},
		{name: "accepted compute", body: `{"siteId":"` + site.ID.String() + `","type":"Compute"}`, wantStatus: http.StatusAccepted, proxied: true,
			method: corev1.Forge_DecommissionManagedHost_FullMethodName, expected: &corev1.DecommissionManagedHostRequest{MachineId: &corev1.MachineId{Id: "component-id"}}},
		{name: "accepted power shelf", body: `{"siteId":"` + site.ID.String() + `","type":"PowerShelf"}`, wantStatus: http.StatusAccepted, proxied: true,
			method: corev1.Forge_DecommissionPowerShelf_FullMethodName, expected: &corev1.DecommissionPowerShelfRequest{PowerShelfId: &corev1.PowerShelfId{Id: "component-id"}}},
		{name: "missing type", body: `{"siteId":"` + site.ID.String() + `"}`, wantStatus: http.StatusBadRequest},
		{name: "unsupported type", body: `{"siteId":"` + site.ID.String() + `","type":"Switch"}`, wantStatus: http.StatusBadRequest},
		{name: "mismatched namespace", id: "ps100-component", wantStatus: http.StatusBadRequest},
		{name: "missing user", wantStatus: http.StatusInternalServerError},
		{name: "provider viewer", user: &cdbm.User{OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{authz.ProviderViewerRole}}}}, wantStatus: http.StatusForbidden},
		{name: "tenant admin", user: &cdbm.User{OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{authz.TenantAdminRole}}}}, wantStatus: http.StatusForbidden},
		{name: "wrong org", user: otherUser, wantStatus: http.StatusForbidden},
		{name: "malformed body", body: "{", wantStatus: http.StatusBadRequest},
		{name: "missing site", body: "{}", wantStatus: http.StatusBadRequest},
		{name: "invalid site", body: `{"siteId":"invalid","type":"NVSwitch"}`, wantStatus: http.StatusBadRequest},
		{name: "unknown site", body: `{"siteId":"` + uuid.NewString() + `","type":"NVSwitch"}`, wantStatus: http.StatusBadRequest},
		{name: "other provider site", body: `{"siteId":"` + otherSite.ID.String() + `","type":"NVSwitch"}`, wantStatus: http.StatusForbidden},
		{name: "unregistered site", body: `{"siteId":"` + unregistered.ID.String() + `","type":"NVSwitch"}`, wantStatus: http.StatusBadRequest},
		{name: "missing component", id: "empty", wantStatus: http.StatusBadRequest},
		{name: "Core rejects assigned rack or unsupported backend", resultError: swe.WrapErr(status.Error(codes.FailedPrecondition, "decommission precondition failed")), wantStatus: http.StatusPreconditionFailed, proxied: true},
		{name: "Core component absent", resultError: swe.WrapErr(status.Error(codes.NotFound, "component not found")), wantStatus: http.StatusNotFound, proxied: true},
		{name: "Core invalid component ID", resultError: swe.WrapErr(status.Error(codes.InvalidArgument, "invalid component ID")), wantStatus: http.StatusBadRequest, proxied: true},
		{name: "older Core lacks RPC", resultError: swe.WrapErr(status.Error(codes.Unimplemented, "method unavailable")), wantStatus: http.StatusNotImplemented, proxied: true},
		{name: "Core unavailable", resultError: swe.WrapErr(status.Error(codes.Unavailable, "Core unavailable")), wantStatus: http.StatusServiceUnavailable, proxied: true},
		{name: "lost result", resultError: temporal.NewTimeoutError(temporalEnums.TIMEOUT_TYPE_START_TO_CLOSE, nil), wantStatus: http.StatusGatewayTimeout, proxied: true},
		{name: "unexpected workflow failure", resultError: errors.New("workflow failed"), wantStatus: http.StatusInternalServerError, proxied: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			pool := sc.NewClientPool(nil)
			client := &tmocks.Client{}
			var captured grpcproxy.Request
			var options tclient.StartWorkflowOptions
			if tc.proxied {
				run := &tmocks.WorkflowRun{}
				run.On("Get", mock.Anything, mock.Anything).Return(tc.resultError).Once()
				client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
					Run(func(args mock.Arguments) {
						options = args.Get(1).(tclient.StartWorkflowOptions)
						captured = args.Get(3).(grpcproxy.Request)
					}).Return(run, nil).Once()
			}
			pool.IDClientMap[site.ID.String()] = client
			requestBody := tc.body
			if requestBody == "" {
				requestBody = body
			}
			request := httptest.NewRequest(http.MethodPost, "/", strings.NewReader(requestBody))
			request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			recorder := httptest.NewRecorder()
			c := echo.New().NewContext(request, recorder)
			c.SetParamNames("orgName", "id")
			id := tc.id
			if id == "" {
				id = "component-id"
			}
			if tc.id == "empty" {
				id = ""
			}
			c.SetParamValues(org, id)
			requestUser := tc.user
			if requestUser == nil && tc.name != "missing user" {
				requestUser = user
			}
			if requestUser != nil {
				c.Set("user", requestUser)
			}
			require.NoError(t, NewDecommissionTrayHandler(db, pool).Handle(c))
			assert.Equal(t, tc.wantStatus, recorder.Code, recorder.Body.String())
			if tc.proxied {
				method := tc.method
				if method == "" {
					method = corev1.Forge_DecommissionSwitch_FullMethodName
				}
				expected := tc.expected
				if expected == nil {
					expected = &corev1.DecommissionSwitchRequest{SwitchId: &corev1.SwitchId{Id: id}}
				}
				assert.Equal(t, method, captured.FullMethod)
				decoded := proto.Clone(expected)
				proto.Reset(decoded)
				require.NoError(t, protojson.Unmarshal(captured.RequestJSON, decoded))
				assert.True(t, proto.Equal(expected, decoded), "got %v", decoded)
				assert.Empty(t, captured.EncryptedSecrets)
				assert.Equal(t, temporalEnums.WORKFLOW_ID_CONFLICT_POLICY_UNSPECIFIED, options.WorkflowIDConflictPolicy)
				assert.Equal(t, grpcproxy.WorkflowExecutionTimeout, options.WorkflowExecutionTimeout)
				assert.True(t, strings.HasPrefix(options.ID, "core-grpc-Decommission"))
			} else {
				assert.Empty(t, client.Calls)
			}
			client.AssertExpectations(t)
			client.AssertNotCalled(t, "TerminateWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything)
			if tc.wantStatus == http.StatusAccepted {
				var response model.APIMessageResponse
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				assert.Equal(t, "Tray decommissioning request was accepted", response.Message)
			} else {
				var response cutil.APIError
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				assert.Nil(t, response.Data)
			}
		})
	}
}
