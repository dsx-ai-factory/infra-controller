// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package expectedmachine

import (
	"bytes"
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	commonpb "go.temporal.io/api/common/v1"
	enumspb "go.temporal.io/api/enums/v1"
	failurepb "go.temporal.io/api/failure/v1"
	historypb "go.temporal.io/api/history/v1"
	"go.temporal.io/api/serviceerror"
	workflowpb "go.temporal.io/api/workflow/v1"
	workflowservice "go.temporal.io/api/workflowservice/v1"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/temporal"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
	sc "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
	cwu "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
)

type pendingMachineRecoveryFixture struct {
	ctx      context.Context
	session  *cdb.Session
	site     *cdbm.Site
	batch    cdbm.PendingExpectedMachineBatch
	pending  cdbm.PendingExpectedMachineBatchDAO
	machines cdbm.ExpectedMachineDAO
	client   *tmocks.Client
	pool     *sc.ClientPool
}

func newPendingMachineRecoveryFixture(t *testing.T) *pendingMachineRecoveryFixture {
	t.Helper()
	ctx := context.Background()
	session := testExpectedMachineInitDB(t)
	t.Cleanup(session.Close)
	testExpectedMachineSetupSchema(t, session)
	user := cwu.TestBuildUser(t, session, uuid.NewString(), []string{"test-org"}, []string{"FORGE_PROVIDER_ADMIN"})
	provider := cwu.TestBuildInfrastructureProvider(t, session, "pending-provider", "test-org", user)
	site := cwu.TestBuildSite(t, session, provider, "pending-site", cdbm.SiteStatusRegistered, nil, user)
	batch := cdbm.PendingExpectedMachineBatch{WorkflowID: "create-expected-machines-" + uuid.NewString(), SiteID: site.ID, Created: time.Now().Add(-time.Minute), Inputs: []cdbm.ExpectedMachineCreateInput{
		{ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:01", ChassisSerialNumber: "reserved-first", Name: cutil.GetPtr("original name"), Manufacturer: cutil.GetPtr("original manufacturer"), Description: cutil.GetPtr("original description"), Labels: map[string]string{"request": "original"}, CreatedBy: user.ID},
		{ExpectedMachineID: uuid.New(), SiteID: site.ID, BmcMacAddress: "00:11:22:33:44:02", ChassisSerialNumber: "reserved-second", CreatedBy: user.ID},
	}}
	pending := cdbm.NewPendingExpectedMachineBatchDAO(session)
	require.NoError(t, pending.Create(ctx, nil, batch))
	client := &tmocks.Client{}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[site.ID.String()] = client
	return &pendingMachineRecoveryFixture{ctx: ctx, session: session, site: site, batch: batch, pending: pending, machines: cdbm.NewExpectedMachineDAO(session), client: client, pool: pool}
}

func expectedMachineExecution(status enumspb.WorkflowExecutionStatus) *workflowservice.DescribeWorkflowExecutionResponse {
	return &workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Status: status, Execution: &commonpb.WorkflowExecution{RunId: "recorded-run"}}}
}

func expectedMachineBatchSuccess(batch cdbm.PendingExpectedMachineBatch) []*corev1.ExpectedMachineOperationResult {
	results := make([]*corev1.ExpectedMachineOperationResult, 0, len(batch.Inputs))
	for _, input := range batch.Inputs {
		results = append(results, &corev1.ExpectedMachineOperationResult{Id: &corev1.UUID{Value: input.ExpectedMachineID.String()}, Success: true})
	}
	return results
}

func expectedMachineActivityFailure(errorType string) error {
	converter := temporal.GetDefaultFailureConverter()
	return converter.FailureToError(&failurepb.Failure{Message: "private activity message", Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("private application message", errorType, nil)), FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}, ActivityId: "batch-create", ScheduledEventId: 10, StartedEventId: 12}}})
}

