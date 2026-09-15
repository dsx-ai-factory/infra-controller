// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func int32Ptr(v int32) *int32 { return &v }

// sampleRunCreateRequest returns a fully-populated create request that
// exercises every branch of the configuration tree.
func sampleRunCreateRequest() APITaskRunCreateRequest {
	ruleID := "rule-id"
	return APITaskRunCreateRequest{
		SiteID:      "site-id",
		Name:        "fw-rollout",
		Description: "desc",
		Selector: &APITaskRunSelector{
			Percentage: &APITaskRunPercentageSelector{Percent: 50, Seed: "seed-1"},
		},
		Options: APITaskRunOptions{
			MaxConcurrentTargets: 3,
			SafetyPolicy: &APITaskRunSafetyPolicy{
				Gates: []APITaskRunSafetyGate{
					{FailureRate: &APITaskRunFailureRateGate{Scope: "currentPhase", ThresholdPercent: 20}},
					{FailureCount: &APITaskRunFailureCountGate{Scope: "cumulativeRun", ThresholdCount: 5}},
				},
			},
			ConflictPolicy: &APITaskRunConflictPolicy{
				Retry: &APITaskRunConflictRetry{
					RetryTimeout:      "30m",
					InitialRetryDelay: "10s",
					MaxRetryDelay:     "5m",
				},
			},
			OrderingPolicy: &APITaskRunOrderingPolicy{
				Random: &APITaskRunRandomOrdering{Seed: "seed-2"},
			},
			PhasePolicy: &APITaskRunPhasePolicy{
				Equal:       &APITaskRunEqualPhases{PhaseCount: 4},
				AutoAdvance: true,
			},
		},
		Operation: APITaskRunOperation{
			Firmware: &APITaskRunFirmwareOperation{
				Version:                "1.2.3",
				RuleID:                 &ruleID,
				OverrideReadinessCheck: true,
				SubTargets:             []string{"bmc", "bios"},
			},
			ExcludeRunIDs: []string{"prev-1", "prev-2"},
		},
	}
}

func TestAPITaskRunCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		mutate  func(r *APITaskRunCreateRequest)
		wantErr string
	}{
		{name: "valid full", mutate: func(r *APITaskRunCreateRequest) {}},
		{
			name: "valid minimal",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Selector = nil
				r.Options = APITaskRunOptions{MaxConcurrentTargets: 1}
				r.Operation = APITaskRunOperation{Firmware: &APITaskRunFirmwareOperation{Version: "1.0.0"}}
			},
		},
		{
			name:    "missing siteId",
			mutate:  func(r *APITaskRunCreateRequest) { r.SiteID = "" },
			wantErr: "siteId is required",
		},
		{
			name:    "missing name",
			mutate:  func(r *APITaskRunCreateRequest) { r.Name = "" },
			wantErr: "name is required",
		},
		{
			name:    "non-positive maxConcurrentTargets",
			mutate:  func(r *APITaskRunCreateRequest) { r.Options.MaxConcurrentTargets = 0 },
			wantErr: "maxConcurrentTargets: must be greater than zero",
		},
		{
			name:    "negative maxConcurrentTargets",
			mutate:  func(r *APITaskRunCreateRequest) { r.Options.MaxConcurrentTargets = -1 },
			wantErr: "maxConcurrentTargets: must be greater than zero",
		},
		{
			name:    "missing firmware operation",
			mutate:  func(r *APITaskRunCreateRequest) { r.Operation.Firmware = nil },
			wantErr: "firmware: firmware is required",
		},
		{
			name:    "missing firmware version",
			mutate:  func(r *APITaskRunCreateRequest) { r.Operation.Firmware.Version = "" },
			wantErr: "version: version is required",
		},
		{
			name: "gate sets both",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Options.SafetyPolicy.Gates = []APITaskRunSafetyGate{{
					FailureRate:  &APITaskRunFailureRateGate{Scope: "currentPhase", ThresholdPercent: 10},
					FailureCount: &APITaskRunFailureCountGate{Scope: "currentPhase", ThresholdCount: 1},
				}}
			},
			wantErr: "must set exactly one of failureRate or failureCount",
		},
		{
			name: "gate sets neither",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Options.SafetyPolicy.Gates = []APITaskRunSafetyGate{{}}
			},
			wantErr: "must set exactly one of failureRate or failureCount",
		},
		{
			name: "invalid gate scope",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Options.SafetyPolicy.Gates = []APITaskRunSafetyGate{{
					FailureRate: &APITaskRunFailureRateGate{Scope: "bogus", ThresholdPercent: 10},
				}}
			},
			wantErr: "scope: must be one of",
		},
		{
			name: "unparseable conflict retry duration",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Options.ConflictPolicy.Retry.RetryTimeout = "not-a-duration"
			},
			wantErr: "retryTimeout: invalid duration",
		},
		{
			name: "empty conflict retry durations are allowed",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Options.ConflictPolicy.Retry = &APITaskRunConflictRetry{}
			},
		},
		{
			name: "phase policy sets more than one",
			mutate: func(r *APITaskRunCreateRequest) {
				r.Options.PhasePolicy = &APITaskRunPhasePolicy{
					Equal:      &APITaskRunEqualPhases{PhaseCount: 2},
					Percentage: &APITaskRunPercentagePhases{Phases: []int32{50, 50}},
				}
			},
			wantErr: "must set at most one of equal, percentage, count",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := sampleRunCreateRequest()
			tt.mutate(&req)
			err := req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}

