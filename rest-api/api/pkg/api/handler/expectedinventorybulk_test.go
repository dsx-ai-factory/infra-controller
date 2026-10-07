// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
)

func TestExpectedInventoryBulkHandlersRejectIncompleteRequests(t *testing.T) {
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("bulk-user")}
	siteID := uuid.New().String()
	tests := []struct {
		name, path string
		handle     func(echo.Context) error
	}{
		{"machine", "/v2/org/test-org/nico/expected-machine/all", NewReplaceAllExpectedMachinesHandler(nil, nil, nil).Handle},
		{"switch", "/v2/org/test-org/nico/expected-switch/all", NewReplaceAllExpectedSwitchesHandler(nil, nil, nil).Handle},
		{"power shelf", "/v2/org/test-org/nico/expected-power-shelf/all", NewReplaceAllExpectedPowerShelvesHandler(nil, nil, nil).Handle},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			recorder := invokeExpectedInventoryBulkHandler(t, user, "test-org", http.MethodPut, test.path, fmt.Sprintf(`{"siteId":%q}`, siteID), "", test.handle)
			require.Equal(t, http.StatusBadRequest, recorder.Code, recorder.Body.String())
		})
	}

	deleteHandlers := []struct {
		name, path string
		handle     func(echo.Context) error
	}{
		{"machine", "/v2/org/test-org/nico/expected-machine/all", NewDeleteAllExpectedMachinesHandler(nil, nil, nil).Handle},
		{"switch", "/v2/org/test-org/nico/expected-switch/all", NewDeleteAllExpectedSwitchesHandler(nil, nil, nil).Handle},
		{"power shelf", "/v2/org/test-org/nico/expected-power-shelf/all", NewDeleteAllExpectedPowerShelvesHandler(nil, nil, nil).Handle},
	}
	for _, test := range deleteHandlers {
		t.Run("delete "+test.name+" without siteId", func(t *testing.T) {
			recorder := invokeExpectedInventoryBulkHandler(t, user, "test-org", http.MethodDelete, test.path, "", "", test.handle)
			require.Equal(t, http.StatusBadRequest, recorder.Code, recorder.Body.String())
		})
	}
}

func TestReplaceAllExpectedMachinesRollsBackOnCoreFailure(t *testing.T) {
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, model := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, model))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("bulk-user"), OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	dao := cdbm.NewExpectedMachineDAO(dbSession)
	original, err := dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
		ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:10",
		ChassisSerialNumber: "original", CreatedBy: user.ID,
	})
	require.NoError(t, err)

	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "InvokeCoreGRPC", mock.Anything).Return(run, nil).Once()
	run.On("Get", mock.Anything, mock.Anything).Return(errors.New("injected Core failure")).Once()
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	t.Cleanup(func() {
		client.AssertExpectations(t)
		run.AssertExpectations(t)
	})

	body := fmt.Sprintf(`{"siteId":%q,"expectedMachines":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:11","chassisSerialNumber":"replacement"}]}`, site.ID.String(), site.ID.String())
	recorder := invokeExpectedInventoryBulkHandler(t, user, org, http.MethodPut, "/v2/org/test-org/nico/expected-machine/all", body, "", NewReplaceAllExpectedMachinesHandler(dbSession, pool, common.GetTestConfig()).Handle)
	require.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())

	stored, err := dao.Get(ctx, nil, original.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, "original", stored.ChassisSerialNumber)
	_, count, err := dao.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 1, count)
}

