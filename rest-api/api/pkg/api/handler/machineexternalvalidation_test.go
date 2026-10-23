// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

type machineExternalValidationCaller struct {
	org  string
	user *cdbm.User
}

type machineExternalValidationFixture struct {
	common.TestSetupProviderMachineHandlerFixture
	provider           machineExternalValidationCaller
	privilegedTenant   machineExternalValidationCaller
	unprivilegedTenant machineExternalValidationCaller
	otherSiteID        string
	otherMachineID     string
}

func newMachineExternalValidationFixture(t *testing.T) machineExternalValidationFixture {
	t.Helper()

	ctx := context.Background()
	fixture := common.NewTestSetupProviderMachineHandlerFixture(t, nil)
	providerUser := fixture.User.(*cdbm.User)

	machine, err := cdbm.NewMachineDAO(fixture.DBSession).GetByID(ctx, nil, fixture.MachineID, nil, false)
	require.NoError(t, err)
	provider, err := cdbm.NewInfrastructureProviderDAO(fixture.DBSession).GetByID(ctx, nil, machine.InfrastructureProviderID, nil)
	require.NoError(t, err)

	privilegedOrg := "test-privileged-tenant-org"
	privilegedUser := common.TestBuildUser(t, fixture.DBSession, uuid.NewString(), privilegedOrg, []string{authz.TenantAdminRole})
	privilegedTenant := common.TestBuildTenant(t, fixture.DBSession, "test-privileged-tenant", privilegedOrg, privilegedUser)
	common.TestBuildTenantAccountWithTargetedInstanceCreation(t, fixture.DBSession, provider, &privilegedTenant.ID, privilegedTenant.Org, cdbm.TenantAccountStatusReady, privilegedUser)

	unprivilegedOrg := "test-unprivileged-tenant-org"
	unprivilegedUser := common.TestBuildUser(t, fixture.DBSession, uuid.NewString(), unprivilegedOrg, []string{authz.TenantAdminRole})
	common.TestBuildTenant(t, fixture.DBSession, "test-unprivileged-tenant", unprivilegedOrg, unprivilegedUser)

	otherProvider := common.TestBuildInfrastructureProvider(t, fixture.DBSession, "Other Infrastructure Provider", "test-other-org", providerUser)
	otherSite := common.TestBuildSite(t, fixture.DBSession, otherProvider, "Other Site", providerUser)
	otherMachine := common.TestBuildMachine(t, fixture.DBSession, otherProvider, otherSite, nil, nil, cdbm.MachineStatusReady)

	return machineExternalValidationFixture{
		TestSetupProviderMachineHandlerFixture: fixture,
		provider:                               machineExternalValidationCaller{org: fixture.Org, user: providerUser},
		privilegedTenant:                       machineExternalValidationCaller{org: privilegedOrg, user: privilegedUser},
		unprivilegedTenant:                     machineExternalValidationCaller{org: unprivilegedOrg, user: unprivilegedUser},
		otherSiteID:                            otherSite.ID.String(),
		otherMachineID:                         otherMachine.ID,
	}
}

func requestMachineExternalValidation(t *testing.T, handler echo.HandlerFunc, caller machineExternalValidationCaller, method, target string, body any, params map[string]string) *httptest.ResponseRecorder {
	t.Helper()

	var reqBody []byte
	if body != nil {
		var err error
		reqBody, err = json.Marshal(body)
		require.NoError(t, err)
	}
	req := httptest.NewRequest(method, target, bytes.NewReader(reqBody))
	if body != nil {
		req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	}
	rec := httptest.NewRecorder()
	ec := echo.New().NewContext(req, rec)

	names := []string{"orgName"}
	values := []string{caller.org}
	for name, value := range params {
		names = append(names, name)
		values = append(values, value)
	}
	ec.SetParamNames(names...)
	ec.SetParamValues(values...)
	ec.Set("user", caller.user)

	require.NoError(t, handler(ec))
	if rec.Code == http.StatusNotImplemented {
		assert.Contains(t, rec.Body.String(), machineExternalValidationNotImplementedMessage)
	}
	return rec
}