func testExpectedMachinePendingRecovery(t *testing.T) {
	tests := []struct {
		name string
		run  func(*testing.T, *pendingMachineRecoveryFixture)
	}{
		{name: "complete inventory resolves all members before unavailable history", run: func(t *testing.T, f *pendingMachineRecoveryFixture) {
			older := cdbm.PendingExpectedMachineBatch{WorkflowID: "unavailable-history", SiteID: f.site.ID, Created: time.Now().Add(-2 * time.Minute), Inputs: []cdbm.ExpectedMachineCreateInput{{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:03", ChassisSerialNumber: "unobserved", CreatedBy: f.batch.Inputs[0].CreatedBy}}}
			require.NoError(t, f.pending.Create(f.ctx, nil, older))
			f.client.On("DescribeWorkflowExecution", mock.Anything, older.WorkflowID, "").Run(func(mock.Arguments) {
				remaining, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
				require.NoError(t, err)
				require.Len(t, remaining, 1, "positive inventory must finalize before unavailable history is observed")
				require.Equal(t, older.WorkflowID, remaining[0].WorkflowID)
			}).Return(nil, serviceerror.NewNotFound("private history message")).Once()
			first := f.batch.Inputs[0].ToExpectedMachine()
			first.Name = cutil.GetPtr("newer Core name")
			first.ChassisSerialNumber = "reported serial"
			first.Description, first.Manufacturer, first.Labels = nil, nil, nil
			second := f.batch.Inputs[1].ToExpectedMachine()
			firstProto := first.ToProto(cdbm.ExpectedMachineCredentials{})
			firstProto.Metadata.Labels = nil
			inventory := &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, ExpectedMachines: []*corev1.ExpectedMachine{firstProto, second.ToProto(cdbm.ExpectedMachineCredentials{})}}
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, inventory))
			stored, err := f.machines.Get(f.ctx, nil, first.ID, nil, false)
			require.NoError(t, err)
			require.Equal(t, "reported serial", stored.ChassisSerialNumber)
			require.Equal(t, first.Name, stored.Name)
			require.Equal(t, f.batch.Inputs[0].Manufacturer, stored.Manufacturer)
			require.Equal(t, f.batch.Inputs[0].Description, stored.Description)
			require.Equal(t, cdbm.Labels(f.batch.Inputs[0].Labels), stored.Labels)
			require.Equal(t, f.batch.Inputs[0].CreatedBy, stored.CreatedBy)
		}},
		{name: "failed inventory still observes a definitive original-run rejection", run: func(t *testing.T, f *pendingMachineRecoveryFixture) {
			require.NoError(t, f.pending.BindRunID(f.ctx, f.site.ID, f.batch.WorkflowID, "recorded-run"))
			f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "recorded-run").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_FAILED), nil).Once()
			f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_FAILED), nil).Once()
			run := &tmocks.WorkflowRun{}
			f.client.On("GetWorkflow", mock.Anything, f.batch.WorkflowID, "recorded-run").Return(run).Once()
			run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Return(expectedMachineActivityFailure(swe.ErrTypeNICoAlreadyExists)).Once()
			f.client.On("GetWorkflowHistory", mock.Anything, f.batch.WorkflowID, "recorded-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(pendingMachineAttemptHistory(1)).Once()
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}))
			pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
			for _, input := range f.batch.Inputs {
				_, err = f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			}
			run.AssertExpectations(t)
		}},
		{name: "acknowledged offline original closes before scheduling any activity", run: func(t *testing.T, f *pendingMachineRecoveryFixture) {
			require.NoError(t, f.pending.BindRunID(f.ctx, f.site.ID, f.batch.WorkflowID, "recorded-run"))
			execution := expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_TIMED_OUT)
			f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "recorded-run").Return(execution, nil).Once()
			f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(execution, nil).Once()
			f.client.On("GetWorkflowHistory", mock.Anything, f.batch.WorkflowID, "recorded-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(&pendingMachineRejectionHistory{events: []*historypb.HistoryEvent{
				{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{WorkflowType: &commonpb.WorkflowType{Name: "CreateExpectedMachines"}, OriginalExecutionRunId: "recorded-run", FirstExecutionRunId: "recorded-run", Attempt: 1}}},
				{EventId: 2, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_TIMED_OUT, Attributes: &historypb.HistoryEvent_WorkflowExecutionTimedOutEventAttributes{WorkflowExecutionTimedOutEventAttributes: &historypb.WorkflowExecutionTimedOutEventAttributes{}}},
			}}).Once()
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}))
			pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
			for _, input := range f.batch.Inputs {
				_, err := f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			}
		}},
		{name: "one reported member cannot confirm a multi-member batch", run: func(t *testing.T, f *pendingMachineRecoveryFixture) {
			f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(nil, serviceerror.NewNotFound("private history message")).Once()
			first := f.batch.Inputs[0].ToExpectedMachine()
			inventory := &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, ExpectedMachines: []*corev1.ExpectedMachine{first.ToProto(cdbm.ExpectedMachineCredentials{})}}
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, inventory))
			pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Len(t, pending, 1)
			require.Len(t, pending[0].Inputs, 2)
			_, err = f.machines.Get(f.ctx, nil, f.batch.Inputs[1].ExpectedMachineID, nil, false)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			inventory.ExpectedMachines = nil
			inventory.InventoryPage = &corev1.InventoryPage{CurrentPage: 2, TotalPages: 2, TotalItems: 2, ItemIds: []string{first.ID.String(), f.batch.Inputs[1].ExpectedMachineID.String()}}
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, inventory))
			pending, err = f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
		}},
		{name: "unknown absence preserves a member and a fresh worker creates the missing member", run: func(t *testing.T, f *pendingMachineRecoveryFixture) {
			_, err := f.machines.Create(f.ctx, nil, f.batch.Inputs[0])
			require.NoError(t, err)
			unrelatedID := uuid.New()
			_, err = f.machines.Create(f.ctx, nil, cdbm.ExpectedMachineCreateInput{ExpectedMachineID: unrelatedID, SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:03", ChassisSerialNumber: "stale", CreatedBy: f.batch.Inputs[0].CreatedBy})
			require.NoError(t, err)
			_, err = f.session.DB.ExecContext(f.ctx, "UPDATE expected_machine SET updated = ?", time.Now().Add(-2*cutil.DefaultInventoryReceiptInterval))
			require.NoError(t, err)
			f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Run(func(args mock.Arguments) {
				deadline, bounded := args.Get(0).(context.Context).Deadline()
				require.True(t, bounded)
				require.LessOrEqual(t, time.Until(deadline), pendingBatchObservationTimeout)
				pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
				require.NoError(t, err)
				require.NotNil(t, pending[0].LastObserved, "the marker must commit before observation")
				require.NoError(t, cdb.WithTx(f.ctx, f.session, func(tx *cdb.Tx) error {
					return tx.TryAcquireAdvisoryLock(f.ctx, cdbm.ExpectedInventoryMutationLockID(f.site.ID), nil)
				}))
			}).Return(nil, serviceerror.NewNotFound("private history message")).Once()
			inventory := &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, inventory))
			_, err = f.machines.Get(f.ctx, nil, f.batch.Inputs[0].ExpectedMachineID, nil, false)
			require.NoError(t, err)
			_, err = f.machines.Get(f.ctx, nil, unrelatedID, nil, false)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			_, err = f.machines.Get(f.ctx, nil, f.batch.Inputs[1].ExpectedMachineID, nil, false)
			require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			freshClient, freshRun := &tmocks.Client{}, &tmocks.WorkflowRun{}
			freshPool := sc.NewClientPool(nil)
			freshPool.IDClientMap[f.site.ID.String()] = freshClient
			freshClient.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED), nil).Once()
			freshClient.On("GetWorkflow", mock.Anything, f.batch.WorkflowID, "recorded-run").Return(freshRun).Once()
			freshRun.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
				args.Get(1).(*corev1.BatchExpectedMachineOperationResponse).Results = expectedMachineBatchSuccess(f.batch)
			}).Return(nil).Once()
			inventory.InventoryStatus = corev1.InventoryStatus_INVENTORY_STATUS_UNSPECIFIED
			require.NoError(t, NewManageExpectedMachine(f.session, freshPool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, inventory))
			pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Empty(t, pending)
			for _, input := range f.batch.Inputs {
				stored, err := f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
				require.NoError(t, err)
				require.Equal(t, input.CreatedBy, stored.CreatedBy)
			}
			freshClient.AssertExpectations(t)
			freshRun.AssertExpectations(t)
		}},
		{name: "running and ambiguous fetched histories retain the group", run: testPendingMachineAmbiguousHistory},
		{name: "history budget rotates work durably across workers", run: testPendingMachineHistoryFairness},
		{name: "one resolution conflict does not block the next group", run: testPendingMachineIndependentResolution},
		{name: "completion fences a stale inventory deletion snapshot", run: testPendingMachineCompletionFencesSnapshot},
		{name: "stale deletion stops after one contended Site lock", run: testPendingMachineStaleDeletionContention},
		{name: "maximum batch completes from history with bounded SQL latency", run: testPendingMachineMaximumCompletion},
		{name: "positive completion failures rotate durably within the recovery budget", run: testPendingMachinePositiveFairness},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			f := newPendingMachineRecoveryFixture(t)
			test.run(t, f)
			f.client.AssertExpectations(t)
		})
	}
	t.Run("Site deletion after the inventory read prevents import and update", testPendingMachineSiteDeletionAfterRead)
}

