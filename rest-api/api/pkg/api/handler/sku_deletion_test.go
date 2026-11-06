// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
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
	"google.golang.org/protobuf/encoding/protojson"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	swe "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/error"
)

func setSkuDeletion(t *testing.T, session *cdb.Session, skuID string, deletion *cdbm.SkuDeletion) {
	t.Helper()
	ctx := context.Background()
	dao := cdbm.NewSkuDAO(session)
	err := cdb.WithTx(ctx, session, func(tx *cdb.Tx) error {
		sku, err := dao.GetForUpdate(ctx, tx, skuID)
		if err != nil {
			return err
		}
		applied, err := dao.SetDeletion(ctx, tx, skuID, sku.SiteID, nil, deletion)
		require.True(t, applied)
		return err
	})
	require.NoError(t, err)
}

type skuAttemptHistory struct{ events []*historypb.HistoryEvent }

func (h *skuAttemptHistory) HasNext() bool { return len(h.events) > 0 }
func (h *skuAttemptHistory) Next() (*historypb.HistoryEvent, error) {
	event := h.events[0]
	h.events = h.events[1:]
	return event, nil
}

func skuOriginalHistory(runID string, failed bool) *skuAttemptHistory {
	events := []*historypb.HistoryEvent{
		{EventId: 1, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_STARTED, Attributes: &historypb.HistoryEvent_WorkflowExecutionStartedEventAttributes{WorkflowExecutionStartedEventAttributes: &historypb.WorkflowExecutionStartedEventAttributes{WorkflowType: &commonpb.WorkflowType{Name: grpcproxy.Core.WorkflowName}, OriginalExecutionRunId: runID, FirstExecutionRunId: runID, Attempt: 1}}},
		{EventId: 2, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_WorkflowTaskScheduledEventAttributes{WorkflowTaskScheduledEventAttributes: &historypb.WorkflowTaskScheduledEventAttributes{}}},
		{EventId: 3, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_STARTED, Attributes: &historypb.HistoryEvent_WorkflowTaskStartedEventAttributes{WorkflowTaskStartedEventAttributes: &historypb.WorkflowTaskStartedEventAttributes{ScheduledEventId: 2}}},
		{EventId: 4, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_COMPLETED, Attributes: &historypb.HistoryEvent_WorkflowTaskCompletedEventAttributes{WorkflowTaskCompletedEventAttributes: &historypb.WorkflowTaskCompletedEventAttributes{ScheduledEventId: 2, StartedEventId: 3}}},
		{EventId: 5, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_ActivityTaskScheduledEventAttributes{ActivityTaskScheduledEventAttributes: &historypb.ActivityTaskScheduledEventAttributes{ActivityId: "delete", ActivityType: &commonpb.ActivityType{Name: "InvokeCoreGRPCOnSite"}}}},
		{EventId: 6, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_STARTED, Attributes: &historypb.HistoryEvent_ActivityTaskStartedEventAttributes{ActivityTaskStartedEventAttributes: &historypb.ActivityTaskStartedEventAttributes{ScheduledEventId: 5, Attempt: 1}}},
		{EventId: 7, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_COMPLETED, Attributes: &historypb.HistoryEvent_ActivityTaskCompletedEventAttributes{ActivityTaskCompletedEventAttributes: &historypb.ActivityTaskCompletedEventAttributes{ScheduledEventId: 5, StartedEventId: 6}}},
		{EventId: 8, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_SCHEDULED, Attributes: &historypb.HistoryEvent_WorkflowTaskScheduledEventAttributes{WorkflowTaskScheduledEventAttributes: &historypb.WorkflowTaskScheduledEventAttributes{}}},
		{EventId: 9, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_STARTED, Attributes: &historypb.HistoryEvent_WorkflowTaskStartedEventAttributes{WorkflowTaskStartedEventAttributes: &historypb.WorkflowTaskStartedEventAttributes{ScheduledEventId: 8}}},
		{EventId: 10, EventType: enumspb.EVENT_TYPE_WORKFLOW_TASK_COMPLETED, Attributes: &historypb.HistoryEvent_WorkflowTaskCompletedEventAttributes{WorkflowTaskCompletedEventAttributes: &historypb.WorkflowTaskCompletedEventAttributes{ScheduledEventId: 8, StartedEventId: 9}}},
		{EventId: 11, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_COMPLETED, Attributes: &historypb.HistoryEvent_WorkflowExecutionCompletedEventAttributes{WorkflowExecutionCompletedEventAttributes: &historypb.WorkflowExecutionCompletedEventAttributes{}}},
	}
	if failed {
		events[6] = &historypb.HistoryEvent{EventId: 7, EventType: enumspb.EVENT_TYPE_ACTIVITY_TASK_FAILED, Attributes: &historypb.HistoryEvent_ActivityTaskFailedEventAttributes{ActivityTaskFailedEventAttributes: &historypb.ActivityTaskFailedEventAttributes{ScheduledEventId: 5, StartedEventId: 6}}}
		events[10] = &historypb.HistoryEvent{EventId: 11, EventType: enumspb.EVENT_TYPE_WORKFLOW_EXECUTION_FAILED, Attributes: &historypb.HistoryEvent_WorkflowExecutionFailedEventAttributes{WorkflowExecutionFailedEventAttributes: &historypb.WorkflowExecutionFailedEventAttributes{}}}
	}
	return &skuAttemptHistory{events: events}
}