func TestGetAllMachineExternalValidationHoldHandler_Handle(t *testing.T) {
	fixture := newMachineExternalValidationFixture(t)
	handler := NewGetAllMachineExternalValidationHoldHandler(fixture.DBSession)
	siteQuery := "/?siteId=" + fixture.SiteID

	tests := []struct {
		name           string
		caller         machineExternalValidationCaller
		target         string
		expectedStatus int
	}{
		{
			name:           "Provider Admin of the Site reaches Core",
			caller:         fixture.provider,
			target:         siteQuery,
			expectedStatus: http.StatusNotImplemented,
		},
		{
			name:           "privileged Tenant reaches Core",
			caller:         fixture.privilegedTenant,
			target:         siteQuery,
			expectedStatus: http.StatusNotImplemented,
		},
		{
			name:           "Tenant without targeted Instance creation is rejected",
			caller:         fixture.unprivilegedTenant,
			target:         siteQuery,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "Site of another Provider is rejected",
			caller:         fixture.provider,
			target:         "/?siteId=" + fixture.otherSiteID,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "missing siteId is rejected",
			caller:         fixture.provider,
			target:         "/",
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "unknown Site is rejected",
			caller:         fixture.provider,
			target:         "/?siteId=" + uuid.NewString(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "unknown query parameter is rejected",
			caller:         fixture.provider,
			target:         siteQuery + "&status=Pending",
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "orderBy on a field other than creation time is rejected",
			caller:         fixture.provider,
			target:         siteQuery + "&orderBy=NAME_ASC",
			expectedStatus: http.StatusBadRequest,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			rec := requestMachineExternalValidation(t, handler.Handle, tc.caller, http.MethodGet, tc.target, nil, nil)
			assert.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}

func TestCreateMachineExternalValidationAttemptHandler_Handle(t *testing.T) {
	fixture := newMachineExternalValidationFixture(t)
	handler := NewCreateMachineExternalValidationAttemptHandler(fixture.DBSession)
	validRequest := model.APIMachineExternalValidationAttemptCreateRequest{IdempotencyKey: uuid.NewString()}

	tests := []struct {
		name           string
		caller         machineExternalValidationCaller
		machineID      string
		body           model.APIMachineExternalValidationAttemptCreateRequest
		expectedStatus int
	}{
		{
			name:           "privileged Tenant reaches Core",
			caller:         fixture.privilegedTenant,
			machineID:      fixture.MachineID,
			body:           validRequest,
			expectedStatus: http.StatusNotImplemented,
		},
		{
			name:           "user outside the org is rejected",
			caller:         machineExternalValidationCaller{org: "test-unknown-org", user: fixture.privilegedTenant.user},
			machineID:      fixture.MachineID,
			body:           validRequest,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "Provider Admin is rejected",
			caller:         fixture.provider,
			machineID:      fixture.MachineID,
			body:           validRequest,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "Tenant without targeted Instance creation is rejected",
			caller:         fixture.unprivilegedTenant,
			machineID:      fixture.MachineID,
			body:           validRequest,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "malformed idempotencyKey is rejected",
			caller:         fixture.privilegedTenant,
			machineID:      fixture.MachineID,
			body:           model.APIMachineExternalValidationAttemptCreateRequest{IdempotencyKey: "start-1"},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "unknown Machine is not found",
			caller:         fixture.privilegedTenant,
			machineID:      uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusNotFound,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			rec := requestMachineExternalValidation(t, handler.Handle, tc.caller, http.MethodPost, "/", tc.body, map[string]string{"id": tc.machineID})
			assert.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}

func TestCompleteMachineExternalValidationAttemptHandler_Handle(t *testing.T) {
	fixture := newMachineExternalValidationFixture(t)
	handler := NewCompleteMachineExternalValidationAttemptHandler(fixture.DBSession)
	validRequest := model.APIMachineExternalValidationAttemptCompleteRequest{
		Outcome:    model.MachineExternalValidationOutcomePassed,
		InstanceID: uuid.NewString(),
	}

	tests := []struct {
		name           string
		caller         machineExternalValidationCaller
		attemptID      string
		body           model.APIMachineExternalValidationAttemptCompleteRequest
		expectedStatus int
	}{
		{
			name:           "privileged Tenant reaches Core",
			caller:         fixture.privilegedTenant,
			attemptID:      uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusNotImplemented,
		},
		{
			name:           "malformed attempt ID is rejected",
			caller:         fixture.privilegedTenant,
			attemptID:      "attempt-1",
			body:           validRequest,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:      "unknown outcome is rejected",
			caller:    fixture.privilegedTenant,
			attemptID: uuid.NewString(),
			body: model.APIMachineExternalValidationAttemptCompleteRequest{
				Outcome:    "Skipped",
				InstanceID: validRequest.InstanceID,
			},
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "Provider Admin is rejected",
			caller:         fixture.provider,
			attemptID:      uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusForbidden,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			params := map[string]string{"id": fixture.MachineID, "attemptId": tc.attemptID}
			rec := requestMachineExternalValidation(t, handler.Handle, tc.caller, http.MethodPost, "/", tc.body, params)
			assert.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}

func TestCancelMachineExternalValidationAttemptHandler_Handle(t *testing.T) {
	fixture := newMachineExternalValidationFixture(t)
	handler := NewCancelMachineExternalValidationAttemptHandler(fixture.DBSession)
	validRequest := model.APIMachineExternalValidationAttemptCancelRequest{Details: "Validation run aborted before allocation"}

	tests := []struct {
		name           string
		caller         machineExternalValidationCaller
		attemptID      string
		body           model.APIMachineExternalValidationAttemptCancelRequest
		expectedStatus int
	}{
		{
			name:           "privileged Tenant reaches Core",
			caller:         fixture.privilegedTenant,
			attemptID:      uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusNotImplemented,
		},
		{
			name:           "malformed attempt ID is rejected",
			caller:         fixture.privilegedTenant,
			attemptID:      "attempt-1",
			body:           validRequest,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "missing details are rejected",
			caller:         fixture.privilegedTenant,
			attemptID:      uuid.NewString(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "Provider Admin is rejected",
			caller:         fixture.provider,
			attemptID:      uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusForbidden,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			params := map[string]string{"id": fixture.MachineID, "attemptId": tc.attemptID}
			rec := requestMachineExternalValidation(t, handler.Handle, tc.caller, http.MethodPost, "/", tc.body, params)
			assert.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}

func TestDeleteMachineExternalValidationHoldHandler_Handle(t *testing.T) {
	fixture := newMachineExternalValidationFixture(t)
	handler := NewDeleteMachineExternalValidationHoldHandler(fixture.DBSession)
	validRequest := model.APIMachineExternalValidationHoldDeleteRequest{Reason: "Machine is exempt from external validation for this Site"}

	tests := []struct {
		name           string
		caller         machineExternalValidationCaller
		machineID      string
		holdID         string
		body           model.APIMachineExternalValidationHoldDeleteRequest
		expectedStatus int
	}{
		{
			name:           "Provider Admin reaches Core",
			caller:         fixture.provider,
			machineID:      fixture.MachineID,
			holdID:         uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusNotImplemented,
		},
		{
			name:           "privileged Tenant is rejected",
			caller:         fixture.privilegedTenant,
			machineID:      fixture.MachineID,
			holdID:         uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusForbidden,
		},
		{
			name:           "malformed hold ID is rejected",
			caller:         fixture.provider,
			machineID:      fixture.MachineID,
			holdID:         "hold-1",
			body:           validRequest,
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "missing reason is rejected",
			caller:         fixture.provider,
			machineID:      fixture.MachineID,
			holdID:         uuid.NewString(),
			expectedStatus: http.StatusBadRequest,
		},
		{
			name:           "Machine of another Provider is not found",
			caller:         fixture.provider,
			machineID:      fixture.otherMachineID,
			holdID:         uuid.NewString(),
			body:           validRequest,
			expectedStatus: http.StatusNotFound,
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			params := map[string]string{"id": tc.machineID, "holdId": tc.holdID}
			rec := requestMachineExternalValidation(t, handler.Handle, tc.caller, http.MethodDelete, "/", tc.body, params)
			assert.Equal(t, tc.expectedStatus, rec.Code)
		})
	}
}
