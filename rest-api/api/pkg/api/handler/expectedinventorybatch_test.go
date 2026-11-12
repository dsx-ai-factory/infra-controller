// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	enumspb "go.temporal.io/api/enums/v1"
	workflowpb "go.temporal.io/api/workflow/v1"
	workflowservice "go.temporal.io/api/workflowservice/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
)

// Keep the deployed key independent of its production helper in lock tests.
const expectedInventoryMutationLockPrefix = "expected-inventory-mutation:"

func TestExpectedInventoryBatchHandlersRejectIncompleteRequests(t *testing.T) {
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("batch-user")}
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
			recorder := invokeExpectedInventoryBatchHandler(t, user, "test-org", http.MethodPut, test.path, fmt.Sprintf(`{"siteId":%q}`, siteID), "", test.handle)
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
			recorder := invokeExpectedInventoryBatchHandler(t, user, "test-org", http.MethodDelete, test.path, "", "", test.handle)
			require.Equal(t, http.StatusBadRequest, recorder.Code, recorder.Body.String())
		})
	}
}

type expectedMachineMutationLockCase struct {
	mutation string
	shared   bool
}

func testExpectedMachineMutationRejectsInventoryLock(t *testing.T, test expectedMachineMutationLockCase) {
	t.Helper()
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, dbModel := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil), (*cdbm.PendingExpectedMachine)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, dbModel))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("batch-user"), OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	original, err := cdbm.NewExpectedMachineDAO(dbSession).Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
		ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:10",
		ChassisSerialNumber: "original", Name: cutil.GetPtr("original"), CreatedBy: user.ID,
	})
	require.NoError(t, err)

	lockTx, err := cdb.BeginTx(ctx, dbSession, nil)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, lockTx.Rollback()) })
	lockID := cdb.GetAdvisoryLockIDFromString(expectedInventoryMutationLockPrefix + site.ID.String())
	if test.shared {
		require.NoError(t, lockTx.TryAcquireSharedAdvisoryLock(ctx, lockID, nil))
	} else {
		require.NoError(t, lockTx.TryAcquireAdvisoryLock(ctx, lockID, nil))
	}
	client := &tmocks.Client{}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client

	var method, path, body, id string
	var handle func(echo.Context) error
	switch test.mutation {
	case "single create":
		method = http.MethodPost
		path = "/v2/org/test-org/nico/expected-machine"
		body = fmt.Sprintf(`{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:11","chassisSerialNumber":"created"}`, site.ID.String())
		handle = NewCreateExpectedMachineHandler(dbSession, pool, common.GetTestConfig()).Handle
	case "single update":
		method = http.MethodPatch
		path = "/v2/org/test-org/nico/expected-machine/" + original.ID.String()
		id = original.ID.String()
		body = `{"name":"updated"}`
		handle = NewUpdateExpectedMachineHandler(dbSession, pool, common.GetTestConfig()).Handle
	case "single delete":
		method = http.MethodDelete
		path = "/v2/org/test-org/nico/expected-machine/" + original.ID.String()
		id = original.ID.String()
		handle = NewDeleteExpectedMachineHandler(dbSession, pool, common.GetTestConfig()).Handle
	case "batch create":
		method = http.MethodPost
		path = "/v2/org/test-org/nico/expected-machine/batch"
		body = fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:12","chassisSerialNumber":"batch-created"}]`, site.ID.String())
		handle = NewCreateExpectedMachinesHandler(dbSession, pool, common.GetTestConfig()).Handle
	case "batch update":
		method = http.MethodPatch
		path = "/v2/org/test-org/nico/expected-machine/batch"
		body = fmt.Sprintf(`[{"id":%q,"name":"batch-updated"}]`, original.ID.String())
		if test.shared {
			body = fmt.Sprintf(`[{"id":%q,"chassisSerialNumber":"batch-updated","name":"batch-updated"}]`, original.ID.String())
		}
		handle = NewUpdateExpectedMachinesHandler(dbSession, pool, common.GetTestConfig()).Handle
	default:
		t.Fatalf("unknown Expected Machine mutation %q", test.mutation)
	}

	recorder := invokeExpectedInventoryMutationHandler(t, user, org, method, path, body, id, handle)
	require.Equal(t, http.StatusConflict, recorder.Code, recorder.Body.String())
	var response struct {
		Message string `json:"message"`
	}
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
	require.Equal(t, "Expected Inventory for this Site is being updated; retry the request", response.Message)
	require.Empty(t, client.Calls, "lock contention must not reach Core")

	stored, err := cdbm.NewExpectedMachineDAO(dbSession).Get(ctx, nil, original.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, original, stored)
	_, count, err := cdbm.NewExpectedMachineDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 1, count)
}

// expectedMachineQueryHook runs one selected callback for the first matching
// query. Set either `beforeQuery` or `afterQuery` to choose the observation point.
type expectedMachineQueryHook struct {
	queryPrefix string
	beforeQuery func()
	afterQuery  func(*bun.QueryEvent)
	armed       bool
	fired       bool
}

func (hook *expectedMachineQueryHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if hook.beforeQuery != nil && hook.armed && !hook.fired && strings.HasPrefix(event.Query, hook.queryPrefix) {
		hook.armed = false
		hook.fired = true
		hook.beforeQuery()
	}
	return ctx
}

func (hook *expectedMachineQueryHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if hook.afterQuery != nil && hook.armed && !hook.fired && strings.HasPrefix(event.Query, hook.queryPrefix) {
		hook.armed = false
		hook.fired = true
		hook.afterQuery(event)
	}
}

type expectedMachineSerialAdmissionCase struct {
	mutation               string
	commitAfterLockFailure bool
}