func TestReplaceAllExpectedMachinesFindsRequestedSKUAmongLargerSiteInventory(t *testing.T) {
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, model := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, model))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("bulk-user"), OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	for _, sku := range []cdbm.SKU{
		{ID: "unrequested-sku", SiteID: site.ID, Created: time.Unix(1, 0)},
		{ID: "requested-sku", SiteID: site.ID, Created: time.Unix(2, 0)},
	} {
		_, err = dbSession.DB.NewInsert().Model(&sku).Exec(ctx)
		require.NoError(t, err)
	}

	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "InvokeCoreGRPC", mock.Anything).Return(run, nil).Once()
	run.On("Get", mock.Anything, mock.Anything).Return(nil).Once()
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	t.Cleanup(func() {
		client.AssertExpectations(t)
		run.AssertExpectations(t)
	})

	body := fmt.Sprintf(`{"siteId":%q,"expectedMachines":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:01","chassisSerialNumber":"machine-1","skuId":"requested-sku"}]}`, site.ID.String(), site.ID.String())
	recorder := invokeExpectedInventoryBulkHandler(t, user, org, http.MethodPut, "/v2/org/test-org/nico/expected-machine/all", body, "", NewReplaceAllExpectedMachinesHandler(dbSession, pool, common.GetTestConfig()).Handle)
	require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
}