func testPendingMachineAmbiguousHistory(t *testing.T, f *pendingMachineRecoveryFixture) {
	cases := []struct {
		name           string
		status         enumspb.WorkflowExecutionStatus
		resultErr      error
		results        []*corev1.ExpectedMachineOperationResult
		historyAttempt int32
		missingRun     bool
		originalRunID  string
		describedRunID string
		latestRunID    string
		wantCategory   string
	}{
		{name: "running history does not fetch a result", status: enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING},
		{name: "a missing described run cannot fetch latest results", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, missingRun: true},
		{name: "malformed fetched success remains unknown", status: enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED, results: []*corev1.ExpectedMachineOperationResult{{Id: &corev1.UUID{Value: f.batch.Inputs[0].ExpectedMachineID.String()}, Success: true}, {Id: &corev1.UUID{Value: f.batch.Inputs[0].ExpectedMachineID.String()}, Success: true}}},
		{name: "fetched transport failure remains unknown", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, resultErr: expectedMachineActivityFailure(swe.ErrTypeNICoUnavailable), wantCategory: "execution_failed"},
		{name: "a latest first-attempt rejection cannot release an unbound original run", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, resultErr: expectedMachineActivityFailure(swe.ErrTypeNICoInvalidArgument)},
		{name: "a fetched rejection after an earlier activity attempt remains unknown", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, resultErr: expectedMachineActivityFailure(swe.ErrTypeNICoInvalidArgument), historyAttempt: 2, originalRunID: "recorded-run"},
		{name: "a replacement described run cannot release the bound original", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, originalRunID: "recorded-run", describedRunID: "replacement-run"},
		{name: "an original rejection cannot release while a replacement is running", status: enumspb.WORKFLOW_EXECUTION_STATUS_FAILED, resultErr: expectedMachineActivityFailure(swe.ErrTypeNICoInvalidArgument), originalRunID: "recorded-run", latestRunID: "replacement-run"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			client, run := &tmocks.Client{}, &tmocks.WorkflowRun{}
			pool := sc.NewClientPool(nil)
			pool.IDClientMap[f.site.ID.String()] = client
			if tc.originalRunID != "" {
				require.NoError(t, f.pending.BindRunID(f.ctx, f.site.ID, f.batch.WorkflowID, tc.originalRunID))
			}
			execution := expectedMachineExecution(tc.status)
			if tc.describedRunID != "" {
				execution.WorkflowExecutionInfo.Execution.RunId = tc.describedRunID
			}
			if tc.missingRun {
				execution.WorkflowExecutionInfo.Execution.RunId = ""
			}
			client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, tc.originalRunID).Return(execution, nil).Once()
			if tc.status != enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING && !tc.missingRun && tc.describedRunID == "" {
				client.On("GetWorkflow", mock.Anything, f.batch.WorkflowID, "recorded-run").Return(run).Once()
				run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
					args.Get(1).(*corev1.BatchExpectedMachineOperationResponse).Results = tc.results
				}).Return(tc.resultErr).Once()
			}
			if tc.historyAttempt != 0 {
				client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(execution, nil).Once()
				client.On("GetWorkflowHistory", mock.Anything, f.batch.WorkflowID, "recorded-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(pendingMachineAttemptHistory(tc.historyAttempt)).Once()
			}
			if tc.latestRunID != "" {
				latest := expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING)
				latest.WorkflowExecutionInfo.Execution.RunId = tc.latestRunID
				client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(latest, nil).Once()
			}
			var logs bytes.Buffer
			previousLogger := log.Logger
			log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
			t.Cleanup(func() { log.Logger = previousLogger })
			require.NoError(t, NewManageExpectedMachine(f.session, pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}))
			if tc.wantCategory != "" {
				require.Contains(t, logs.String(), `"FailureCategory":"`+tc.wantCategory+`"`)
				require.NotContains(t, logs.String(), "private activity message")
				require.NotContains(t, logs.String(), "private application message")
				require.NotContains(t, logs.String(), swe.ErrTypeNICoUnavailable)
			}
			pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Len(t, pending, 1)
			require.Len(t, pending[0].Inputs, 2)
			for _, input := range f.batch.Inputs {
				_, err = f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			}
			client.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}