func testExpectedMachineBatchSerialAdmission(t *testing.T, test expectedMachineSerialAdmissionCase) {
	t.Helper()
	if test.commitAfterLockFailure {
		t.Setenv("PGOPTIONS", `-c default_transaction_isolation=repeatable\ read`)
	}
	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	if test.commitAfterLockFailure {
		var isolation string
		err := dbSession.DB.NewRaw("SHOW default_transaction_isolation").Scan(ctx, &isolation)
		require.NoError(t, err)
		require.Equal(t, "repeatable read", isolation)
	}
	const org = "test-org"
	_, site := testExpectedMachineSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID: uuid.New(), StarfleetID: cutil.GetPtr("serial-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}},
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	dao := cdbm.NewExpectedMachineDAO(dbSession)
	original, err := dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
		ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:10",
		ChassisSerialNumber: "original", Name: cutil.GetPtr("original"), CreatedBy: user.ID,
	})
	require.NoError(t, err)

	// Keep this probe independent of the production key helper.
	lockID := cdb.GetAdvisoryLockIDFromString(expectedInventoryMutationLockPrefix + site.ID.String())
	probeSharedLock := func(wantAcquired bool) {
		t.Helper()
		probeTx, err := cdb.BeginTx(ctx, dbSession, nil)
		require.NoError(t, err)
		defer func() { require.NoError(t, probeTx.Rollback()) }()
		var acquired bool
		err = probeTx.GetBunTx().NewSelect().ColumnExpr("pg_try_advisory_xact_lock_shared(?)", lockID).Scan(ctx, &acquired)
		require.NoError(t, err)
		require.Equal(t, wantAcquired, acquired, "unexpected shared access to the Expected Inventory lock")
	}
	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	var dispatched *corev1.BatchExpectedMachineOperationRequest
	if !test.commitAfterLockFailure {
		workflowName := "InvokeCoreGRPC"
		if test.mutation == "batch create" {
			workflowName = "CreateExpectedMachines"
		}
		client.On("ExecuteWorkflow", mock.Anything, mock.Anything, workflowName, mock.Anything).
			Run(func(args mock.Arguments) {
				probeSharedLock(test.mutation == "batch create")
				if test.mutation == "batch create" {
					dispatched = args.Get(3).(*corev1.BatchExpectedMachineOperationRequest)
				}
			}).Return(run, nil).Once()
		var resultCall *mock.Call
		if test.mutation == "batch create" {
			run.On("GetRunID").Return("serial-admission-run").Once()
			resultCall = run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true})
		} else {
			resultCall = run.On("Get", mock.Anything, mock.Anything)
		}
		resultCall.Run(func(args mock.Arguments) {
			result, ok := args.Get(1).(*corev1.BatchExpectedMachineOperationResponse)
			if ok {
				result.Results = []*corev1.ExpectedMachineOperationResult{{Id: dispatched.GetExpectedMachines().GetExpectedMachines()[0].GetId(), Success: true}}
			}
		}).Return(nil).Once()
	}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	cfg := common.GetTestConfig()
	if test.mutation == "batch create" {
		cfg = expectedMachineBatchRecoveryConfig(t)
	}
	path := "/v2/org/test-org/nico/expected-machine/batch"
	var method, body, winnerBody, submittedSerial string
	var handle func(echo.Context) error
	winnerStatus := http.StatusOK
	winnerID := uuid.New()
	switch test.mutation {
	case "batch create":
		method = http.MethodPost
		submittedSerial = "Committed-Serial"
		body = fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:12","chassisSerialNumber":%q}]`, site.ID.String(), submittedSerial)
		winnerBody = fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:11","chassisSerialNumber":"Committed-Serial","name":"winner"}]`, site.ID.String())
		handle = NewCreateExpectedMachinesHandler(dbSession, pool, cfg).Handle
		winnerStatus = http.StatusCreated
	case "batch update":
		_, err = dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID: winnerID, SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:11",
			ChassisSerialNumber: "winner-original", CreatedBy: user.ID,
		})
		require.NoError(t, err)
		method = http.MethodPatch
		submittedSerial = "committed-serial"
		body = fmt.Sprintf(`[{"id":%q,"chassisSerialNumber":%q,"name":"loser"}]`, original.ID.String(), submittedSerial)
		winnerBody = fmt.Sprintf(`[{"id":%q,"chassisSerialNumber":"Committed-Serial","name":"winner"}]`, winnerID.String())
		handle = NewUpdateExpectedMachinesHandler(dbSession, pool, cfg).Handle
	default:
		t.Fatalf("unknown Expected Machine batch mutation %q", test.mutation)
	}

	var commitHook *expectedMachineQueryHook
	var writerCommitError error
	var admissionHook *expectedMachineQueryHook
	if test.commitAfterLockFailure {
		writerTx, err := cdb.BeginTx(ctx, dbSession, nil)
		require.NoError(t, err)
		defer func() {
			rollbackErr := writerTx.Rollback()
			if rollbackErr != nil {
				require.ErrorIs(t, rollbackErr, sql.ErrTxDone)
			}
		}()
		var sharedAcquired bool
		err = writerTx.GetBunTx().NewSelect().ColumnExpr("pg_try_advisory_xact_lock_shared(?)", lockID).Scan(ctx, &sharedAcquired)
		require.NoError(t, err)
		require.True(t, sharedAcquired)
		if test.mutation == "batch create" {
			_, err = dao.Create(ctx, writerTx, cdbm.ExpectedMachineCreateInput{
				ExpectedMachineID: winnerID, SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:11",
				ChassisSerialNumber: "Committed-Serial", Name: cutil.GetPtr("winner"), CreatedBy: user.ID,
			})
		} else {
			_, err = dao.Update(ctx, writerTx, cdbm.ExpectedMachineUpdateInput{
				ExpectedMachineID: winnerID, ChassisSerialNumber: cutil.GetPtr("Committed-Serial"),
				Name: cutil.GetPtr("winner"),
			})
		}
		require.NoError(t, err)
		// The shared lock makes the first exclusive SELECT return false.
		// Commit afterward: inheriting Repeatable Read would keep that snapshot
		// and hide the serial even after the exclusive lock succeeds.
		admissionHook = &expectedMachineQueryHook{
			queryPrefix: fmt.Sprintf("SELECT pg_try_advisory_xact_lock(%d)", lockID), armed: true,
			afterQuery: func(event *bun.QueryEvent) {
				writerCommitError = event.Err
				if writerCommitError == nil {
					writerCommitError = writerTx.Commit()
				}
			},
		}
	} else {
		commitHook = &expectedMachineQueryHook{
			queryPrefix: "COMMIT", armed: true,
			beforeQuery: func() { probeSharedLock(false) },
		}
		dbSession.DB.AddQueryHook(commitHook)
		// Commit a public request at the last point before the outer transaction.
		// A lookup before BEGIN would miss this serial and admit the losing request.
		admissionHook = &expectedMachineQueryHook{
			queryPrefix: "BEGIN", armed: true,
			beforeQuery: func() {
				winner := invokeExpectedInventoryMutationHandler(t, user, org, method, path, winnerBody, "", handle)
				require.Equal(t, winnerStatus, winner.Code, winner.Body.String())
				var response []model.APIExpectedMachine
				require.NoError(t, json.Unmarshal(winner.Body.Bytes(), &response))
				require.Len(t, response, 1)
				winnerID = response[0].ID
			},
		}
	}
	dbSession.DB.AddQueryHook(admissionHook)
	loser := invokeExpectedInventoryMutationHandler(t, user, org, method, path, body, "", handle)
	require.True(t, admissionHook.fired)
	require.NoError(t, writerCommitError)
	if commitHook != nil {
		require.True(t, commitHook.fired)
	}
	require.Equal(t, http.StatusBadRequest, loser.Code, loser.Body.String())
	var response struct {
		Message string                       `json:"message"`
		Data    map[string]map[string]string `json:"data"`
	}
	require.NoError(t, json.Unmarshal(loser.Body.Bytes(), &response))
	require.Equal(t, "Failed to validate Expected Machine update data", response.Message)
	require.Equal(t, map[string]map[string]string{
		"0": {"chassisSerialNumber": "Expected Machine with Chassis Serial Number: " + submittedSerial + " already exists"},
	}, response.Data)
	stored, err := dao.Get(ctx, nil, original.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, original, stored, "rejected admission must preserve the target")
	winner, err := dao.Get(ctx, nil, winnerID, nil, false)
	require.NoError(t, err)
	require.Equal(t, "Committed-Serial", winner.ChassisSerialNumber)
	require.Equal(t, cutil.GetPtr("winner"), winner.Name)
	machines, count, err := dao.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 2, count)
	for _, machine := range machines {
		require.NotEqual(t, "00:11:22:33:44:12", machine.BmcMacAddress, "rejected creation must leave no row")
	}
	client.AssertExpectations(t)
	run.AssertExpectations(t)
	if test.commitAfterLockFailure {
		require.Empty(t, client.Calls, "a batch rejected after a lock retry must not reach Core")
	} else {
		client.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
	}
	probeSharedLock(true)
}

