// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	commonpb "go.temporal.io/api/common/v1"
	enumspb "go.temporal.io/api/enums/v1"
	failurepb "go.temporal.io/api/failure/v1"
	historypb "go.temporal.io/api/history/v1"
	workflowpb "go.temporal.io/api/workflow/v1"
	workflowservice "go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	sc "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/site"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	expectedmachineactivity "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/expectedmachine"
	workflowclient "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
)

type expectedMachineAPIRecoveryFixture struct {
	ctx     context.Context
	session *cdb.Session
	site    *cdbm.Site
	user    *cdbm.User
	pool    *sc.ClientPool
	client  *tmocks.Client
}

func newExpectedMachineAPIRecoveryFixture(t *testing.T) *expectedMachineAPIRecoveryFixture {
	t.Helper()
	session := testExpectedMachineInitDB(t)
	t.Cleanup(session.Close)
	ctx := context.Background()
	_, site := testExpectedMachineSetupTestData(t, session, "test-org")
	user := &cdbm.User{ID: uuid.New(), StarfleetID: cutil.GetPtr("recovery-user"), OrgData: cdbm.OrgData{"test-org": cdbm.Org{Name: "test-org", Roles: []string{"FORGE_PROVIDER_ADMIN"}}}}
	_, err := session.DB.NewInsert().Model(user).Exec(ctx)
	require.NoError(t, err)
	pool := sc.NewClientPool(nil)
	temporalClient := &tmocks.Client{}
	pool.IDClientMap[site.ID.String()] = temporalClient
	return &expectedMachineAPIRecoveryFixture{ctx: ctx, session: session, site: site, user: user, pool: pool, client: temporalClient}
}

func expectedMachineBatchRecoveryConfig(t *testing.T) *config.Config {
	t.Helper()
	t.Setenv(strings.ToUpper(config.ConfigExpectedMachineBatchRecoveryEnabled), "true")
	return common.GetTestConfig()
}

func expectedMachineBatchRejection() error {
	converter := temporal.GetDefaultFailureConverter()
	return converter.FailureToError(&failurepb.Failure{
		Message: "activity failed", Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("rejected reservation-secret", swe.ErrTypeNICoInvalidArgument, nil)),
		FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}, ActivityId: "batch-create", ScheduledEventId: 10, StartedEventId: 12}},
	})
}

func fillExpectedMachineBatchSuccess(value any, request *corev1.BatchExpectedMachineOperationRequest) {
	result := value.(*corev1.BatchExpectedMachineOperationResponse)
	for _, machine := range request.GetExpectedMachines().GetExpectedMachines() {
		result.Results = append(result.Results, &corev1.ExpectedMachineOperationResult{Id: machine.GetId(), Success: true})
	}
}