func testPendingMachineHistoryFairness(t *testing.T, f *pendingMachineRecoveryFixture) {
	later := cdbm.PendingExpectedMachineBatch{WorkflowID: "later-confirmed", SiteID: f.site.ID, Inputs: []cdbm.ExpectedMachineCreateInput{{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:03", ChassisSerialNumber: "later", CreatedBy: f.batch.Inputs[0].CreatedBy}}}
	require.NoError(t, f.pending.Create(f.ctx, nil, later))
	f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Run(func(args mock.Arguments) { <-args.Get(0).(context.Context).Done() }).Return(nil, context.DeadlineExceeded).Once()
	activity := NewManageExpectedMachine(f.session, f.pool)
	activity.historyObservationBudget = 200 * time.Millisecond
	failed := &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}
	require.NoError(t, activity.UpdateExpectedMachinesInDB(f.ctx, f.site.ID, failed))
	pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 2)
	require.Equal(t, later.WorkflowID, pending[0].WorkflowID)
	require.Nil(t, pending[0].LastObserved)
	require.NotNil(t, pending[1].LastObserved)
	client, run := &tmocks.Client{}, &tmocks.WorkflowRun{}
	pool := sc.NewClientPool(nil)
	pool.IDClientMap[f.site.ID.String()] = client
	client.On("DescribeWorkflowExecution", mock.Anything, later.WorkflowID, "").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED), nil).Once()
	client.On("GetWorkflow", mock.Anything, later.WorkflowID, "recorded-run").Return(run).Once()
	run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
		args.Get(1).(*corev1.BatchExpectedMachineOperationResponse).Results = expectedMachineBatchSuccess(later)
	}).Return(nil).Once()
	client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(nil, serviceerror.NewNotFound("private history message")).Once()
	require.NoError(t, NewManageExpectedMachine(f.session, pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, failed))
	pending, err = f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 1)
	require.Equal(t, f.batch.WorkflowID, pending[0].WorkflowID)
	_, err = f.machines.Get(f.ctx, nil, later.Inputs[0].ExpectedMachineID, nil, false)
	require.NoError(t, err)
	client.AssertExpectations(t)
	run.AssertExpectations(t)
}