func testExpectedMachineBatchCreateWorkflowDeadline(t *testing.T) {
	t.Helper()
	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	const org = "test-org"
	_, site := testExpectedMachineSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID: uuid.New(), StarfleetID: cutil.GetPtr("workflow-deadline-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}},
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	run.On("GetRunID").Return("deadline-run")
	var startCtx, resultCtx context.Context
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedMachines", mock.Anything).
		Run(func(args mock.Arguments) { startCtx = args.Get(0).(context.Context) }).Return(run, nil).Once()
	run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).
		Run(func(args mock.Arguments) { resultCtx = args.Get(0).(context.Context) }).
		Return(errors.New("injected workflow result error")).Once()
	client.On("DescribeWorkflowExecution", mock.Anything, mock.Anything, "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING}}, nil).Once()
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:11","chassisSerialNumber":"deadline-serial"}]`, site.ID.String())
	started := time.Now()
	recorder := invokeExpectedInventoryMutationHandler(t, user, org, http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", NewCreateExpectedMachinesHandler(dbSession, pool, expectedMachineBatchRecoveryConfig(t)).Handle)
	finished := time.Now()
	require.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())
	require.Contains(t, recorder.Body.String(), "Failed to verify batch Expected Machine creation workflow results")
	require.NotNil(t, startCtx)
	deadline, hasDeadline := startCtx.Deadline()
	require.True(t, hasDeadline, "workflow start and result wait must share a bounded context")
	require.Same(t, startCtx, resultCtx)
	require.False(t, deadline.Before(started.Add(cutil.WorkflowContextTimeout)))
	require.False(t, deadline.After(finished.Add(cutil.WorkflowContextTimeout)))
	require.ErrorIs(t, startCtx.Err(), context.Canceled, "the handler must release the workflow context after a result error")
	count, err := dbSession.DB.NewSelect().Model((*cdbm.ExpectedMachine)(nil)).Count(ctx)
	require.NoError(t, err)
	require.Zero(t, count, "an unconfirmed batch must remain private")
	pending, err := cdbm.NewPendingExpectedMachineBatchDAO(dbSession).GetAll(ctx, nil, site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 1)
	require.Equal(t, "deadline-serial", pending[0].Inputs[0].ChassisSerialNumber)
	probeTx, err := cdb.BeginTx(ctx, dbSession, nil)
	require.NoError(t, err)
	defer func() { require.NoError(t, probeTx.Rollback()) }()
	var acquired bool
	lockID := cdb.GetAdvisoryLockIDFromString(expectedInventoryMutationLockPrefix + site.ID.String())
	err = probeTx.GetBunTx().NewSelect().ColumnExpr("pg_try_advisory_xact_lock(?)", lockID).Scan(ctx, &acquired)
	require.NoError(t, err)
	require.True(t, acquired, "a workflow result error must release the Expected Inventory lock")
	client.AssertExpectations(t)
	run.AssertExpectations(t)
}

type expectedMachineStoredIdentityCase struct {
	bmcMacAddress       string
	chassisSerialNumber string
	status              int
}

func testExpectedMachineBatchCreateStoredIdentity(t *testing.T, test expectedMachineStoredIdentityCase) {
	t.Helper()
	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	const org = "test-org"
	_, site := testExpectedMachineSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID: uuid.New(), StarfleetID: cutil.GetPtr("stored-identity-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}},
	}
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
	var dispatched *corev1.BatchExpectedMachineOperationRequest
	if test.status == http.StatusCreated {
		run.On("GetRunID").Return("stored-identity-run").Once()
		client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedMachines", mock.Anything).
			Run(func(args mock.Arguments) { dispatched = args.Get(3).(*corev1.BatchExpectedMachineOperationRequest) }).Return(run, nil).Once()
		run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
			result := args.Get(1).(*corev1.BatchExpectedMachineOperationResponse)
			result.Results = []*corev1.ExpectedMachineOperationResult{{Id: dispatched.ExpectedMachines.ExpectedMachines[0].Id, Success: true}}
		}).Return(nil).Once()
	}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":%q,"chassisSerialNumber":%q}]`, site.ID.String(), test.bmcMacAddress, test.chassisSerialNumber)
	recorder := invokeExpectedInventoryMutationHandler(t, user, org, http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", NewCreateExpectedMachinesHandler(dbSession, pool, expectedMachineBatchRecoveryConfig(t)).Handle)
	require.Equal(t, test.status, recorder.Code, recorder.Body.String())
	if test.status == http.StatusCreated {
		var response []model.APIExpectedMachine
		require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
		require.Len(t, response, 1)
		require.Equal(t, test.chassisSerialNumber, response[0].ChassisSerialNumber)
		stored, err := dao.Get(ctx, nil, response[0].ID, nil, false)
		require.NoError(t, err)
		require.Equal(t, test.chassisSerialNumber, stored.ChassisSerialNumber)
		require.NotNil(t, dispatched)
		require.Len(t, dispatched.ExpectedMachines.ExpectedMachines, 1)
		machine := dispatched.ExpectedMachines.ExpectedMachines[0]
		require.Equal(t, stored.ID.String(), machine.GetId().GetValue())
		require.Equal(t, test.bmcMacAddress, machine.BmcMacAddress)
		require.Equal(t, test.chassisSerialNumber, machine.ChassisSerialNumber)
	} else {
		var response struct {
			Message string                       `json:"message"`
			Data    map[string]map[string]string `json:"data"`
		}
		require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
		require.Equal(t, "Failed to validate Expected Machine update data", response.Message)
		require.Equal(t, map[string]map[string]string{
			"0": {"bmcMacAddress": "Expected Machine with BMC MAC Address: " + test.bmcMacAddress + " already exist"},
		}, response.Data)
		require.Empty(t, client.Calls, "a stored MAC duplicate must not reach Core")
		count, err := dbSession.DB.NewSelect().Model((*cdbm.ExpectedMachine)(nil)).Count(ctx)
		require.NoError(t, err)
		require.Equal(t, 1, count)
	}
	stored, err := dao.Get(ctx, nil, original.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, original, stored)
	client.AssertExpectations(t)
	run.AssertExpectations(t)
}