func testExpectedMachineBatchRecovery(t *testing.T) {
	completionGate := make(chan struct{})
	completed := make(chan *corev1.BatchExpectedMachineOperationRequest, 1)
	tests := []struct {
		name            string
		startErr        error
		timeout         time.Duration
		duringDispatch  func(*testing.T, *expectedMachineAPIRecoveryFixture)
		get             func(*testing.T, *expectedMachineAPIRecoveryFixture, context.Context, any, *corev1.BatchExpectedMachineOperationRequest, context.CancelFunc) error
		wantStatus      int
		wantPending     bool
		historyAttempt  int32
		bindFailure     bool
		noDispatchProof bool
		checkDuration   func(*testing.T, time.Duration)
		afterFailure    func(*testing.T, *expectedMachineAPIRecoveryFixture, *corev1.BatchExpectedMachineOperationRequest, string)
	}{
		{name: "caller deadline retains durable identities and independent admission", timeout: 250 * time.Millisecond, duringDispatch: testExpectedMachineIndependentBatchAdmission, get: func(_ *testing.T, _ *expectedMachineAPIRecoveryFixture, ctx context.Context, _ any, _ *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			<-ctx.Done()
			return ctx.Err()
		}, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "canceled admission recovers a controlled late success with a fresh worker", get: func(t *testing.T, _ *expectedMachineAPIRecoveryFixture, ctx context.Context, _ any, request *corev1.BatchExpectedMachineOperationRequest, cancel context.CancelFunc) error {
			go func() { <-completionGate; completed <- request }()
			t.Cleanup(func() {
				select {
				case <-completionGate:
				default:
					close(completionGate)
				}
			})
			cancel()
			return ctx.Err()
		}, afterFailure: func(t *testing.T, f *expectedMachineAPIRecoveryFixture, request *corev1.BatchExpectedMachineOperationRequest, workflowID string) {
			select {
			case <-completed:
				t.Fatal("creation completed before the canceled response")
			default:
			}
			close(completionGate)
			select {
			case late := <-completed:
				require.Same(t, request, late)
			case <-time.After(time.Second):
				t.Fatal("late creation did not pass its gate")
			}
			freshClient, freshRun := &tmocks.Client{}, &tmocks.WorkflowRun{}
			freshPool := workflowclient.NewClientPool(nil)
			freshPool.IDClientMap[f.site.ID.String()] = freshClient
			freshClient.On("DescribeWorkflowExecution", mock.Anything, workflowID, "batch-create-run").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED, Execution: &commonpb.WorkflowExecution{RunId: "batch-create-run"}}}, nil).Once()
			freshClient.On("GetWorkflow", mock.Anything, workflowID, "batch-create-run").Return(freshRun).Once()
			freshRun.On("GetWithOptions", mock.Anything, mock.Anything, client.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) { fillExpectedMachineBatchSuccess(args.Get(1), request) }).Return(nil).Once()
			require.NoError(t, expectedmachineactivity.NewManageExpectedMachine(f.session, freshPool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}))
			for _, machine := range request.GetExpectedMachines().GetExpectedMachines() {
				stored, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, uuid.MustParse(machine.GetId().GetValue()), nil, false)
				require.NoError(t, err)
				require.Equal(t, machine.GetChassisSerialNumber(), stored.ChassisSerialNumber)
				require.Equal(t, f.user.ID, stored.CreatedBy)
			}
			first, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, uuid.MustParse(request.GetExpectedMachines().GetExpectedMachines()[0].GetId().GetValue()), nil, false)
			require.NoError(t, err)
			require.Equal(t, cutil.GetPtr("original request description"), first.Description)
			pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
			freshClient.AssertExpectations(t)
			freshRun.AssertExpectations(t)
		}, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "run binding failure retains admission without observing a result", bindFailure: true, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "dispatch ambiguity retains durable identities", startErr: errors.New("dispatch unavailable reservation-secret"), wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "verified first-attempt rejection releases the reservation", historyAttempt: 1, get: func(_ *testing.T, _ *expectedMachineAPIRecoveryFixture, _ context.Context, _ any, _ *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			return expectedMachineBatchRejection()
		}, wantStatus: http.StatusInternalServerError},
		{name: "closed original workflow with no scheduled activity clears the reservation", noDispatchProof: true, get: func(_ *testing.T, _ *expectedMachineAPIRecoveryFixture, _ context.Context, _ any, _ *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			return temporal.NewTimeoutError(enumspb.TIMEOUT_TYPE_START_TO_CLOSE, nil)
		}, wantStatus: http.StatusInternalServerError},
		{name: "a prior activity retry keeps the reservation", historyAttempt: 2, get: func(_ *testing.T, _ *expectedMachineAPIRecoveryFixture, _ context.Context, _ any, _ *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			return expectedMachineBatchRejection()
		}, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "malformed result retains the reservation", get: func(_ *testing.T, _ *expectedMachineAPIRecoveryFixture, _ context.Context, value any, request *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			id := request.GetExpectedMachines().GetExpectedMachines()[0].GetId()
			value.(*corev1.BatchExpectedMachineOperationResponse).Results = []*corev1.ExpectedMachineOperationResult{{Id: id, Success: true}, {Id: id, Success: true}}
			return nil
		}, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "confirmed success followed by finalization contention returns 500", timeout: 250 * time.Millisecond, checkDuration: func(t *testing.T, elapsed time.Duration) {
			require.Less(t, elapsed, 700*time.Millisecond, "retry backoff must stop at the caller deadline")
		}, get: func(t *testing.T, f *expectedMachineAPIRecoveryFixture, _ context.Context, value any, request *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			lock, err := cdb.BeginTx(f.ctx, f.session, nil)
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, lock.Rollback()) })
			require.NoError(t, lock.TryAcquireSharedAdvisoryLock(f.ctx, cdbm.ExpectedInventoryMutationLockID(f.site.ID), nil))
			fillExpectedMachineBatchSuccess(value, request)
			return nil
		}, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "finalization retries after the first contended attempt is released", get: func(t *testing.T, f *expectedMachineAPIRecoveryFixture, _ context.Context, value any, request *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			lock, err := cdb.BeginTx(f.ctx, f.session, nil)
			require.NoError(t, err)
			require.NoError(t, lock.TryAcquireSharedAdvisoryLock(f.ctx, cdbm.ExpectedInventoryMutationLockID(f.site.ID), nil))
			released := false
			hook := &expectedMachineQueryHook{queryPrefix: fmt.Sprintf("SELECT pg_try_advisory_xact_lock(%d)", cdbm.ExpectedInventoryMutationLockID(f.site.ID)), armed: true, afterQuery: func(*bun.QueryEvent) { require.NoError(t, lock.Rollback()); released = true }}
			f.session.DB.AddQueryHook(hook)
			t.Cleanup(func() {
				if !released {
					require.NoError(t, lock.Rollback())
				}
				require.True(t, hook.fired)
			})
			fillExpectedMachineBatchSuccess(value, request)
			return nil
		}, wantStatus: http.StatusCreated},
		{name: "finalization cannot outlive the caller wait budget", timeout: 250 * time.Millisecond, get: func(_ *testing.T, _ *expectedMachineAPIRecoveryFixture, ctx context.Context, value any, request *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			<-ctx.Done()
			fillExpectedMachineBatchSuccess(value, request)
			return nil
		}, wantStatus: http.StatusInternalServerError, wantPending: true},
		{name: "worker and admitted request finish after new recovery admission is disabled", get: func(t *testing.T, f *expectedMachineAPIRecoveryFixture, _ context.Context, value any, request *corev1.BatchExpectedMachineOperationRequest, _ context.CancelFunc) error {
			pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Len(t, pending, 1)
			t.Setenv(strings.ToUpper(config.ConfigExpectedMachineBatchRecoveryEnabled), "false")
			require.False(t, common.GetTestConfig().GetExpectedMachineBatchRecoveryEnabled())
			activity := expectedmachineactivity.NewManageExpectedMachine(f.session, nil)
			require.NoError(t, activity.UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{
				InventoryStatus:  corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
				ExpectedMachines: request.GetExpectedMachines().GetExpectedMachines(),
			}))
			fillExpectedMachineBatchSuccess(value, request)
			return nil
		}, wantStatus: http.StatusCreated},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			f := newExpectedMachineAPIRecoveryFixture(t)
			bindFailure := &expectedMachineAdmissionFailureHook{match: func(event *bun.QueryEvent) bool {
				return event.Operation() == "UPDATE" && strings.Contains(event.Query, `"pending_expected_machine"`) && strings.Contains(event.Query, "SET run_id")
			}}
			if tc.bindFailure {
				f.session.DB.AddQueryHook(bindFailure)
			}
			run := &tmocks.WorkflowRun{}
			var dispatched *corev1.BatchExpectedMachineOperationRequest
			var workflowID string
			var cancelRequest context.CancelFunc
			f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedMachines", mock.Anything).Run(func(args mock.Arguments) {
				dispatched = args.Get(3).(*corev1.BatchExpectedMachineOperationRequest)
				options := args.Get(1).(client.StartWorkflowOptions)
				workflowID = options.ID
				require.Equal(t, cutil.WorkflowExecutionTimeout, options.WorkflowExecutionTimeout)
				pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
				require.NoError(t, err)
				require.Len(t, pending, 1)
				require.Len(t, pending[0].Inputs, 2, "the entire group must commit before dispatch")
				require.Equal(t, workflowID, pending[0].WorkflowID)
				require.Nil(t, pending[0].RunID, "original execution is not bound before dispatch acknowledgement")
				for i, input := range pending[0].Inputs {
					require.Equal(t, dispatched.GetExpectedMachines().GetExpectedMachines()[i].GetId().GetValue(), input.ExpectedMachineID.String())
				}
				encoded, err := json.Marshal(pending[0])
				require.NoError(t, err)
				require.NotContains(t, string(encoded), "reservation-secret")
				require.Equal(t, "reservation-secret", dispatched.GetExpectedMachines().GetExpectedMachines()[0].GetBmcPassword())
				if tc.duringDispatch != nil {
					tc.duringDispatch(t, f)
				}
			}).Return(run, tc.startErr).Once()
			var resultCtx context.Context
			if tc.startErr == nil {
				run.On("GetRunID").Return("batch-create-run").Once()
			}
			if tc.get != nil {
				run.On("GetWithOptions", mock.Anything, mock.Anything, client.WorkflowRunGetOptions{DisableFollowingRuns: true}).Return(func(ctx context.Context, value any, _ client.WorkflowRunGetOptions) error {
					resultCtx = ctx
					return tc.get(t, f, ctx, value, dispatched, cancelRequest)
				}).Once()
			}
			if tc.noDispatchProof {
				f.client.On("DescribeWorkflowExecution", mock.Anything, mock.MatchedBy(func(id string) bool { return id == workflowID }), "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_TIMED_OUT, Execution: &commonpb.WorkflowExecution{RunId: "batch-create-run"}}}, nil).Once()
				f.client.On("GetWorkflowHistory", mock.Anything, mock.MatchedBy(func(id string) bool { return id == workflowID }), "batch-create-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(&expectedMachineAPIRejectionHistory{events: []*historypb.HistoryEvent{
					{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{WorkflowType: &commonpb.WorkflowType{Name: "CreateExpectedMachines"}, OriginalExecutionRunId: "batch-create-run", FirstExecutionRunId: "batch-create-run", Attempt: 1}}},
					{EventId: 2, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT, Attributes: &historypb.HistoryEvent_WorkflowExecutionTimedOutEventAttributes{WorkflowExecutionTimedOutEventAttributes: &historypb.WorkflowExecutionTimedOutEventAttributes{}}},
				}}).Once()
			} else if tc.historyAttempt != 0 {
				f.client.On("DescribeWorkflowExecution", mock.Anything, mock.MatchedBy(func(id string) bool { return id == workflowID }), "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, Execution: &commonpb.WorkflowExecution{RunId: "batch-create-run"}}}, nil).Once()
			} else {
				f.client.On("DescribeWorkflowExecution", mock.Anything, mock.Anything, "").Return(&workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING, Execution: &commonpb.WorkflowExecution{RunId: "batch-create-run"}}}, nil).Maybe()
			}
			if tc.historyAttempt != 0 {
				history := &expectedMachineAPIRejectionHistory{events: []*historypb.HistoryEvent{
					{EventId: 10, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{ActivityId: "batch-create", ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}}}},
					{EventId: 12, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{ScheduledEventId: 10, Attempt: tc.historyAttempt}}},
				}}
				f.client.On("GetWorkflowHistory", mock.Anything, mock.MatchedBy(func(id string) bool { return id == workflowID }), "batch-create-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Run(func(args mock.Arguments) { require.Same(t, resultCtx, args.Get(0)) }).Return(history).Once()
			}
			handler := NewCreateExpectedMachinesHandler(f.session, f.pool, expectedMachineBatchRecoveryConfig(t))
			require.Equal(t, cutil.WorkflowContextTimeout, handler.workflowTimeout)
			if tc.timeout != 0 {
				handler.workflowTimeout = tc.timeout
			}
			handle := func(c echo.Context) error {
				ctx, cancel := context.WithCancel(c.Request().Context())
				cancelRequest = cancel
				defer cancel()
				c.SetRequest(c.Request().WithContext(ctx))
				return handler.Handle(c)
			}
			var logs bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
			t.Cleanup(func() { log.Logger = previousLogger })
			body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:18","chassisSerialNumber":"reserved-first","defaultBmcPassword":"reservation-secret","description":"original request description"},{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:19","chassisSerialNumber":"reserved-second"}]`, f.site.ID, f.site.ID)
			started := time.Now()
			recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", handle)
			if tc.checkDuration != nil {
				tc.checkDuration(t, time.Since(started))
			}
			require.Equal(t, tc.wantStatus, recorder.Code, recorder.Body.String())
			require.NotContains(t, recorder.Body.String(), "reservation-secret")
			require.NotContains(t, logs.String(), "reservation-secret")
			pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			if tc.wantPending {
				require.Len(t, pending, 1)
				require.Equal(t, workflowID, pending[0].WorkflowID)
				require.Len(t, pending[0].Inputs, 2)
				if tc.startErr != nil || tc.bindFailure {
					require.Nil(t, pending[0].RunID)
				} else {
					require.Equal(t, cutil.GetPtr("batch-create-run"), pending[0].RunID)
				}
			} else {
				require.Empty(t, pending)
			}
			if tc.wantStatus == http.StatusCreated {
				var response []model.APIExpectedMachine
				require.NoError(t, json.Unmarshal(recorder.Body.Bytes(), &response))
				require.Len(t, response, 2)
				for i, machine := range dispatched.GetExpectedMachines().GetExpectedMachines() {
					require.Equal(t, machine.GetId().GetValue(), response[i].ID.String())
					stored, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, response[i].ID, nil, false)
					require.NoError(t, err)
					require.Equal(t, f.user.ID, stored.CreatedBy)
				}
			} else {
				for _, machine := range dispatched.GetExpectedMachines().GetExpectedMachines() {
					_, err = cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, uuid.MustParse(machine.GetId().GetValue()), nil, false)
					require.ErrorIs(t, err, cdb.ErrDoesNotExist)
				}
			}
			if tc.bindFailure {
				require.True(t, bindFailure.fired)
			}
			if tc.afterFailure != nil {
				tc.afterFailure(t, f, dispatched, workflowID)
			}
			f.client.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
			f.client.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}

func testExpectedMachineIndependentBatchAdmission(t *testing.T, f *expectedMachineAPIRecoveryFixture) {
	t.Helper()
	overlapBody := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:21","chassisSerialNumber":"reserved-second"}]`, f.site.ID)
	overlap := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", overlapBody, "", NewCreateExpectedMachinesHandler(f.session, f.pool, expectedMachineBatchRecoveryConfig(t)).Handle)
	require.Equal(t, http.StatusConflict, overlap.Code, overlap.Body.String())
	independentClient, independentRun := &tmocks.Client{}, &tmocks.WorkflowRun{}
	independentPool := sc.NewClientPool(nil)
	independentPool.IDClientMap[f.site.ID.String()] = independentClient
	var independentRequest *corev1.BatchExpectedMachineOperationRequest
	independentClient.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedMachines", mock.Anything).Run(func(args mock.Arguments) {
		independentRequest = args.Get(3).(*corev1.BatchExpectedMachineOperationRequest)
	}).Return(independentRun, nil).Once()
	independentRun.On("GetRunID").Return("independent-create-run").Once()
	independentRun.On("GetWithOptions", mock.Anything, mock.Anything, client.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) { fillExpectedMachineBatchSuccess(args.Get(1), independentRequest) }).Return(nil).Once()
	body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:20","chassisSerialNumber":"independent"}]`, f.site.ID)
	recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", NewCreateExpectedMachinesHandler(f.session, independentPool, expectedMachineBatchRecoveryConfig(t)).Handle)
	require.Equal(t, http.StatusCreated, recorder.Code, recorder.Body.String())
	independentClient.AssertExpectations(t)
	independentRun.AssertExpectations(t)
}