type pendingMachineLockAttemptHook struct {
	prefix   string
	after    func()
	attempts int
}

func (*pendingMachineLockAttemptHook) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	return ctx
}
func (h *pendingMachineLockAttemptHook) AfterQuery(_ context.Context, event *bun.QueryEvent) {
	if strings.HasPrefix(event.Query, h.prefix) {
		h.attempts++
		if h.after != nil {
			after := h.after
			h.after = nil
			after()
		}
	}
}

func testPendingMachineIndependentResolution(t *testing.T, f *pendingMachineRecoveryFixture) {
	later := cdbm.PendingExpectedMachineBatch{WorkflowID: "independent-confirmed", SiteID: f.site.ID, Inputs: []cdbm.ExpectedMachineCreateInput{{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:03", ChassisSerialNumber: "independent", CreatedBy: f.batch.Inputs[0].CreatedBy}}}
	require.NoError(t, f.pending.Create(f.ctx, nil, later))
	lock, err := cdb.BeginTx(f.ctx, f.session, nil)
	require.NoError(t, err)
	lockID := cdbm.ExpectedInventoryMutationLockID(f.site.ID)
	require.NoError(t, lock.TryAcquireSharedAdvisoryLock(f.ctx, lockID, nil))
	released := false
	t.Cleanup(func() {
		if !released {
			require.NoError(t, lock.Rollback())
		}
	})
	hook := &pendingMachineLockAttemptHook{prefix: fmt.Sprintf("SELECT pg_try_advisory_xact_lock(%d)", lockID), after: func() { require.NoError(t, lock.Rollback()); released = true }}
	f.session.DB.AddQueryHook(hook)
	for _, batch := range []cdbm.PendingExpectedMachineBatch{f.batch, later} {
		run := &tmocks.WorkflowRun{}
		f.client.On("DescribeWorkflowExecution", mock.Anything, batch.WorkflowID, "").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED), nil).Once()
		f.client.On("GetWorkflow", mock.Anything, batch.WorkflowID, "recorded-run").Return(run).Once()
		run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
			args.Get(1).(*corev1.BatchExpectedMachineOperationResponse).Results = expectedMachineBatchSuccess(batch)
		}).Return(nil).Once()
		t.Cleanup(func() { run.AssertExpectations(t) })
	}
	require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}))
	pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 1)
	require.Equal(t, f.batch.WorkflowID, pending[0].WorkflowID)
	_, err = f.machines.Get(f.ctx, nil, later.Inputs[0].ExpectedMachineID, nil, false)
	require.NoError(t, err)
	require.Equal(t, 2, hook.attempts)
}