func testExpectedMachineBatchLegacySerial(t *testing.T) {
	t.Helper()
	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	const org = "test-org"
	_, site := testExpectedMachineSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID: uuid.New(), StarfleetID: cutil.GetPtr("legacy-serial-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}},
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	dao := cdbm.NewExpectedMachineDAO(dbSession)
	ids := []uuid.UUID{uuid.New(), uuid.New()}
	for i, id := range ids {
		_, err = dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
			ExpectedMachineID: id, SiteID: site.ID, BmcMacAddress: fmt.Sprintf("00:11:22:33:44:%02d", i+10),
			ChassisSerialNumber: "legacy-duplicate", Name: cutil.GetPtr("original"), CreatedBy: user.ID,
		})
		require.NoError(t, err)
	}
	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "InvokeCoreGRPC", mock.Anything).Return(run, nil).Once()
	run.On("Get", mock.Anything, mock.Anything).Return(nil).Once()
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	handler := NewUpdateExpectedMachinesHandler(dbSession, pool, common.GetTestConfig())
	sharedTx, err := cdb.BeginTx(ctx, dbSession, nil)
	require.NoError(t, err)
	defer func() {
		rollbackErr := sharedTx.Rollback()
		if rollbackErr != nil {
			require.ErrorIs(t, rollbackErr, sql.ErrTxDone)
		}
	}()
	lockID := cdb.GetAdvisoryLockIDFromString(expectedInventoryMutationLockPrefix + site.ID.String())
	require.NoError(t, sharedTx.TryAcquireSharedAdvisoryLock(ctx, lockID, nil))
	path := "/v2/org/test-org/nico/expected-machine/batch"

	// Omitted and null serials allow metadata edits to historical duplicates.
	metadataBody := fmt.Sprintf(`[{"id":%q,"name":"updated"},{"id":%q,"chassisSerialNumber":null,"name":"updated"}]`, ids[0].String(), ids[1].String())
	metadata := invokeExpectedInventoryMutationHandler(t, user, org, http.MethodPatch, path, metadataBody, "", handler.Handle)
	require.Equal(t, http.StatusOK, metadata.Code, metadata.Body.String())
	afterMetadata := make([]*cdbm.ExpectedMachine, 0, len(ids))
	for _, id := range ids {
		stored, err := dao.Get(ctx, nil, id, nil, false)
		require.NoError(t, err)
		require.Equal(t, "legacy-duplicate", stored.ChassisSerialNumber)
		require.Equal(t, cutil.GetPtr("updated"), stored.Name)
		afterMetadata = append(afterMetadata, stored)
	}
	require.NoError(t, sharedTx.Rollback())

	// Explicitly reasserting the same duplicate still requires serial admission.
	serialBody := fmt.Sprintf(`[{"id":%q,"chassisSerialNumber":"legacy-duplicate","name":"rejected"}]`, ids[0].String())
	rejected := invokeExpectedInventoryMutationHandler(t, user, org, http.MethodPatch, path, serialBody, "", handler.Handle)
	require.Equal(t, http.StatusBadRequest, rejected.Code, rejected.Body.String())
	var response struct {
		Message string                       `json:"message"`
		Data    map[string]map[string]string `json:"data"`
	}
	require.NoError(t, json.Unmarshal(rejected.Body.Bytes(), &response))
	require.Equal(t, "Failed to validate Expected Machine update data", response.Message)
	require.Equal(t, map[string]map[string]string{
		"0": {"chassisSerialNumber": "Expected Machine with Chassis Serial Number: legacy-duplicate already exists"},
	}, response.Data)
	for i, id := range ids {
		stored, err := dao.Get(ctx, nil, id, nil, false)
		require.NoError(t, err)
		require.Equal(t, afterMetadata[i], stored)
	}

	client.AssertExpectations(t)
	run.AssertExpectations(t)
	client.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
}