type expectedMachineAdmissionFailureHook struct {
	match func(*bun.QueryEvent) bool
	fired bool
}

func (h *expectedMachineAdmissionFailureHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.fired && h.match(event) {
		h.fired = true
		canceled, cancel := context.WithCancel(ctx)
		cancel()
		return canceled
	}
	return ctx
}
func (*expectedMachineAdmissionFailureHook) AfterQuery(context.Context, *bun.QueryEvent) {}

func testExpectedMachineBatchAdmissionFailure(t *testing.T) {
	t.Run("Site row contention consumes the admission budget without dispatch", testExpectedMachineAdmissionRowDeadline)
	t.Run("request ends after admission commits", testExpectedMachineCanceledBeforeDispatch)
	tests := []struct {
		name       string
		before     func(*testing.T, *expectedMachineAPIRecoveryFixture)
		match      func(*bun.QueryEvent) bool
		wantStatus int
		wantLog    string
	}{
		{name: "Site deleted before locked admission remains a 400", before: func(t *testing.T, f *expectedMachineAPIRecoveryFixture) {
			f.session.DB.AddQueryHook(&expectedMachineQueryHook{queryPrefix: "BEGIN", armed: true, beforeQuery: func() { require.NoError(t, cdbm.NewSiteDAO(f.session).Delete(f.ctx, nil, f.site.ID)) }})
		}, wantStatus: http.StatusBadRequest},
		{name: "operational locked Site lookup failure is a 500", match: func(event *bun.QueryEvent) bool {
			return strings.Contains(event.Query, `FROM "site"`) && strings.Contains(event.Query, "FOR NO KEY UPDATE")
		}, wantStatus: http.StatusInternalServerError, wantLog: "failed to lock Site for Expected Machine batch admission"},
		{name: "pending lookup failure logs its DB cause without dispatch", match: func(event *bun.QueryEvent) bool {
			return event.Operation() == "SELECT" && strings.Contains(event.Query, `FROM "pending_expected_machine"`)
		}, wantStatus: http.StatusInternalServerError, wantLog: "failed to retrieve pending Expected Machine batches"},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			f := newExpectedMachineAPIRecoveryFixture(t)
			if tc.before != nil {
				tc.before(t, f)
			}
			var failure *expectedMachineAdmissionFailureHook
			if tc.match != nil {
				failure = &expectedMachineAdmissionFailureHook{match: tc.match}
				f.session.DB.AddQueryHook(failure)
			}
			var logs bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
			t.Cleanup(func() { log.Logger = previousLogger })
			body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:18","chassisSerialNumber":"admission"}]`, f.site.ID)
			recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", NewCreateExpectedMachinesHandler(f.session, f.pool, expectedMachineBatchRecoveryConfig(t)).Handle)
			require.Equal(t, tc.wantStatus, recorder.Code, recorder.Body.String())
			if failure != nil {
				require.True(t, failure.fired)
			}
			if tc.wantLog != "" {
				require.Contains(t, logs.String(), tc.wantLog)
				found := false
				for _, line := range bytes.Split(logs.Bytes(), []byte("\n")) {
					var entry map[string]any
					if json.Unmarshal(line, &entry) == nil && entry["message"] == tc.wantLog {
						cause, ok := entry["error"].(string)
						require.True(t, ok, "DB cause must be an error field")
						require.Contains(t, cause, "canceled")
						found = true
					}
				}
				require.True(t, found)
			}
			require.Empty(t, f.client.Calls)
			pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
		})
	}
}