func testPendingMachineCompletionFencesSnapshot(t *testing.T, f *pendingMachineRecoveryFixture) {
	input := f.batch.Inputs[0]
	imported, err := f.machines.Create(f.ctx, nil, cdbm.ExpectedMachineCreateInput{ExpectedMachineID: input.ExpectedMachineID, SiteID: f.site.ID, BmcMacAddress: input.BmcMacAddress, ChassisSerialNumber: input.ChassisSerialNumber, CreatedBy: f.site.ID})
	require.NoError(t, err)
	oldUpdated := time.Now().Add(-2 * cutil.DefaultInventoryReceiptInterval)
	_, err = f.session.DB.ExecContext(f.ctx, "UPDATE expected_machine SET updated = ? WHERE id = ?", oldUpdated, imported.ID)
	require.NoError(t, err)
	completed := false
	hook := &testExpectedMachineReconcileHook{afterRead: func() {
		require.NoError(t, cdb.WithTx(f.ctx, f.session, func(tx *cdb.Tx) error {
			err := tx.TryAcquireAdvisoryLock(f.ctx, cdbm.ExpectedInventoryMutationLockID(f.site.ID), nil)
			if err != nil {
				return err
			}
			_, err = f.pending.Complete(f.ctx, tx, f.site.ID, f.batch.WorkflowID)
			return err
		}))
		completed = true
	}}
	f.session.DB.AddQueryHook(hook)
	activityCtx := context.WithValue(f.ctx, testExpectedMachineReconcileContextKey{}, hook)
	require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(activityCtx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}))
	require.True(t, completed)
	stored, err := f.machines.Get(f.ctx, nil, imported.ID, nil, false)
	require.NoError(t, err, "the older inventory snapshot cannot delete the completed member")
	require.True(t, stored.Updated.After(oldUpdated))
	require.Equal(t, input.Name, stored.Name)
	require.Equal(t, input.CreatedBy, stored.CreatedBy)
	pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Empty(t, pending)
}

func testPendingMachineStaleDeletionContention(t *testing.T, f *pendingMachineRecoveryFixture) {
	for _, input := range f.batch.Inputs {
		_, err := f.machines.Create(f.ctx, nil, input)
		require.NoError(t, err)
	}
	_, err := f.session.DB.ExecContext(f.ctx, "UPDATE expected_machine SET updated = ?", time.Now().Add(-2*cutil.DefaultInventoryReceiptInterval))
	require.NoError(t, err)
	lock, err := cdb.BeginTx(f.ctx, f.session, nil)
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, lock.Rollback()) })
	lockID := cdbm.ExpectedInventoryMutationLockID(f.site.ID)
	require.NoError(t, lock.TryAcquireAdvisoryLock(f.ctx, lockID, nil))
	hook := &pendingMachineLockAttemptHook{prefix: fmt.Sprintf("SELECT pg_try_advisory_xact_lock_shared(%d)", lockID)}
	f.session.DB.AddQueryHook(hook)
	var logs bytes.Buffer
	previousLogger := log.Logger
	log.Logger = zerolog.New(zerolog.SyncWriter(&logs))
	t.Cleanup(func() { log.Logger = previousLogger })
	f.client.On("DescribeWorkflowExecution", mock.Anything, f.batch.WorkflowID, "").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_RUNNING), nil).Once()
	require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS}))
	require.Equal(t, 1, hook.attempts)
	require.Contains(t, logs.String(), "deferring stale Expected Machine deletion after Site inventory lock contention")
	require.Contains(t, logs.String(), `"level":"warn"`)
	for _, input := range f.batch.Inputs {
		_, err := f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
		require.NoError(t, err)
	}
}

type pendingMachineRejectionHistory struct{ events []*historypb.HistoryEvent }

func (h *pendingMachineRejectionHistory) HasNext() bool { return len(h.events) > 0 }
func (h *pendingMachineRejectionHistory) Next() (*historypb.HistoryEvent, error) {
	event := h.events[0]
	h.events = h.events[1:]
	return event, nil
}