func testExpectedMachineRejectsRecreatedSKU(t *testing.T, mutation string) {
	t.Helper()
	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	const org = "test-org"
	provider, site := testExpectedMachineSetupTestData(t, dbSession, org)
	otherSite := &cdbm.Site{
		ID: uuid.New(), Name: "other-site", Org: org,
		InfrastructureProviderID: provider.ID, Status: cdbm.SiteStatusRegistered,
	}
	_, err := dbSession.DB.NewInsert().Model(otherSite).Exec(ctx)
	require.NoError(t, err)
	user := &cdbm.User{
		ID: uuid.New(), StarfleetID: cutil.GetPtr("attachment-user"),
		OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}},
	}
	_, err = dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	dao := cdbm.NewExpectedMachineDAO(dbSession)
	original, err := dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
		ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:10",
		ChassisSerialNumber: "original", Name: cutil.GetPtr("original"),
		SkuID: cutil.GetPtr("test-sku-uuid-1"), CreatedBy: user.ID,
	})
	require.NoError(t, err)

	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	run.On("GetID").Return("attachment-workflow")
	run.On("Get", mock.Anything, mock.Anything).Return(nil)
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(run, nil)
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	cfg := common.GetTestConfig()

	const skuID = "test-sku-uuid-2"
	createBody := fmt.Sprintf(`{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:11","chassisSerialNumber":"created","skuId":%q}`, site.ID.String(), skuID)
	var method, path, body, id string
	var handle func(echo.Context) error
	expectedStatus := http.StatusBadRequest
	expectedMessage := "the SkuID specified for Expected Machine does not exist in DB: " + skuID
	switch mutation {
	case "single create":
		method, path, body = http.MethodPost, "/v2/org/test-org/nico/expected-machine", createBody
		handle = NewCreateExpectedMachineHandler(dbSession, pool, cfg).Handle
		expectedStatus, expectedMessage = http.StatusUnprocessableEntity, "SKU ID specified in request does not exist"
	case "single update":
		id = original.ID.String()
		method, path = http.MethodPatch, "/v2/org/test-org/nico/expected-machine/"+id
		body = fmt.Sprintf(`{"skuId":%q,"name":"updated"}`, skuID)
		handle = NewUpdateExpectedMachineHandler(dbSession, pool, cfg).Handle
		expectedStatus, expectedMessage = http.StatusUnprocessableEntity, "SKU ID specified in request does not exist"
	case "batch create":
		method, path, body = http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", "["+createBody+"]"
		handle = NewCreateExpectedMachinesHandler(dbSession, pool, cfg).Handle
	case "batch update":
		method, path = http.MethodPatch, "/v2/org/test-org/nico/expected-machine/batch"
		body = fmt.Sprintf(`[{"id":%q,"skuId":%q,"name":"updated"}]`, original.ID.String(), skuID)
		handle = NewUpdateExpectedMachinesHandler(dbSession, pool, cfg).Handle
	case "replace all":
		method, path = http.MethodPut, "/v2/org/test-org/nico/expected-machine/all"
		body = fmt.Sprintf(`{"siteId":%q,"expectedMachines":[%s]}`, site.ID.String(), createBody)
		// An invalid SKU must be rejected before resolving a Site client.
		handle = NewReplaceAllExpectedMachinesHandler(dbSession, nil, cfg).Handle
		expectedMessage = "SkuID specified for an Expected Machine does not exist on the Site"
	default:
		t.Fatalf("unknown Expected Machine mutation %q", mutation)
	}

	// Recreate an unattached SKU after any pre-transaction validation has run.
	// The write transaction must validate the replacement row's Site itself.
	hook := &expectedMachineQueryHook{
		queryPrefix: "BEGIN", armed: true,
		beforeQuery: func() {
			err := cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
				_, deleteErr := cdb.GetIDB(tx, dbSession).NewDelete().Model((*cdbm.SKU)(nil)).Where("id = ?", skuID).Exec(ctx)
				if deleteErr != nil {
					return deleteErr
				}
				_, insertErr := cdb.GetIDB(tx, dbSession).NewInsert().Model(&cdbm.SKU{ID: skuID, SiteID: otherSite.ID}).Exec(ctx)
				return insertErr
			})
			require.NoError(t, err)
		},
	}
	dbSession.DB.AddQueryHook(hook)
	request := httptest.NewRequest(method, path, strings.NewReader(body))
	request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	recorder := httptest.NewRecorder()
	echoContext := echo.New().NewContext(request, recorder)
	echoContext.Set("user", user)
	echoContext.SetParamNames("orgName", "id")
	echoContext.SetParamValues(org, id)
	require.NoError(t, handle(echoContext))
	require.True(t, hook.fired, "the SKU recreation must happen before the attachment transaction")
	require.Equal(t, expectedStatus, recorder.Code, recorder.Body.String())
	require.Contains(t, recorder.Body.String(), expectedMessage)
	require.Empty(t, client.Calls, "a rejected SKU attachment must not reach Core")

	stored, err := dao.Get(ctx, nil, original.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, original, stored, "rejected attachment must preserve the existing Expected Machine")
	count, err := dbSession.DB.NewSelect().Model((*cdbm.ExpectedMachine)(nil)).Count(ctx)
	require.NoError(t, err)
	require.Equal(t, 1, count, "rejected attachment must not create an Expected Machine")
}