func testExpectedMachineCanceledBeforeDispatch(t *testing.T) {
	cases := []struct {
		name           string
		cleanupFailure bool
		wantPending    int
	}{
		{name: "releases the undispatched reservation"},
		{name: "cleanup failure retains the reservation", cleanupFailure: true, wantPending: 1},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newExpectedMachineAPIRecoveryFixture(t)
			dao := cdbm.NewPendingExpectedMachineBatchDAO(f.session)
			var cancelRequest context.CancelFunc
			commit := &expectedMachineQueryHook{queryPrefix: "COMMIT", armed: true, afterQuery: func(event *bun.QueryEvent) {
				require.NoError(t, event.Err)
				pending, err := dao.GetAll(f.ctx, nil, f.site.ID)
				require.NoError(t, err)
				require.Len(t, pending, 1, "admission must commit before cancellation")
				cancelRequest()
			}}
			f.session.DB.AddQueryHook(commit)
			var failure *expectedMachineAdmissionFailureHook
			if tc.cleanupFailure {
				failure = &expectedMachineAdmissionFailureHook{match: func(event *bun.QueryEvent) bool {
					return event.Operation() == "DELETE" && strings.Contains(event.Query, `"pending_expected_machine"`)
				}}
				f.session.DB.AddQueryHook(failure)
			}
			handler := NewCreateExpectedMachinesHandler(f.session, f.pool, expectedMachineBatchRecoveryConfig(t))
			body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:18","chassisSerialNumber":"undispatched"}]`, f.site.ID)
			recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", func(c echo.Context) error {
				ctx, cancel := context.WithCancel(c.Request().Context())
				cancelRequest = cancel
				defer cancel()
				c.SetRequest(c.Request().WithContext(ctx))
				return handler.Handle(c)
			})
			require.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())
			require.True(t, commit.fired)
			if failure != nil {
				require.True(t, failure.fired)
			}
			require.Empty(t, f.client.Calls)
			pending, err := dao.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Len(t, pending, tc.wantPending)
			count, err := f.session.DB.NewSelect().Model((*cdbm.ExpectedMachine)(nil)).Where("site_id = ?", f.site.ID).Count(f.ctx)
			require.NoError(t, err)
			require.Zero(t, count)
		})
	}
}