func pendingMachineAttemptHistory(attempt int32) *pendingMachineRejectionHistory {
	return &pendingMachineRejectionHistory{events: []*historypb.HistoryEvent{
		{EventId: 10, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{ActivityId: "batch-create", ActivityType: &commonpb.ActivityType{Name: "CreateExpectedMachinesOnSite"}}}},
		{EventId: 12, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{ScheduledEventId: 10, Attempt: attempt}}},
	}}
}

type pendingMachineCompletionHook struct {
	enabled  bool
	delay    time.Duration
	blockID  uuid.UUID
	blocked  bool
	calls    int
	firstCtx context.Context
}

func (h *pendingMachineCompletionHook) BeforeQuery(ctx context.Context, event *bun.QueryEvent) context.Context {
	if !h.enabled || (!strings.Contains(event.Query, `FROM "expected_machine"`) && !strings.HasPrefix(event.Query, `INSERT INTO "expected_machine"`) && !strings.HasPrefix(event.Query, `UPDATE "expected_machine"`)) {
		return ctx
	}
	h.calls++
	if h.firstCtx == nil {
		h.firstCtx = ctx
	}
	if h.blockID != uuid.Nil && strings.Contains(event.Query, h.blockID.String()) {
		h.blocked = true
		<-ctx.Done()
		return ctx
	}
	if h.delay > 0 {
		timer := time.NewTimer(h.delay)
		defer timer.Stop()
		select {
		case <-timer.C:
		case <-ctx.Done():
		}
	}
	return ctx
}
func (*pendingMachineCompletionHook) AfterQuery(context.Context, *bun.QueryEvent) {}

func testPendingMachineMaximumCompletion(t *testing.T, f *pendingMachineRecoveryFixture) {
	batch := f.batch
	for i := len(batch.Inputs); i < 100; i++ {
		batch.Inputs = append(batch.Inputs, cdbm.ExpectedMachineCreateInput{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: fmt.Sprintf("02:00:00:00:00:%02x", i), ChassisSerialNumber: fmt.Sprintf("maximum-%03d", i), CreatedBy: f.batch.Inputs[0].CreatedBy})
	}
	require.NoError(t, cdb.WithTx(f.ctx, f.session, func(tx *cdb.Tx) error {
		if err := f.pending.Delete(f.ctx, tx, f.site.ID, batch.WorkflowID); err != nil {
			return err
		}
		return f.pending.Create(f.ctx, tx, batch)
	}))
	run := &tmocks.WorkflowRun{}
	f.client.On("DescribeWorkflowExecution", mock.Anything, batch.WorkflowID, "").Return(expectedMachineExecution(enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED), nil).Once()
	f.client.On("GetWorkflow", mock.Anything, batch.WorkflowID, "recorded-run").Return(run).Once()
	var observationCtx context.Context
	run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
		observationCtx = args.Get(0).(context.Context)
		args.Get(1).(*corev1.BatchExpectedMachineOperationResponse).Results = expectedMachineBatchSuccess(batch)
	}).Return(nil).Once()
	hook := &pendingMachineCompletionHook{enabled: true, delay: 5 * time.Millisecond}
	f.session.DB.AddQueryHook(hook)
	require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(f.ctx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_FAILED}))
	hook.enabled = false
	require.GreaterOrEqual(t, hook.calls, 300, "all member lookups, inserts and rereads must incur the controlled round-trip delay")
	require.NotSame(t, observationCtx, hook.firstCtx)
	observationDeadline, ok := observationCtx.Deadline()
	require.True(t, ok)
	completionDeadline, ok := hook.firstCtx.Deadline()
	require.True(t, ok)
	require.Greater(t, completionDeadline.Sub(observationDeadline), 5*time.Second)
	pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Empty(t, pending)
	var machines []cdbm.ExpectedMachine
	require.NoError(t, f.session.DB.NewSelect().Model(&machines).Where("site_id = ?", f.site.ID).Scan(f.ctx))
	require.Len(t, machines, 100)
	remaining := make(map[uuid.UUID]bool, 100)
	for _, input := range batch.Inputs {
		remaining[input.ExpectedMachineID] = true
	}
	for _, machine := range machines {
		require.True(t, remaining[machine.ID])
		delete(remaining, machine.ID)
		require.Equal(t, batch.Inputs[0].CreatedBy, machine.CreatedBy)
	}
	require.Empty(t, remaining)
	run.AssertExpectations(t)
}