func TestReplaceAllExpectedMachinesHandler_Handle(t *testing.T) {
	t.Run("pending creation conflicts with replacement", func(t *testing.T) { testExpectedMachinePendingOverlap(t, "replace all") })
	tests := []struct {
		name string
		run  func(*testing.T)
	}{
		{name: "Core failure rolls back replacement", run: testReplaceAllExpectedMachinesRollsBackOnCoreFailure},
		{name: "retained machine loopback intent", run: testReplaceAllExpectedMachinesLoopbacks},
		{name: "requested SKU among larger Site inventory", run: testReplaceAllExpectedMachinesFindsRequestedSKUAmongLargerSiteInventory},
		{name: "SKU recreated on another Site before attachment", run: func(t *testing.T) {
			testExpectedMachineRejectsRecreatedSKU(t, "replace all")
		}},
	}
	for _, test := range tests {
		t.Run(test.name, test.run)
	}
}

func testReplaceAllExpectedMachinesLoopbacks(t *testing.T) {
	dbSession := testExpectedMachineInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	const org = "test-org"
	_, site := testExpectedMachineSetupTestData(t, dbSession, org)
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("loopback-user"), OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	dao := cdbm.NewExpectedMachineDAO(dbSession)
	loopback := cdbm.ExpectedMachineInterface{
		MacAddress: cutil.GetPtr("02:00:00:00:00:20"), Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleDpuLoopback),
		FixedIP: cutil.GetPtr("198.51.100.20"), IPAllocation: cutil.GetPtr(cdbm.ExpectedInterfaceIPAllocationFixed),
	}
	client := &tmocks.Client{}
	run := &tmocks.WorkflowRun{}
	var captured corev1.ExpectedMachineList
	client.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).
		Run(func(args mock.Arguments) {
			captured = corev1.ExpectedMachineList{}
			testDecodeExpectedComponentPatch(t, args.Get(3), site.ID.String(), &captured)
		}).Return(run, nil)
	run.On("Get", mock.Anything, mock.Anything).Return(nil)
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	handler := NewReplaceAllExpectedMachinesHandler(dbSession, pool, common.GetTestConfig())
	tests := []struct {
		name          string
		fields        string
		wantLoopback  string
		wantPhysical  bool
		wantWireCount int
		wantReplace   bool
	}{
		{
			name:         "physical-only retained machine preserves reservations",
			fields:       `,"interfaces":[{"macAddress":"02:00:00:00:00:20","role":"DpuOs"}]`,
			wantLoopback: "198.51.100.20", wantPhysical: true, wantWireCount: 1,
		},
		{name: "omitted interfaces preserve loopbacks only", wantLoopback: "198.51.100.20"},
		{name: "explicit empty replacement clears reservations", fields: `,"interfaces":[],"replaceDpuLoopbacks":true`, wantReplace: true},
		{
			name:         "supplied loopback replaces prior subset",
			fields:       `,"interfaces":[{"macAddress":"02:00:00:00:00:20","role":"DpuLoopback","fixedIp":"198.51.100.21","ipAllocation":"Fixed"}]`,
			wantLoopback: "198.51.100.21", wantWireCount: 1,
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			require.NoError(t, dao.DeleteAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}))
			retained, err := dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
				ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:AA:BB",
				ChassisSerialNumber: "retained", CreatedBy: user.ID,
				Interfaces: []cdbm.ExpectedMachineInterface{
					{MacAddress: loopback.MacAddress, Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleDpuOs), FixedIP: cutil.GetPtr("192.0.2.20")},
					loopback,
				},
			})
			require.NoError(t, err)
			removed, err := dao.Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
				ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:AA:CC",
				ChassisSerialNumber: "removed", CreatedBy: user.ID,
			})
			require.NoError(t, err)
			body := fmt.Sprintf(`{"siteId":%q,"expectedMachines":[{"siteId":%q,"bmcMacAddress":"00-11-22-33-aa-bb","chassisSerialNumber":"retained"%s}]}`, site.ID.String(), site.ID.String(), test.fields)
			recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodPut,
				"/v2/org/test-org/nico/expected-machine/all", body, "", handler.Handle)
			require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
			require.Len(t, captured.ExpectedMachines, 1)
			forwarded := captured.ExpectedMachines[0]
			require.True(t, forwarded.ReplaceHostNics)
			require.Equal(t, test.wantReplace, forwarded.ReplaceDpuLoopbacks)
			require.Len(t, forwarded.HostNics, test.wantWireCount, "Core must only receive submitted declarations")
			var response []model.APIExpectedMachine
			require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
			require.Len(t, response, 1)
			require.NotEqual(t, retained.ID, response[0].ID, "preservation must use the MAC, not replaced DB IDs")
			stored, err := dao.Get(ctx, nil, response[0].ID, nil, false)
			require.NoError(t, err)
			want := []cdbm.ExpectedMachineInterface{}
			if test.wantPhysical {
				want = append(want, cdbm.ExpectedMachineInterface{MacAddress: loopback.MacAddress, Role: cutil.GetPtr(cdbm.ExpectedInterfaceRoleDpuOs)})
			}
			if test.wantLoopback != "" {
				reservation := loopback
				reservation.FixedIP = cutil.GetPtr(test.wantLoopback)
				want = append(want, reservation)
			}
			require.Equal(t, want, stored.Interfaces)
			require.Equal(t, model.NewAPIExpectedMachine(stored).Interfaces, response[0].Interfaces)
			_, err = dao.Get(ctx, nil, removed.ID, nil, false)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			_, count, err := dao.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
			require.NoError(t, err)
			require.Equal(t, 1, count)
		})
	}
}