func testExpectedMachinePendingOverlap(t *testing.T, mutation string) {
	t.Helper()
	f := newExpectedMachineAPIRecoveryFixture(t)
	inputs := []cdbm.ExpectedMachineCreateInput{
		{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:01", ChassisSerialNumber: "reserved-first", CreatedBy: f.user.ID},
		{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:03", ChassisSerialNumber: "reserved-second", CreatedBy: f.user.ID},
	}
	input := inputs[1]
	_, err := cdbm.NewExpectedMachineDAO(f.session).Create(f.ctx, nil, input)
	require.NoError(t, err)
	require.NoError(t, cdbm.NewPendingExpectedMachineBatchDAO(f.session).Create(f.ctx, nil, cdbm.PendingExpectedMachineBatch{WorkflowID: uuid.NewString(), SiteID: f.site.ID, Inputs: inputs}))
	other := cdbm.ExpectedMachineCreateInput{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:04", ChassisSerialNumber: "other", CreatedBy: f.user.ID}
	_, err = cdbm.NewExpectedMachineDAO(f.session).Create(f.ctx, nil, other)
	require.NoError(t, err)
	cfg := common.GetTestConfig()
	if mutation == "batch create" {
		t.Setenv(strings.ToUpper(config.ConfigExpectedMachineBatchRecoveryEnabled), "false")
		require.False(t, cfg.GetExpectedMachineBatchRecoveryEnabled())
	}
	type requestCase struct {
		method, path, body, id string
		handle                 func(echo.Context) error
		status                 int
		check                  func(*testing.T)
	}
	var conflict requestCase
	var additionalConflicts []requestCase
	var independent *requestCase
	switch mutation {
	case "single create":
		conflict = requestCase{method: http.MethodPost, path: "/v2/org/test-org/nico/expected-machine", body: fmt.Sprintf(`{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:02","chassisSerialNumber":"reserved-second"}`, f.site.ID), handle: NewCreateExpectedMachineHandler(f.session, f.pool, cfg).Handle}
		independent = &requestCase{method: http.MethodPost, path: conflict.path, body: fmt.Sprintf(`{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:05","chassisSerialNumber":"independent"}`, f.site.ID), handle: conflict.handle, status: http.StatusCreated}
		additionalConflicts = append(additionalConflicts, requestCase{method: conflict.method, path: conflict.path, body: fmt.Sprintf(`{"siteId":%q,"bmcMacAddress":"00-11-22-33-44-03","chassisSerialNumber":"alias-only"}`, f.site.ID), handle: conflict.handle})
	case "batch create":
		conflict = requestCase{method: http.MethodPost, path: "/v2/org/test-org/nico/expected-machine/batch", body: fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:03","chassisSerialNumber":"different"}]`, f.site.ID), handle: NewCreateExpectedMachinesHandler(f.session, f.pool, cfg).Handle}
	case "single update":
		conflict = requestCase{method: http.MethodPatch, path: "/v2/org/test-org/nico/expected-machine/" + other.ExpectedMachineID.String(), id: other.ExpectedMachineID.String(), body: `{"chassisSerialNumber":"reserved-second"}`, handle: NewUpdateExpectedMachineHandler(f.session, f.pool, cfg).Handle}
		independent = &requestCase{method: conflict.method, path: conflict.path, id: conflict.id, body: `{"name":"independent update"}`, handle: conflict.handle, status: http.StatusOK, check: func(t *testing.T) {
			stored, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, other.ExpectedMachineID, nil, false)
			require.NoError(t, err)
			require.Equal(t, cutil.GetPtr("independent update"), stored.Name)
		}}
		additionalConflicts = append(additionalConflicts, requestCase{method: conflict.method, path: "/v2/org/test-org/nico/expected-machine/" + input.ExpectedMachineID.String(), id: input.ExpectedMachineID.String(), body: `{"name":"reserved target"}`, handle: conflict.handle})
	case "batch update":
		conflict = requestCase{method: http.MethodPatch, path: "/v2/org/test-org/nico/expected-machine/batch", body: fmt.Sprintf(`[{"id":%q,"name":"updated"}]`, input.ExpectedMachineID), handle: NewUpdateExpectedMachinesHandler(f.session, f.pool, cfg).Handle}
		independent = &requestCase{method: conflict.method, path: conflict.path, body: fmt.Sprintf(`[{"id":%q,"name":"independent update"}]`, other.ExpectedMachineID), handle: conflict.handle, status: http.StatusOK, check: func(t *testing.T) {
			stored, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, other.ExpectedMachineID, nil, false)
			require.NoError(t, err)
			require.Equal(t, cutil.GetPtr("independent update"), stored.Name)
		}}
		additionalConflicts = append(additionalConflicts, requestCase{method: conflict.method, path: conflict.path, body: fmt.Sprintf(`[{"id":%q,"chassisSerialNumber":"reserved-second"}]`, other.ExpectedMachineID), handle: conflict.handle})
	case "single delete":
		conflict = requestCase{method: http.MethodDelete, path: "/v2/org/test-org/nico/expected-machine/" + input.ExpectedMachineID.String(), id: input.ExpectedMachineID.String(), handle: NewDeleteExpectedMachineHandler(f.session, f.pool, cfg).Handle}
		independent = &requestCase{method: conflict.method, path: "/v2/org/test-org/nico/expected-machine/" + other.ExpectedMachineID.String(), id: other.ExpectedMachineID.String(), handle: conflict.handle, status: http.StatusNoContent, check: func(t *testing.T) {
			_, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, other.ExpectedMachineID, nil, false)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
		}}
	case "replace all":
		conflict = requestCase{method: http.MethodPut, path: "/v2/org/test-org/nico/expected-machine/all", body: fmt.Sprintf(`{"siteId":%q,"expectedMachines":[]}`, f.site.ID), handle: NewReplaceAllExpectedMachinesHandler(f.session, f.pool, cfg).Handle}
	case "delete all":
		conflict = requestCase{method: http.MethodDelete, path: "/v2/org/test-org/nico/expected-machine/all?siteId=" + f.site.ID.String(), handle: NewDeleteAllExpectedMachinesHandler(f.session, f.pool, cfg).Handle}
	default:
		t.Fatalf("unknown mutation %q", mutation)
	}
	reservedBefore, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
	require.NoError(t, err)
	otherBefore, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, other.ExpectedMachineID, nil, false)
	require.NoError(t, err)
	for _, request := range append([]requestCase{conflict}, additionalConflicts...) {
		recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", request.method, request.path, request.body, request.id, request.handle)
		require.Equal(t, http.StatusConflict, recorder.Code, recorder.Body.String())
		require.Empty(t, f.client.Calls)
	}
	for _, before := range []*cdbm.ExpectedMachine{reservedBefore, otherBefore} {
		after, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, before.ID, nil, false)
		require.NoError(t, err)
		require.Equal(t, before, after)
	}
	if independent != nil {
		run := &tmocks.WorkflowRun{}
		f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, mock.Anything, mock.Anything).Return(run, nil).Once()
		run.On("GetID").Return("independent-workflow").Maybe()
		run.On("Get", mock.Anything, mock.Anything).Return(nil).Once()
		recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", independent.method, independent.path, independent.body, independent.id, independent.handle)
		require.Equal(t, independent.status, recorder.Code, recorder.Body.String())
		if independent.check != nil {
			independent.check(t)
		}
		f.client.AssertExpectations(t)
		run.AssertExpectations(t)
	}
	pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 1)
	require.Len(t, pending[0].Inputs, 2)
	stored, err := cdbm.NewExpectedMachineDAO(f.session).Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
	require.NoError(t, err)
	require.Equal(t, input.ChassisSerialNumber, stored.ChassisSerialNumber)
}