func TestExpectedInventoryBulkHandlers(t *testing.T) {
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, model := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil), (*cdbm.ExpectedSwitch)(nil), (*cdbm.ExpectedPowerShelf)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, model))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("bulk-user"),
		OrgData: cdbm.OrgData{
			org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}},
		},
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)

	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	var captured []grpcproxy.Request
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "InvokeCoreGRPC", mock.Anything).
		Run(func(args mock.Arguments) { captured = append(captured, args.Get(3).(grpcproxy.Request)) }).
		Return(run, nil).Times(6)
	run.On("Get", mock.Anything, mock.Anything).Return(nil).Times(6)
	t.Cleanup(func() {
		client.AssertExpectations(t)
		run.AssertExpectations(t)
	})
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	cfg := common.GetTestConfig()

	replacements := []struct {
		name    string
		path    string
		body    string
		handle  func(echo.Context) error
		method  string
		secrets []string
	}{
		{
			name: "machine", path: "/v2/org/test-org/nico/expected-machine/all",
			body:    fmt.Sprintf(`{"siteId":%q,"expectedMachines":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:01","chassisSerialNumber":"machine-1","defaultBmcUsername":"machine-user","defaultBmcPassword":"machine-pass","interfaces":[{"macAddress":"02:00:00:00:00:09","nicType":"CX9","fixedIp":"192.0.2.9"}]},{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:09","chassisSerialNumber":"machine-empty","interfaces":[]}]}`, site.ID.String(), site.ID.String(), site.ID.String()),
			handle:  NewReplaceAllExpectedMachinesHandler(dbSession, pool, cfg).Handle,
			method:  corev1.Forge_ReplaceAllExpectedMachines_FullMethodName,
			secrets: []string{"machine-user", "machine-pass"},
		},
		{
			name: "switch", path: "/v2/org/test-org/nico/expected-switch/all",
			body:    fmt.Sprintf(`{"siteId":%q,"expectedSwitches":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:02","switchSerialNumber":"switch-1","defaultBmcUsername":"switch-user","defaultBmcPassword":"switch-pass","nvOsUsername":"nvos-user","nvOsPassword":"nvos-pass"}]}`, site.ID.String(), site.ID.String()),
			handle:  NewReplaceAllExpectedSwitchesHandler(dbSession, pool, cfg).Handle,
			method:  corev1.Forge_ReplaceAllExpectedSwitches_FullMethodName,
			secrets: []string{"switch-user", "switch-pass", "nvos-user", "nvos-pass"},
		},
		{
			name: "power shelf", path: "/v2/org/test-org/nico/expected-power-shelf/all",
			body:    fmt.Sprintf(`{"siteId":%q,"expectedPowerShelves":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:03","shelfSerialNumber":"shelf-1","defaultBmcUsername":"shelf-user","defaultBmcPassword":"shelf-pass"}]}`, site.ID.String(), site.ID.String()),
			handle:  NewReplaceAllExpectedPowerShelvesHandler(dbSession, pool, cfg).Handle,
			method:  corev1.Forge_ReplaceAllExpectedPowerShelves_FullMethodName,
			secrets: []string{"shelf-user", "shelf-pass"},
		},
	}

	for _, replacement := range replacements {
		t.Run("replace all "+replacement.name, func(t *testing.T) {
			recorder := invokeExpectedInventoryBulkHandler(t, user, org, http.MethodPut, replacement.path, replacement.body, "", replacement.handle)
			require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
			request := captured[len(captured)-1]
			require.Equal(t, replacement.method, request.FullMethod)
			testExpectedComponentPatchSecrets(t, request, replacement.secrets...)
		})
	}

	var machineList corev1.ExpectedMachineList
	testDecodeExpectedComponentPatch(t, captured[0], site.ID.String(), &machineList)
	require.Len(t, machineList.ExpectedMachines, 2)
	for _, machine := range machineList.ExpectedMachines {
		require.True(t, machine.ReplaceHostNics, "replace-all must explicitly remove interfaces omitted from each replacement")
	}
	require.Empty(t, machineList.ExpectedMachines[1].HostNics)
	require.Equal(t, "machine-pass", machineList.ExpectedMachines[0].BmcPassword)
	require.Len(t, machineList.ExpectedMachines[0].HostNics, 1)
	require.Equal(t, "CX9", machineList.ExpectedMachines[0].HostNics[0].GetNicType())
	require.Equal(t, "192.0.2.9", machineList.ExpectedMachines[0].HostNics[0].GetFixedIp())
	var switchList corev1.ExpectedSwitchList
	testDecodeExpectedComponentPatch(t, captured[1], site.ID.String(), &switchList)
	require.Len(t, switchList.ExpectedSwitches, 1)
	require.Equal(t, "nvos-pass", switchList.ExpectedSwitches[0].GetNvosPassword())
	var shelfList corev1.ExpectedPowerShelfList
	testDecodeExpectedComponentPatch(t, captured[2], site.ID.String(), &shelfList)
	require.Len(t, shelfList.ExpectedPowerShelves, 1)
	require.Equal(t, "shelf-pass", shelfList.ExpectedPowerShelves[0].BmcPassword)

	deletions := []struct {
		name, path, method string
		handle             func(echo.Context) error
	}{
		{"machine", "/v2/org/test-org/nico/expected-machine/all", corev1.Forge_DeleteAllExpectedMachines_FullMethodName, NewDeleteAllExpectedMachinesHandler(dbSession, pool, cfg).Handle},
		{"switch", "/v2/org/test-org/nico/expected-switch/all", corev1.Forge_DeleteAllExpectedSwitches_FullMethodName, NewDeleteAllExpectedSwitchesHandler(dbSession, pool, cfg).Handle},
		{"power shelf", "/v2/org/test-org/nico/expected-power-shelf/all", corev1.Forge_DeleteAllExpectedPowerShelves_FullMethodName, NewDeleteAllExpectedPowerShelvesHandler(dbSession, pool, cfg).Handle},
	}
	for _, deletion := range deletions {
		t.Run("delete all "+deletion.name, func(t *testing.T) {
			recorder := invokeExpectedInventoryBulkHandler(t, user, org, http.MethodDelete, deletion.path, "", site.ID.String(), deletion.handle)
			require.Equal(t, http.StatusNoContent, recorder.Code, recorder.Body.String())
			request := captured[len(captured)-1]
			require.Equal(t, deletion.method, request.FullMethod)
			require.Empty(t, request.EncryptedSecrets)
		})
	}

	_, count, err := cdbm.NewExpectedMachineDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Zero(t, count)
	_, count, err = cdbm.NewExpectedSwitchDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Zero(t, count)
	_, count, err = cdbm.NewExpectedPowerShelfDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedPowerShelfFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Zero(t, count)
}

func invokeExpectedInventoryBulkHandler(t *testing.T, user *cdbm.User, org, method, path, body, siteID string, handle func(echo.Context) error) *httptest.ResponseRecorder {
	t.Helper()
	request := httptest.NewRequest(method, path, strings.NewReader(body))
	request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	if siteID != "" {
		query := request.URL.Query()
		query.Set("siteId", siteID)
		request.URL.RawQuery = query.Encode()
	}
	recorder := httptest.NewRecorder()
	echoContext := echo.New().NewContext(request, recorder)
	echoContext.Set("user", user)
	echoContext.SetParamNames("orgName")
	echoContext.SetParamValues(org)
	require.NoError(t, handle(echoContext))
	return recorder
}