func skuAttemptDescription(runID string, status enumspb.WorkflowExecutionStatus) *workflowservice.DescribeWorkflowExecutionResponse {
	return &workflowservice.DescribeWorkflowExecutionResponse{WorkflowExecutionInfo: &workflowpb.WorkflowExecutionInfo{Type: &commonpb.WorkflowType{Name: grpcproxy.Core.WorkflowName}, Status: status, Execution: &commonpb.WorkflowExecution{RunId: runID}}}
}

func testSkuDeletionLostAck(t *testing.T) {
	f := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
	t.Setenv(strings.ToUpper(config.ConfigSkuDeletionRecoveryEnabled), "true")
	f.deleteHandler.cfg = common.GetTestConfig()
	stc := &tmocks.Client{}
	stc.On("ExecuteWorkflow", mock.Anything, mock.Anything, grpcproxy.Core.WorkflowName, mock.Anything).Return(nil, serviceerror.NewDeadlineExceeded("start acknowledgement lost")).Once()
	f.deleteHandler.scp.IDClientMap[f.siteID] = stc
	rec := f.request(t, http.MethodDelete, "sku-1", nil, f.deleteHandler.Handle)
	require.Equal(t, http.StatusGatewayTimeout, rec.Code, rec.Body.String())
	dao := cdbm.NewSkuDAO(f.deleteHandler.dbSession)
	stored, err := dao.Get(context.Background(), nil, "sku-1")
	require.NoError(t, err)
	require.NotNil(t, stored.Deletion)
	require.Nil(t, stored.Deletion.RunID)
	workflowID := stored.Deletion.WorkflowID
	stc.On("DescribeWorkflowExecution", mock.Anything, workflowID, "").Return(skuAttemptDescription("accepted-original", enumspb.WORKFLOW_EXECUTION_STATUS_COMPLETED), nil).Twice()
	stc.On("GetWorkflowHistory", mock.Anything, workflowID, "accepted-original", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(skuOriginalHistory("accepted-original", false)).Once()
	run := &tmocks.WorkflowRun{}
	stc.On("GetWorkflow", mock.Anything, workflowID, "accepted-original").Return(run).Once()
	run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
		persisted, err := dao.Get(context.Background(), nil, "sku-1")
		require.NoError(t, err)
		require.Equal(t, cutil.GetPtr("accepted-original"), persisted.Deletion.RunID, "binding must commit before observing the result")
		args.Get(1).(*grpcproxy.Response).ResponseJSON = []byte(`{}`)
	}).Return(nil).Once()
	t.Setenv(strings.ToUpper(config.ConfigSkuDeletionRecoveryEnabled), "false")
	restarted := NewDeleteSkuHandler(f.deleteHandler.dbSession, f.deleteHandler.scp, f.deleteHandler.cfg)
	rec = f.request(t, http.MethodDelete, "sku-1", nil, restarted.Handle)
	require.Equal(t, http.StatusNoContent, rec.Code, rec.Body.String())
	_, err = dao.Get(context.Background(), nil, "sku-1")
	require.ErrorIs(t, err, cdb.ErrDoesNotExist)
	stc.AssertNumberOfCalls(t, "ExecuteWorkflow", 1)
	stc.AssertExpectations(t)
	run.AssertExpectations(t)
}