func TestDeleteAllExpectedMachinesHandler_Handle(t *testing.T) {
	t.Run("pending creation conflicts with complete Site deletion", func(t *testing.T) { testExpectedMachinePendingOverlap(t, "delete all") })
}

type expectedMachineAPIRejectionHistory struct{ events []*historypb.HistoryEvent }

func (h *expectedMachineAPIRejectionHistory) HasNext() bool { return len(h.events) > 0 }
func (h *expectedMachineAPIRejectionHistory) Next() (*historypb.HistoryEvent, error) {
	event := h.events[0]
	h.events = h.events[1:]
	return event, nil
}

func testExpectedMachineLegacyBatch(t *testing.T) {
	t.Setenv(strings.ToUpper(config.ConfigExpectedMachineBatchRecoveryEnabled), "false")
	cfg := common.GetTestConfig()
	require.False(t, cfg.GetExpectedMachineBatchRecoveryEnabled())
	cases := []struct {
		name                  string
		resultErr             error
		wantStatus, wantCount int
	}{
		{name: "success commits the original synchronous transaction", wantStatus: http.StatusCreated, wantCount: 2},
		{name: "unknown result rolls back without creating a reservation", resultErr: errors.New("private legacy result error"), wantStatus: http.StatusInternalServerError},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newExpectedMachineAPIRecoveryFixture(t)
			var logs bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
			t.Cleanup(func() { log.Logger = previousLogger })
			run := &tmocks.WorkflowRun{}
			inserted := false
			f.session.DB.AddQueryHook(&expectedMachineQueryHook{queryPrefix: `INSERT INTO "expected_machine"`, armed: true, afterQuery: func(event *bun.QueryEvent) { require.NoError(t, event.Err); inserted = true }})
			var request *corev1.BatchExpectedMachineOperationRequest
			f.client.On("ExecuteWorkflow", mock.Anything, mock.Anything, "CreateExpectedMachines", mock.Anything).Run(func(args mock.Arguments) {
				request = args.Get(3).(*corev1.BatchExpectedMachineOperationRequest)
				require.True(t, inserted, "legacy creation inserts public rows before dispatch")
				pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
				require.NoError(t, err)
				require.Empty(t, pending)
				count, err := f.session.DB.NewSelect().Model((*cdbm.ExpectedMachine)(nil)).Count(f.ctx)
				require.NoError(t, err)
				require.Zero(t, count, "the legacy rows remain uncommitted while Core runs")
				probe, err := cdb.BeginTx(f.ctx, f.session, nil)
				require.NoError(t, err)
				defer func() { require.NoError(t, probe.Rollback()) }()
				var acquired bool
				err = probe.GetBunTx().NewSelect().ColumnExpr("pg_try_advisory_xact_lock_shared(?)", cdbm.ExpectedInventoryMutationLockID(f.site.ID)).Scan(f.ctx, &acquired)
				require.NoError(t, err)
				require.False(t, acquired)
			}).Return(run, nil).Once()
			run.On("Get", mock.Anything, mock.Anything).Run(func(args mock.Arguments) { fillExpectedMachineBatchSuccess(args.Get(1), request) }).Return(tc.resultErr).Once()
			body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:18","chassisSerialNumber":"legacy-first"},{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:19","chassisSerialNumber":"legacy-second"}]`, f.site.ID, f.site.ID)
			recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", NewCreateExpectedMachinesHandler(f.session, f.pool, cfg).Handle)
			require.Equal(t, tc.wantStatus, recorder.Code, recorder.Body.String())
			if tc.resultErr != nil {
				require.NotContains(t, logs.String(), "private legacy result error")
				require.NotContains(t, recorder.Body.String(), "private legacy result error")
				require.Contains(t, logs.String(), `"FailureCategory":"observation_failed"`)
			}
			count, err := f.session.DB.NewSelect().Model((*cdbm.ExpectedMachine)(nil)).Count(f.ctx)
			require.NoError(t, err)
			require.Equal(t, tc.wantCount, count)
			pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
			f.client.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}

type expectedMachineAdmissionDeadlineHook struct{ ctx context.Context }

func (h *expectedMachineAdmissionDeadlineHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if strings.Contains(event.Query, `FROM "site"`) && strings.Contains(event.Query, "FOR NO KEY UPDATE") {
		h.ctx = ctx
	}
	return ctx
}
func (*expectedMachineAdmissionDeadlineHook) AfterQuery(context.Context, *bun.QueryEvent) {}

func testExpectedMachineAdmissionRowDeadline(t *testing.T) {
	f := newExpectedMachineAPIRecoveryFixture(t)
	lock, err := cdb.BeginTx(f.ctx, f.session, nil)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, lock.Rollback()) })
	_, err = cdbm.NewSiteDAO(f.session).GetByID(f.ctx, lock, f.site.ID, nil, false, true)
	require.NoError(t, err)
	hook := &expectedMachineAdmissionDeadlineHook{}
	f.session.DB.AddQueryHook(hook)
	handler := NewCreateExpectedMachinesHandler(f.session, f.pool, expectedMachineBatchRecoveryConfig(t))
	handler.workflowTimeout = 200 * time.Millisecond
	body := fmt.Sprintf(`[{"siteId":%q,"bmcMacAddress":"00:11:22:33:44:18","chassisSerialNumber":"row-contention"}]`, f.site.ID)
	started := time.Now()
	recorder := invokeExpectedInventoryMutationHandler(t, f.user, "test-org", http.MethodPost, "/v2/org/test-org/nico/expected-machine/batch", body, "", handler.Handle)
	require.Equal(t, http.StatusInternalServerError, recorder.Code, recorder.Body.String())
	require.Less(t, time.Since(started), time.Second)
	require.NotNil(t, hook.ctx, "the request must reach the contended Site row")
	require.ErrorIs(t, hook.ctx.Err(), context.DeadlineExceeded)
	require.Empty(t, f.client.Calls)
	pending, err := cdbm.NewPendingExpectedMachineBatchDAO(f.session).GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Empty(t, pending)
	require.NoError(t, cdb.WithTx(f.ctx, f.session, func(tx *cdb.Tx) error {
		return tx.TryAcquireAdvisoryLock(f.ctx, cdbm.ExpectedInventoryMutationLockID(f.site.ID), &cdb.LockRetryOptions{Retries: cutil.GetPtr(1)})
	}))
}