func TestAPITaskRunCreateRequest_ToProto(t *testing.T) {
	req := sampleRunCreateRequest()

	pb := req.ToProto()
	require.NotNil(t, pb)
	assert.Equal(t, "fw-rollout", pb.GetName())
	assert.Equal(t, "desc", pb.GetDescription())

	cfg := pb.GetConfiguration()
	require.NotNil(t, cfg)

	// Selector.
	sel := cfg.GetSelector().GetPercentage()
	require.NotNil(t, sel)
	assert.Equal(t, int32(50), sel.GetPercentage())
	assert.Equal(t, "seed-1", sel.GetSeed())

	// Options.
	opts := cfg.GetOptions()
	require.NotNil(t, opts)
	assert.Equal(t, int32(3), opts.GetMaxConcurrentTargets())

	// Safety gates: rate + count, with scope mapping.
	gates := opts.GetSafetyPolicy().GetGates()
	require.Len(t, gates, 2)
	rate := gates[0].GetFailureRate()
	require.NotNil(t, rate)
	assert.Equal(t, int32(20), rate.GetFailureThresholdPercent())
	assert.Equal(t, flowv1.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CURRENT_PHASE, rate.GetScope())
	count := gates[1].GetFailureCount()
	require.NotNil(t, count)
	assert.Equal(t, int32(5), count.GetFailureThresholdCount())
	assert.Equal(t, flowv1.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CUMULATIVE_RUN, count.GetScope())

	// Conflict retry durations.
	retry := opts.GetConflictPolicy().GetRetry()
	require.NotNil(t, retry)
	assert.Equal(t, 30*time.Minute, retry.GetRetryTimeout().AsDuration())
	assert.Equal(t, 10*time.Second, retry.GetInitialRetryDelay().AsDuration())
	assert.Equal(t, 5*time.Minute, retry.GetMaxRetryDelay().AsDuration())

	// Ordering.
	assert.Equal(t, "seed-2", opts.GetOrderingPolicy().GetRandom().GetSeed())

	// Phase policy: equal + auto-advance.
	phase := opts.GetPhasePolicy()
	require.NotNil(t, phase)
	assert.Equal(t, int32(4), phase.GetEqual().GetPhaseCount())
	assert.True(t, phase.GetAdvancePolicy().GetAutoAdvance())

	// Operation.
	fw := cfg.GetOperation().GetUpgradeFirmware()
	require.NotNil(t, fw)
	assert.Equal(t, "1.2.3", fw.GetTargetVersion())
	assert.Equal(t, "rule-id", fw.GetRuleId().GetId())
	assert.True(t, fw.GetOverrideReadinessCheck())
	assert.Equal(t, []string{"bmc", "bios"}, fw.GetSubTargets())

	// Target scope excludes.
	excludes := cfg.GetOperation().GetTargetScope().GetExcludeOperationRunIds()
	require.Len(t, excludes, 2)
	assert.Equal(t, "prev-1", excludes[0].GetId())
	assert.Equal(t, "prev-2", excludes[1].GetId())
}

func TestAPITaskRunCreateRequest_ToProto_OmitsUnsetRuleID(t *testing.T) {
	req := sampleRunCreateRequest()
	req.Operation.Firmware.RuleID = stringPtr("")

	pb := req.ToProto()
	assert.Nil(t, pb.GetConfiguration().GetOperation().GetUpgradeFirmware().GetRuleId())
}