func mockConclusiveSkuDelete(t *testing.T, f *skuManagementFixture, resultErr error, beforeResult func(string)) (*tmocks.Client, *tmocks.WorkflowRun) {
	t.Helper()
	stc, run := &tmocks.Client{}, &tmocks.WorkflowRun{}
	workflowID := ""
	stc.On("ExecuteWorkflow", mock.Anything, mock.MatchedBy(func(options tclient.StartWorkflowOptions) bool {
		workflowID = options.ID
		assert.Equal(t, enumspb.WORKFLOW_ID_REUSE_POLICY_REJECT_DUPLICATE, options.WorkflowIDReusePolicy)
		return true
	}), grpcproxy.Core.WorkflowName, mock.MatchedBy(func(req grpcproxy.Request) bool {
		assert.Equal(t, corev1.Forge_DeleteSku_FullMethodName, req.FullMethod)
		var payload corev1.SkuIdList
		require.NoError(t, protojson.Unmarshal(req.RequestJSON, &payload))
		assert.Equal(t, []string{"sku-1"}, payload.Ids)
		return true
	})).Return(run, nil).Once()
	run.On("GetRunID").Return("original-run").Once()
	run.On("GetWithOptions", mock.Anything, mock.Anything, tclient.WorkflowRunGetOptions{DisableFollowingRuns: true}).Run(func(args mock.Arguments) {
		args.Get(1).(*grpcproxy.Response).ResponseJSON = []byte(`{}`)
		if beforeResult != nil {
			beforeResult(workflowID)
		}
	}).Return(resultErr).Once()
	if resultErr != nil {
		stc.On("DescribeWorkflowExecution", mock.Anything, mock.MatchedBy(func(id string) bool { return id == workflowID }), "").Return(skuAttemptDescription("original-run", enumspb.WORKFLOW_EXECUTION_STATUS_FAILED), nil).Once()
		stc.On("GetWorkflowHistory", mock.Anything, mock.MatchedBy(func(id string) bool { return id == workflowID }), "original-run", false, enumspb.HISTORY_EVENT_FILTER_TYPE_ALL_EVENT).Return(skuOriginalHistory("original-run", true)).Once()
	}
	f.deleteHandler.scp.IDClientMap[f.siteID] = stc
	return stc, run
}

func testSkuDeletionCoreResults(t *testing.T) {
	cases := []struct {
		name, errorType string
		wantCode        int
		alreadyReleased bool
	}{
		{name: "success", wantCode: http.StatusNoContent},
		{name: "verified Core not found cleans stale record", errorType: swe.ErrTypeNICoObjectNotFound, wantCode: http.StatusNoContent},
		{name: "concurrently released rejection preserves original Core error", errorType: swe.ErrTypeNICoFailedPrecondition, wantCode: http.StatusPreconditionFailed, alreadyReleased: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
			t.Setenv(strings.ToUpper(config.ConfigSkuDeletionRecoveryEnabled), "true")
			f.deleteHandler.cfg = common.GetTestConfig()
			dao := cdbm.NewSkuDAO(f.deleteHandler.dbSession)
			var resultErr error
			if tc.errorType != "" {
				converter := temporal.GetDefaultFailureConverter()
				resultErr = converter.FailureToError(&failurepb.Failure{Cause: converter.ErrorToFailure(temporal.NewNonRetryableApplicationError("Core rejected deletion", tc.errorType, nil)), FailureInfo: &failurepb.Failure_ActivityFailureInfo{ActivityFailureInfo: &failurepb.ActivityFailureInfo{ActivityType: &commonpb.ActivityType{Name: "InvokeCoreGRPCOnSite"}, ActivityId: "delete", ScheduledEventId: 5, StartedEventId: 6}}})
			}
			stc, run := mockConclusiveSkuDelete(t, f, resultErr, func(workflowID string) {
				if tc.alreadyReleased {
					// Another observer can commit the same definitive rejection first.
					applied, err := dao.ResolveDeletion(context.Background(), "sku-1", uuid.MustParse(f.siteID), workflowID, false)
					require.NoError(t, err)
					require.True(t, applied)
				}
			})
			rec := f.request(t, http.MethodDelete, "sku-1", nil, f.deleteHandler.Handle)
			require.Equal(t, tc.wantCode, rec.Code, rec.Body.String())
			stored, err := dao.Get(context.Background(), nil, "sku-1")
			if tc.wantCode == http.StatusNoContent {
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			} else {
				require.NoError(t, err)
				assert.Nil(t, stored.Deletion)
				assert.Contains(t, rec.Body.String(), "Core rejected deletion")
			}
			stc.AssertExpectations(t)
			run.AssertExpectations(t)
		})
	}
}