func testReplaceAllExpectedMachinesRollsBackOnCoreFailure(t *testing.T) {
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, model := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil), (*cdbm.PendingExpectedMachine)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, model))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("batch-user"), OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
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
	recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodPut, "/v2/org/test-org/nico/expected-machine/all", body, "", NewReplaceAllExpectedMachinesHandler(dbSession, pool, common.GetTestConfig()).Handle)
	require.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())

	stored, err := dao.Get(ctx, nil, original.ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, "original", stored.ChassisSerialNumber)
	_, count, err := dao.GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 1, count)
}

func testReplaceAllExpectedMachinesFindsRequestedSKUAmongLargerSiteInventory(t *testing.T) {
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, model := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil), (*cdbm.PendingExpectedMachine)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, model))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("batch-user"), OrgData: cdbm.OrgData{org: cdbm.Org{Name: org, Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	for _, sku := range []cdbm.SKU{
		{ID: "unrequested-sku", SiteID: site.ID, Created: time.Unix(1, 0)},
		{ID: "requested-sku", SiteID: site.ID, Created: time.Unix(2, 0)},
	} {
		_, err = dbSession.DB.NewInsert().Model(&sku).Exec(ctx)
		require.NoError(t, err)
	}
	connection, err := dbSession.DB.DB.Conn(ctx)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, connection.Close()) })
	var lockError error
	lockProbe := &expectedMachineQueryHook{
		queryPrefix: `INSERT INTO "expected_machine"`, armed: true,
		beforeQuery: func() {
			// Probe before the INSERT can acquire its own foreign-key lock.
			var skuID string
			lockError = connection.QueryRowContext(ctx, "SELECT id FROM sku WHERE id = $1 FOR UPDATE NOWAIT", "requested-sku").Scan(&skuID)
		},
	}
	dbSession.DB.AddQueryHook(lockProbe)

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
	recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodPut, "/v2/org/test-org/nico/expected-machine/all", body, "", NewReplaceAllExpectedMachinesHandler(dbSession, pool, common.GetTestConfig()).Handle)
	require.Equal(t, http.StatusOK, recorder.Code, recorder.Body.String())
	require.True(t, lockProbe.fired, "the lock probe must run before inserting the Expected Machine")
	var pgErr *pgconn.PgError
	require.ErrorAs(t, lockError, &pgErr, "the handler must retain the SKU lock through the attachment write")
	require.Equal(t, "55P03", pgErr.Code)
	var response []model.APIExpectedMachine
	require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
	require.Len(t, response, 1)
	require.Equal(t, cutil.GetPtr("requested-sku"), response[0].SkuID)
	stored, err := cdbm.NewExpectedMachineDAO(dbSession).Get(ctx, nil, response[0].ID, nil, false)
	require.NoError(t, err)
	require.Equal(t, response[0].SkuID, stored.SkuID)
	require.Len(t, client.Calls, 1)
	var dispatched corev1.ExpectedMachineList
	testDecodeExpectedComponentPatch(t, client.Calls[0].Arguments.Get(3), site.ID.String(), &dispatched)
	require.Len(t, dispatched.ExpectedMachines, 1)
	require.Equal(t, response[0].SkuID, dispatched.ExpectedMachines[0].SkuId)
}