// Empty durations mean "use the operation default", which Flow expresses as an
// absent field rather than a zero duration.
func TestAPITaskRunCreateRequest_ToProto_OmitsUnsetDurations(t *testing.T) {
	req := sampleRunCreateRequest()
	req.Options.ConflictPolicy.Retry = &APITaskRunConflictRetry{}

	retry := req.ToProto().GetConfiguration().GetOptions().GetConflictPolicy().GetRetry()
	require.NotNil(t, retry)
	assert.Nil(t, retry.GetRetryTimeout())
	assert.Nil(t, retry.GetInitialRetryDelay())
	assert.Nil(t, retry.GetMaxRetryDelay())
}

func TestAPITaskRunCreateRequest_ToProto_PhasePercentageAndCount(t *testing.T) {
	req := sampleRunCreateRequest()

	req.Options.PhasePolicy = &APITaskRunPhasePolicy{Percentage: &APITaskRunPercentagePhases{Phases: []int32{30, 70}}}
	pb := req.ToProto()
	pct := pb.GetConfiguration().GetOptions().GetPhasePolicy().GetPercentage().GetPhases()
	require.Len(t, pct, 2)
	assert.Equal(t, int32(30), pct[0].GetPercentage())
	assert.Equal(t, int32(70), pct[1].GetPercentage())

	req.Options.PhasePolicy = &APITaskRunPhasePolicy{Count: &APITaskRunCountPhases{Phases: []int32{2, 3}}}
	pb = req.ToProto()
	cnt := pb.GetConfiguration().GetOptions().GetPhasePolicy().GetCount().GetPhases()
	require.Len(t, cnt, 2)
	assert.Equal(t, int32(2), cnt[0].GetCount())
	assert.Equal(t, int32(3), cnt[1].GetCount())
}

func TestAPITaskRun_FromProto(t *testing.T) {
	created := time.Date(2026, 6, 6, 12, 0, 0, 0, time.UTC)
	updated := created.Add(time.Hour)
	started := created.Add(2 * time.Minute)

	run := &flowv1.OperationRun{
		Summary: &flowv1.OperationRunSummary{
			Id:          &flowv1.UUID{Id: "run-id"},
			Name:        "fw-rollout",
			Description: "desc",
			OperationKind: &flowv1.OperationKind{
				Type: flowv1.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL,
				Code: stringPtr("upgrade"),
			},
			State: &flowv1.OperationRunState{
				Status: flowv1.OperationRunStatus_OPERATION_RUN_STATUS_PAUSED,
				Reason: flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_PHASE_GATE,
			},
			StatusMessage: "paused at phase gate",
			TotalPhases:   4,
			CreatedAt:     timestamppb.New(created),
			UpdatedAt:     timestamppb.New(updated),
			StartedAt:     timestamppb.New(started),
		},
		Stats: &flowv1.OperationRunStats{
			CurrentPhaseStats: &flowv1.OperationRunPhaseStats{
				PhaseIndex:      1,
				SelectedTargets: 6,
				OutcomeCounts: &flowv1.OperationRunTargetOutcomeCounts{
					Completed: 3, Failed: 1, Terminated: 0, Skipped: 2,
				},
			},
			CumulativePhaseStats: &flowv1.OperationRunPhaseStats{
				PhaseIndex:      1,
				SelectedTargets: 12,
			},
		},
	}

	got := &APITaskRun{}
	got.FromProto(run)
	assert.Equal(t, "run-id", got.ID)
	assert.Equal(t, "fw-rollout", got.Name)
	assert.Equal(t, "desc", got.Description)
	assert.Equal(t, APIOperationTypeFirmwareControl, got.OperationType)
	assert.Equal(t, "upgrade", got.OperationCode)
	assert.Equal(t, TaskRunStatusPaused, got.Status)
	assert.Equal(t, TaskRunStatusReasonPhaseGate, got.StatusReason)
	assert.Equal(t, "paused at phase gate", got.StatusMessage)
	assert.Equal(t, int32(4), got.TotalPhases)
	assert.Equal(t, created, got.Created)
	assert.Equal(t, updated, got.Updated)
	require.NotNil(t, got.Started)
	assert.Equal(t, started, *got.Started)
	assert.Nil(t, got.Finished)

	require.NotNil(t, got.Stats)
	assert.Equal(t, int32(1), got.Stats.CurrentPhase.PhaseIndex)
	assert.Equal(t, int32(6), got.Stats.CurrentPhase.SelectedTargets)
	assert.Equal(t, int32(3), got.Stats.CurrentPhase.OutcomeCounts.Completed)
	assert.Equal(t, int32(2), got.Stats.CurrentPhase.OutcomeCounts.Skipped)
	assert.Equal(t, int32(12), got.Stats.CumulativePhase.SelectedTargets)
}