func testSkuDeletionWaitsForAttachment(t *testing.T) {
	f := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	session := f.deleteHandler.dbSession
	gate, err := cdb.BeginTx(ctx, session, nil)
	require.NoError(t, err)
	var workers sync.WaitGroup
	defer func() { cancel(); _ = gate.Rollback(); workers.Wait() }()
	dao := cdbm.NewSkuDAO(session)
	admitted, err := dao.GetForAttachment(ctx, gate, uuid.MustParse(f.siteID), []string{"sku-1"})
	require.NoError(t, err)
	require.Len(t, admitted, 1)
	_, err = cdbm.NewExpectedMachineDAO(session).Create(ctx, gate, cdbm.ExpectedMachineCreateInput{ExpectedMachineID: uuid.New(), SiteID: uuid.MustParse(f.siteID), SkuID: cutil.GetPtr("sku-1"), BmcMacAddress: "00:11:22:33:44:55", ChassisSerialNumber: "earlier-admission", CreatedBy: f.user.ID})
	require.NoError(t, err)
	req := httptest.NewRequest(http.MethodDelete, "/", nil).WithContext(ctx)
	rec := httptest.NewRecorder()
	ec := echo.New().NewContext(req, rec)
	ec.SetParamNames("orgName", "id")
	ec.SetParamValues(f.org, "sku-1")
	ec.Set("user", f.user)
	done := make(chan error, 1)
	workers.Go(func() { done <- f.deleteHandler.Handle(ec) })
	require.Eventually(t, func() bool {
		var blocked int
		err := session.DB.NewRaw(`SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND query LIKE '%FROM "sku"%' AND query LIKE '%FOR UPDATE%' AND cardinality(pg_blocking_pids(pid)) > 0`).Scan(ctx, &blocked)
		return err == nil && blocked > 0
	}, 5*time.Second, 10*time.Millisecond, "DELETE must wait for the admitted reference transaction")
	require.NoError(t, gate.Commit())
	select {
	case err = <-done:
		require.NoError(t, err)
	case <-ctx.Done():
		t.Fatal("DELETE did not finish after attachment commit", ctx.Err())
	}
	require.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
	assert.Contains(t, rec.Body.String(), "referenced by Expected Machines")
	assert.Empty(t, f.requests)
	stored, err := dao.Get(context.Background(), nil, "sku-1")
	require.NoError(t, err)
	assert.Nil(t, stored.Deletion)
}

func testSkuDeletionPreparationRaces(t *testing.T) {
	cases := []struct {
		name     string
		replace  bool
		wantCode int
	}{
		{name: "concurrent removal returns not found", wantCode: http.StatusNotFound},
		{name: "captured attempt cannot adopt replacement attempt", replace: true, wantCode: http.StatusConflict},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
			session := f.deleteHandler.dbSession
			dao := cdbm.NewSkuDAO(session)
			if tc.replace {
				setSkuDeletion(t, session, "sku-1", &cdbm.SkuDeletion{WorkflowID: "captured-attempt"})
			}
			fired := false
			session.DB.AddQueryHook(skuQueryContextHook(func(ctx context.Context, event *bun.QueryEvent) {
				if fired || event.Operation() != "SELECT" || !strings.Contains(event.Query, `FROM "sku"`) || !strings.Contains(event.Query, "FOR UPDATE") {
					return
				}
				fired = true
				require.NoError(t, dao.Delete(ctx, nil, "sku-1"))
				if tc.replace {
					_, err := dao.Create(ctx, nil, cdbm.SkuCreateInput{SkuID: "sku-1", SiteID: uuid.MustParse(f.siteID), Description: "replacement"})
					require.NoError(t, err)
					setSkuDeletion(t, session, "sku-1", &cdbm.SkuDeletion{WorkflowID: "replacement-attempt"})
				}
			}))
			rec := f.request(t, http.MethodDelete, "sku-1", nil, f.deleteHandler.Handle)
			require.True(t, fired)
			require.Equal(t, tc.wantCode, rec.Code, rec.Body.String())
			assert.Empty(t, f.requests)
			stored, err := dao.Get(context.Background(), nil, "sku-1")
			if tc.replace {
				require.NoError(t, err)
				assert.Equal(t, "replacement-attempt", stored.Deletion.WorkflowID)
			} else {
				require.ErrorIs(t, err, cdb.ErrDoesNotExist)
			}
		})
	}
}

func testSkuDeletionLateCompletionPreservesReplacement(t *testing.T) {
	f := newSkuManagementFixture(t, []string{authz.ProviderAdminRole})
	t.Setenv(strings.ToUpper(config.ConfigSkuDeletionRecoveryEnabled), "true")
	f.deleteHandler.cfg = common.GetTestConfig()
	session := f.deleteHandler.dbSession
	dao := cdbm.NewSkuDAO(session)
	stc, run := mockConclusiveSkuDelete(t, f, nil, func(string) {
		require.NoError(t, dao.Delete(context.Background(), nil, "sku-1"))
		_, err := dao.Create(context.Background(), nil, cdbm.SkuCreateInput{SkuID: "sku-1", SiteID: uuid.MustParse(f.siteID), Description: "replacement"})
		require.NoError(t, err)
		setSkuDeletion(t, session, "sku-1", &cdbm.SkuDeletion{WorkflowID: "replacement-attempt"})
	})
	rec := f.request(t, http.MethodDelete, "sku-1", nil, f.deleteHandler.Handle)
	require.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
	stored, err := dao.Get(context.Background(), nil, "sku-1")
	require.NoError(t, err)
	assert.Equal(t, "replacement", stored.Description)
	require.NotNil(t, stored.Deletion)
	assert.Equal(t, "replacement-attempt", stored.Deletion.WorkflowID)
	assert.False(t, stored.Deletion.Confirmed)
	stc.AssertExpectations(t)
	run.AssertExpectations(t)
}