func TestExpectedInventoryBatchHandlers(t *testing.T) {
	dbSession := testExpectedRackInitDB(t)
	t.Cleanup(func() { dbSession.Close() })
	ctx := context.Background()
	for _, model := range []any{(*cdbm.InstanceType)(nil), (*cdbm.Machine)(nil), (*cdbm.SKU)(nil), (*cdbm.ExpectedMachine)(nil), (*cdbm.PendingExpectedMachine)(nil), (*cdbm.ExpectedSwitch)(nil), (*cdbm.ExpectedPowerShelf)(nil)} {
		require.NoError(t, dbSession.DB.ResetModel(ctx, model))
	}

	const org = "test-org"
	_, site, _ := testExpectedRackSetupTestData(t, dbSession, org)
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("batch-user"),
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
			body:    fmt.Sprintf(`{"siteId":%q,"expectedMachines":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:01","chassisSerialNumber":"machine-1","defaultBmcUsername":"machine-user","defaultBmcPassword":"machine-pass","interfaces":[{"macAddress":"02:00:00:00:00:09","nicType":"CX9","fixedIp":"192.0.2.9"}]},{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:09","chassisSerialNumber":"machine-empty"}]}`, site.ID.String(), site.ID.String(), site.ID.String()),
			handle:  NewReplaceAllExpectedMachinesHandler(dbSession, pool, cfg).Handle,
			method:  corev1.Forge_ReplaceAllExpectedMachines_FullMethodName,
			secrets: []string{"machine-user", "machine-pass"},
		},
		{
			name: "switch", path: "/v2/org/test-org/nico/expected-switch/all",
			body:    fmt.Sprintf(`{"siteId":%q,"expectedSwitches":[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:02","switchSerialNumber":"switch-1","defaultBmcUsername":"switch-user","defaultBmcPassword":"switch-pass","nvOsUsername":"nvos-user","nvOsPassword":"nvos-pass","nvosInterfaces":[{"macAddress":"02:00:00:00:00:20"},{"macAddress":"02:00:00:00:00:21","isPrimary":true}]}]}`, site.ID.String(), site.ID.String()),
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
			recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodPut, replacement.path, replacement.body, "", replacement.handle)
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
		require.False(t, machine.ReplaceDpuLoopbacks, "replace-all must preserve omitted loopbacks without explicit intent")
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
	interfaces := switchList.ExpectedSwitches[0].NvosInterfaces
	require.Len(t, interfaces, 2)
	require.Equal(t, "02:00:00:00:00:20", interfaces[0].MacAddress)
	require.False(t, interfaces[0].IsPrimary)
	require.Equal(t, "02:00:00:00:00:21", interfaces[1].MacAddress)
	require.True(t, interfaces[1].IsPrimary)
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
			recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodDelete, deletion.path, "", site.ID.String(), deletion.handle)
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

	_, err = cdbm.NewExpectedMachineDAO(dbSession).Create(ctx, nil, cdbm.ExpectedMachineCreateInput{
		ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:21",
		ChassisSerialNumber: "retained-machine", CreatedBy: user.ID,
	})
	require.NoError(t, err)
	_, err = cdbm.NewExpectedSwitchDAO(dbSession).Create(ctx, nil, cdbm.ExpectedSwitchCreateInput{
		ExpectedSwitchID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:22",
		SwitchSerialNumber: "retained-switch", CreatedBy: user.ID,
	})
	require.NoError(t, err)
	_, err = cdbm.NewExpectedPowerShelfDAO(dbSession).Create(ctx, nil, cdbm.ExpectedPowerShelfCreateInput{
		ExpectedPowerShelfID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:23",
		ShelfSerialNumber: "retained-shelf", CreatedBy: user.ID,
	})
	require.NoError(t, err)

	lockTx, err := cdb.BeginTx(ctx, dbSession, nil)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, lockTx.Rollback()) })
	lockID := cdb.GetAdvisoryLockIDFromString(expectedInventoryMutationLockPrefix + site.ID.String())
	require.NoError(t, lockTx.TryAcquireAdvisoryLock(ctx, lockID, nil))
	for _, replacement := range replacements {
		t.Run("reject concurrent "+replacement.name+" replacement", func(t *testing.T) {
			callsBefore := len(captured)
			recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodPut, replacement.path, replacement.body, "", replacement.handle)
			require.Equal(t, http.StatusConflict, recorder.Code, recorder.Body.String())
			require.Len(t, captured, callsBefore, "lock conflict must not reach Core")
		})
	}
	for _, deletion := range deletions {
		t.Run("reject concurrent "+deletion.name+" deletion", func(t *testing.T) {
			callsBefore := len(captured)
			recorder := invokeExpectedInventoryBatchHandler(t, user, org, http.MethodDelete, deletion.path, "", site.ID.String(), deletion.handle)
			require.Equal(t, http.StatusConflict, recorder.Code, recorder.Body.String())
			require.Len(t, captured, callsBefore, "lock conflict must not reach Core")
		})
	}

	machines, count, err := cdbm.NewExpectedMachineDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedMachineFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 1, count)
	require.Equal(t, "retained-machine", machines[0].ChassisSerialNumber)
	switches, count, err := cdbm.NewExpectedSwitchDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedSwitchFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 1, count)
	require.Equal(t, "retained-switch", switches[0].SwitchSerialNumber)
	shelves, count, err := cdbm.NewExpectedPowerShelfDAO(dbSession).GetAll(ctx, nil, cdbm.ExpectedPowerShelfFilterInput{SiteIDs: []uuid.UUID{site.ID}}, paginator.PageInput{}, nil)
	require.NoError(t, err)
	require.Equal(t, 1, count)
	require.Equal(t, "retained-shelf", shelves[0].ShelfSerialNumber)
}

func invokeExpectedInventoryBatchHandler(t *testing.T, user *cdbm.User, org, method, path, body, siteID string, handle func(echo.Context) error) *httptest.ResponseRecorder {
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

func invokeExpectedInventoryMutationHandler(t *testing.T, user *cdbm.User, org, method, path, body, id string, handle func(echo.Context) error) *httptest.ResponseRecorder {
	t.Helper()
	request := httptest.NewRequest(method, path, strings.NewReader(body))
	request.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	recorder := httptest.NewRecorder()
	echoContext := echo.New().NewContext(request, recorder)
	echoContext.Set("user", user)
	echoContext.SetParamNames("orgName", "id")
	echoContext.SetParamValues(org, id)
	require.NoError(t, handle(echoContext))
	return recorder
}