func testPendingMachinePositiveFairness(t *testing.T, f *pendingMachineRecoveryFixture) {
	later := cdbm.PendingExpectedMachineBatch{WorkflowID: "later-positive", SiteID: f.site.ID, Inputs: []cdbm.ExpectedMachineCreateInput{{ExpectedMachineID: uuid.New(), SiteID: f.site.ID, BmcMacAddress: "00:11:22:33:44:03", ChassisSerialNumber: "later-positive", CreatedBy: f.batch.Inputs[0].CreatedBy}}}
	require.NoError(t, f.pending.Create(f.ctx, nil, later))
	reported := make(map[uuid.UUID]bool)
	for _, input := range append(f.batch.Inputs, later.Inputs...) {
		reported[input.ExpectedMachineID] = true
	}
	hook := &pendingMachineCompletionHook{enabled: true, blockID: f.batch.Inputs[0].ExpectedMachineID}
	f.session.DB.AddQueryHook(hook)
	activity := NewManageExpectedMachine(f.session, nil)
	activity.recoveryBudget = 200 * time.Millisecond
	require.NoError(t, activity.reconcilePendingBatches(f.ctx, log.Logger, f.site.ID, reported, true), "its own exhausted budget must defer without failing inventory")
	require.True(t, hook.blocked)
	pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 2)
	require.Equal(t, later.WorkflowID, pending[0].WorkflowID)
	require.NotNil(t, pending[1].LastObserved, "failed positive completion must rotate durably")
	fresh := NewManageExpectedMachine(f.session, nil)
	fresh.recoveryBudget = 200 * time.Millisecond
	require.NoError(t, fresh.reconcilePendingBatches(f.ctx, log.Logger, f.site.ID, reported, true))
	pending, err = f.pending.GetAll(f.ctx, nil, f.site.ID)
	require.NoError(t, err)
	require.Len(t, pending, 1)
	require.Equal(t, f.batch.WorkflowID, pending[0].WorkflowID)
	_, err = f.machines.Get(f.ctx, nil, later.Inputs[0].ExpectedMachineID, nil, false)
	require.NoError(t, err)
}

func testPendingMachineSiteDeletionAfterRead(t *testing.T) {
	cases := []struct {
		name     string
		existing bool
	}{
		{name: "pending-only member cannot be imported after Site deletion"},
		{name: "existing member cannot be updated after Site deletion", existing: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newPendingMachineRecoveryFixture(t)
			input := f.batch.Inputs[0]
			var before *cdbm.ExpectedMachine
			if tc.existing {
				_, err := f.machines.Create(f.ctx, nil, input)
				require.NoError(t, err)
				_, err = f.session.DB.ExecContext(f.ctx, "UPDATE expected_machine SET updated = ? WHERE id = ?", time.Now().Add(-2*cutil.DefaultInventoryReceiptInterval), input.ExpectedMachineID)
				require.NoError(t, err)
				before, err = f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
				require.NoError(t, err)
			}
			fired := false
			hook := &testExpectedMachineReconcileHook{afterRead: func() { fired = true; require.NoError(t, cdbm.NewSiteDAO(f.session).Delete(f.ctx, nil, f.site.ID)) }}
			f.session.DB.AddQueryHook(hook)
			reported := input.ToExpectedMachine()
			reported.Name = cutil.GetPtr("late inventory update")
			activityCtx := context.WithValue(f.ctx, testExpectedMachineReconcileContextKey{}, hook)
			require.NoError(t, NewManageExpectedMachine(f.session, f.pool).UpdateExpectedMachinesInDB(activityCtx, f.site.ID, &corev1.ExpectedMachineInventory{InventoryStatus: corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, ExpectedMachines: []*corev1.ExpectedMachine{reported.ToProto(cdbm.ExpectedMachineCredentials{})}}))
			require.True(t, fired)
			stored, err := f.machines.Get(f.ctx, nil, input.ExpectedMachineID, nil, false)
			if tc.existing {
				require.NoError(t, err)
				require.Equal(t, before, stored)
			} else {
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			}
			pending, err := f.pending.GetAll(f.ctx, nil, f.site.ID)
			require.NoError(t, err)
			require.Len(t, pending, 1)
			require.Len(t, pending[0].Inputs, 2)
			require.Empty(t, f.client.Calls)
		})
	}
}