func TestAPITaskRun_FromProtoSummary_NoStats(t *testing.T) {
	got := &APITaskRun{}
	got.FromProtoSummary(&flowv1.OperationRunSummary{
		Id:    &flowv1.UUID{Id: "run-id"},
		Name:  "fw-rollout",
		State: &flowv1.OperationRunState{Status: flowv1.OperationRunStatus_OPERATION_RUN_STATUS_RUNNING},
	})
	assert.Equal(t, "run-id", got.ID)
	assert.Equal(t, TaskRunStatusRunning, got.Status)
	assert.Nil(t, got.Stats)
}

func TestAPITaskRun_FromProto_NilSafe(t *testing.T) {
	got := &APITaskRun{}
	assert.NotPanics(t, func() { got.FromProto(nil) })
	assert.NotPanics(t, func() { got.FromProtoSummary(nil) })
}

func TestAPITaskRunTarget_FromProto(t *testing.T) {
	created := time.Date(2026, 6, 6, 12, 0, 0, 0, time.UTC)

	target := &flowv1.OperationRunTarget{
		Id:             &flowv1.UUID{Id: "target-id"},
		OperationRunId: &flowv1.UUID{Id: "run-id"},
		RackId:         &flowv1.UUID{Id: "flow-rack-uuid"},
		RackExternalId: "core-rack-01",
		SequenceIndex:  2,
		PhaseIndex:     1,
		TaskId:         &flowv1.UUID{Id: "task-id"},
		Status:         flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SUBMITTED,
		Message:        "submitted",
		CreatedAt:      timestamppb.New(created),
		UpdatedAt:      timestamppb.New(created),
	}

	got := &APITaskRunTarget{}
	got.FromProto(target)
	assert.Equal(t, "target-id", got.ID)
	assert.Equal(t, "run-id", got.RunID)
	assert.Equal(t, "core-rack-01", got.RackID)
	assert.Equal(t, int32(2), got.SequenceIndex)
	assert.Equal(t, int32(1), got.PhaseIndex)
	require.NotNil(t, got.TaskID)
	assert.Equal(t, "task-id", *got.TaskID)
	assert.Equal(t, TaskRunTargetStatusSubmitted, got.Status)
	assert.Equal(t, "submitted", got.Message)
}

func TestAPITaskRunTarget_FromProto_UnsetTaskID(t *testing.T) {
	got := &APITaskRunTarget{}
	got.FromProto(&flowv1.OperationRunTarget{
		Id:     &flowv1.UUID{Id: "target-id"},
		TaskId: &flowv1.UUID{Id: ""},
		Status: flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_PENDING,
	})
	assert.Nil(t, got.TaskID)
	assert.Equal(t, TaskRunTargetStatusPending, got.Status)
}

func TestAPITaskRunGetRequest_Validate(t *testing.T) {
	require.Error(t, (&APITaskRunGetRequest{}).Validate())
	require.NoError(t, (&APITaskRunGetRequest{SiteID: "site-id"}).Validate())
}

func TestAPITaskRunSiteRequest_Validate(t *testing.T) {
	require.Error(t, (&APITaskRunSiteRequest{}).Validate())
	require.NoError(t, (&APITaskRunSiteRequest{SiteID: "site-id"}).Validate())
}

func TestAPITaskRunAdvanceRequest_Validate(t *testing.T) {
	require.Error(t, (&APITaskRunAdvanceRequest{}).Validate())
	require.NoError(t, (&APITaskRunAdvanceRequest{SiteID: "site-id"}).Validate())
	require.NoError(t, (&APITaskRunAdvanceRequest{SiteID: "site-id", ExpectedPhaseIndex: int32Ptr(2)}).Validate())
}

func TestAPITaskRunCancelRequest_Validate(t *testing.T) {
	require.Error(t, (&APITaskRunCancelRequest{}).Validate())
	require.NoError(t, (&APITaskRunCancelRequest{SiteID: "site-id"}).Validate())
}

func TestAPITaskRunGetAllRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APITaskRunGetAllRequest
		wantErr string
	}{
		{name: "valid no filters", req: APITaskRunGetAllRequest{SiteID: "site-id"}},
		{
			name: "valid with filters",
			req:  APITaskRunGetAllRequest{SiteID: "site-id", Status: "Running", OperationType: APIOperationTypeFirmwareControl},
		},
		{name: "missing siteId", req: APITaskRunGetAllRequest{}, wantErr: "siteId"},
		{name: "invalid status", req: APITaskRunGetAllRequest{SiteID: "site-id", Status: "bogus"}, wantErr: "status must be one of"},
		{name: "invalid operationType", req: APITaskRunGetAllRequest{SiteID: "site-id", OperationType: "bogus"}, wantErr: "operationType must be one of"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}

func TestAPITaskRunGetAllRequest_ToProto(t *testing.T) {
	pageNum, pageSize := 2, 10
	req := APITaskRunGetAllRequest{SiteID: "site-id", Status: "Paused", OperationType: APIOperationTypeFirmwareControl}
	// Validate derives Offset/Limit, which is what the handler does before it
	// reaches ToProto.
	page := pagination.PageRequest{PageNumber: &pageNum, PageSize: &pageSize}
	require.NoError(t, page.Validate(nil))

	pb, err := req.ToProto(page)
	require.NoError(t, err)
	require.NotNil(t, pb.GetFilter())
	states := pb.GetFilter().GetStates()
	require.Len(t, states, 1)
	assert.Equal(t, flowv1.OperationRunStatus_OPERATION_RUN_STATUS_PAUSED, states[0].GetStatus())
	kinds := pb.GetFilter().GetOperationKinds()
	require.Len(t, kinds, 1)
	assert.Equal(t, flowv1.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL, kinds[0].GetType())
	require.NotNil(t, pb.GetPagination())
	assert.Equal(t, int32(10), pb.GetPagination().GetLimit())
	assert.Equal(t, int32(10), pb.GetPagination().GetOffset()) // (2-1)*10
}

func TestAPITaskRunGetAllRequest_QueryValues(t *testing.T) {
	pageNum, pageSize := 1, 50
	req := APITaskRunGetAllRequest{SiteID: "site-id", Status: "Running", OperationType: APIOperationTypeFirmwareControl}
	page := pagination.PageRequest{PageNumber: &pageNum, PageSize: &pageSize}

	v := req.QueryValues(page)
	assert.Equal(t, "site-id", v.Get("siteId"))
	assert.Equal(t, "Running", v.Get("status"))
	assert.Equal(t, string(APIOperationTypeFirmwareControl), v.Get("operationType"))
	assert.Equal(t, "1", v.Get("pageNumber"))
	assert.Equal(t, "50", v.Get("pageSize"))
}

func TestAPITaskRunTargetGetAllRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		req     APITaskRunTargetGetAllRequest
		wantErr string
	}{
		{name: "valid no filters", req: APITaskRunTargetGetAllRequest{SiteID: "site-id"}},
		{
			name: "valid with filters",
			req:  APITaskRunTargetGetAllRequest{SiteID: "site-id", Status: TaskRunTargetStatusFailed, PhaseScope: "completedPhases"},
		},
		{name: "missing siteId", req: APITaskRunTargetGetAllRequest{}, wantErr: "siteId"},
		{name: "invalid status", req: APITaskRunTargetGetAllRequest{SiteID: "site-id", Status: "bogus"}, wantErr: "status must be one of"},
		{name: "invalid phaseScope", req: APITaskRunTargetGetAllRequest{SiteID: "site-id", PhaseScope: "bogus"}, wantErr: "phaseScope must be one of"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr == "" {
				assert.NoError(t, err)
				return
			}
			require.Error(t, err)
			assert.Contains(t, err.Error(), tt.wantErr)
		})
	}
}

func TestAPITaskRunTargetGetAllRequest_ToProto(t *testing.T) {
	pageNum, pageSize := 3, 20
	req := APITaskRunTargetGetAllRequest{SiteID: "site-id", Status: TaskRunTargetStatusFailed, PhaseScope: "completedPhases"}
	page := pagination.PageRequest{PageNumber: &pageNum, PageSize: &pageSize}
	require.NoError(t, page.Validate(nil))

	pb := req.ToProto("run-id", page)
	assert.Equal(t, "run-id", pb.GetOperationRunId().GetId())
	assert.Equal(t, flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_FAILED, pb.GetStatus())
	assert.Equal(t, flowv1.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_COMPLETED_PHASES, pb.GetPhaseScope())
	require.NotNil(t, pb.GetPagination())
	assert.Equal(t, int32(20), pb.GetPagination().GetLimit())
	assert.Equal(t, int32(40), pb.GetPagination().GetOffset()) // (3-1)*20
}

func TestAPITaskRunTargetGetAllRequest_ToProto_DefaultsPhaseScope(t *testing.T) {
	req := APITaskRunTargetGetAllRequest{SiteID: "site-id"}
	pb := req.ToProto("run-id", pagination.PageRequest{})
	assert.Equal(t, flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_UNKNOWN, pb.GetStatus())
	assert.Equal(t, flowv1.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_PHASE, pb.GetPhaseScope())
}
