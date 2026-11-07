// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/firmwareauth"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	operationrun "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun"
	operationrunmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/operationrun/manager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

var _ operationrunmanager.Manager = (*mockOperationRunManager)(nil)

func TestCreateOperationRunCallsManager(t *testing.T) {
	createdID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{createID: createdID}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.CreateOperationRun(
		context.Background(),
		validCreateOperationRunRequest(),
	)
	require.NoError(t, err)

	require.Equal(t, createdID.String(), resp.GetId().GetId())
	require.Equal(t, 1, manager.createCalls)
	require.NotNil(t, manager.createdRun)
	require.Equal(t, "firmware canary", manager.createdRun.Name)
	require.Equal(t, operationrun.OperationRunStatusPending, manager.createdRun.Status)
	require.NotEmpty(t, manager.createdRun.Selector)
	require.NotEmpty(t, manager.createdRun.Options)
	require.NotEmpty(t, manager.createdRun.OperationTemplate)
}

func TestCreateOperationRunEncryptsFirmwareAuthenticationData(t *testing.T) {
	manager := &mockOperationRunManager{createID: uuid.New()}
	cipher := newServiceTestCipher(t)
	server := &FlowServerImpl{
		operationRunManager: manager,
		dataCipher:          cipher,
	}
	req := validCreateOperationRunRequest()
	authenticationData := "shared-token"
	req.Configuration.Operation.GetUpgradeFirmware().AuthenticationData =
		sharedServiceAuthenticationData(authenticationData)

	_, err := server.CreateOperationRun(context.Background(), req)

	require.NoError(t, err)
	require.NotContains(
		t,
		string(manager.createdRun.OperationTemplate),
		authenticationData,
	)
	var operationTemplate operationrun.Operation
	require.NoError(
		t,
		operationrun.UnmarshalConfig(
			manager.createdRun.OperationTemplate,
			&operationTemplate,
		),
	)
	firmwareInfo, ok := operationTemplate.Payload.(*operations.FirmwareControlTaskInfo)
	require.True(t, ok)
	got, err := firmwareauth.DecryptFor(
		cipher,
		firmwareInfo.AuthenticationData,
		devicetypes.ComponentTypeCompute,
	)
	require.NoError(t, err)
	require.Equal(t, authenticationData, got)
}

func TestCreateOperationRunAuthenticationDataStatusCodes(t *testing.T) {
	tests := []struct {
		name               string
		cipher             *secret.Cipher
		authenticationData *pb.FirmwareAuthenticationData
		subTargets         []string
		wantCode           codes.Code
	}{
		{
			name:               "invalid input",
			cipher:             newServiceTestCipher(t),
			authenticationData: &pb.FirmwareAuthenticationData{},
			wantCode:           codes.InvalidArgument,
		},
		{
			name:               "missing cipher",
			authenticationData: sharedServiceAuthenticationData("token"),
			wantCode:           codes.FailedPrecondition,
		},
		{
			name:               "authentication with dpu-only subtargets",
			cipher:             newServiceTestCipher(t),
			authenticationData: sharedServiceAuthenticationData("token"),
			subTargets:         []string{"dpu"},
			wantCode:           codes.InvalidArgument,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := validCreateOperationRunRequest()
			req.Configuration.Operation.GetUpgradeFirmware().AuthenticationData =
				tt.authenticationData
			req.Configuration.Operation.GetUpgradeFirmware().SubTargets =
				tt.subTargets
			manager := &mockOperationRunManager{}
			server := &FlowServerImpl{
				operationRunManager: manager,
				dataCipher:          tt.cipher,
			}

			_, err := server.CreateOperationRun(context.Background(), req)

			require.Equal(t, tt.wantCode, status.Code(err))
			require.Zero(t, manager.createCalls)
		})
	}
}

func TestCreateOperationRunRejectsInvalidRequest(t *testing.T) {
	manager := &mockOperationRunManager{}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.CreateOperationRun(
		context.Background(),
		&pb.CreateOperationRunRequest{},
	)

	require.Nil(t, resp)
	require.Equal(t, codes.InvalidArgument, status.Code(err))
	require.Equal(t, 0, manager.createCalls)
}

func TestCreateOperationRunRequiresManager(t *testing.T) {
	server := &FlowServerImpl{}

	resp, err := server.CreateOperationRun(
		context.Background(),
		validCreateOperationRunRequest(),
	)

	require.Nil(t, resp)
	require.Equal(t, codes.FailedPrecondition, status.Code(err))
}

func TestCreateOperationRunReturnsManagerError(t *testing.T) {
	manager := &mockOperationRunManager{
		createErr: errors.New("planning failed"),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.CreateOperationRun(
		context.Background(),
		validCreateOperationRunRequest(),
	)

	require.Nil(t, resp)
	require.Equal(t, codes.Internal, status.Code(err))
	require.ErrorContains(t, err, "planning failed")
}

func TestCreateOperationRunMapsManagerInvalidArgumentErrors(t *testing.T) {
	tests := []struct {
		name    string
		err     error
		message string
	}{
		{
			name:    "no planned targets",
			err:     operationrunmanager.ErrNoPlannedTargets,
			message: "operation run has no planned targets",
		},
		{
			name:    "phase has no targets",
			err:     operationrunmanager.ErrOperationRunInvalidPlan,
			message: "operation run plan is invalid",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			manager := &mockOperationRunManager{createErr: tt.err}
			server := &FlowServerImpl{operationRunManager: manager}

			resp, err := server.CreateOperationRun(
				context.Background(),
				validCreateOperationRunRequest(),
			)

			require.Nil(t, resp)
			require.Equal(t, codes.InvalidArgument, status.Code(err))
			require.ErrorContains(t, err, tt.message)
		})
	}
}

func TestCreateOperationRunPreservesManagerStatusError(t *testing.T) {
	manager := &mockOperationRunManager{
		createErr: status.Error(codes.InvalidArgument, "invalid target scope"),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.CreateOperationRun(
		context.Background(),
		validCreateOperationRunRequest(),
	)

	require.Nil(t, resp)
	require.Equal(t, codes.InvalidArgument, status.Code(err))
	require.ErrorContains(t, err, "invalid target scope")
}

func TestGetOperationRunReturnsDetailedRun(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{
		getRun: testOperationRun(t, runID),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.GetOperationRun(
		context.Background(),
		&pb.GetOperationRunRequest{Id: protobuf.UUIDTo(runID)},
	)
	require.NoError(t, err)

	require.Equal(t, 1, manager.getCalls)
	require.Equal(t, runID, manager.getID)
	require.Equal(t, runID.String(), resp.GetOperationRun().GetSummary().GetId().GetId())
	require.NotNil(t, resp.GetOperationRun().GetConfiguration())
	require.Nil(t, resp.GetOperationRun().GetStats())
}

func TestGetOperationRunIncludesStatsWhenRequested(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	targets := []*operationrun.OperationRunTarget{
		testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusCompleted),
		testOperationRunTarget(runID, 0, operationrun.OperationRunTargetStatusFailed),
	}
	for range dbquery.DefaultPaginationLimit - len(targets) {
		targets = append(
			targets,
			testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusSubmitted),
		)
	}
	targets = append(
		targets,
		testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusTerminated),
	)
	manager := &mockOperationRunManager{
		getRun:           testOperationRun(t, runID),
		listTargets:      targets,
		listTargetsTotal: int32(len(targets)),
		pageListTargets:  true,
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.GetOperationRun(
		context.Background(),
		&pb.GetOperationRunRequest{
			Id:           protobuf.UUIDTo(runID),
			IncludeStats: true,
		},
	)
	require.NoError(t, err)

	require.Equal(t, 2, manager.listTargetsCalls)
	require.Equal(t, runID, manager.listTargetsID)
	require.Equal(
		t,
		operationrun.TargetPhaseScopeCurrentAndCompletedPhases,
		manager.listTargetsOpts.PhaseScope,
	)
	require.Len(t, manager.listTargetsOptsHistory, 2)
	require.EqualValues(t, 0, manager.listTargetsOptsHistory[0].Pagination.Offset)
	require.EqualValues(t, dbquery.DefaultPaginationLimit, manager.listTargetsOptsHistory[0].Pagination.Limit)
	require.EqualValues(t, dbquery.DefaultPaginationLimit, manager.listTargetsOptsHistory[1].Pagination.Offset)
	require.EqualValues(t, dbquery.DefaultPaginationLimit, manager.listTargetsOptsHistory[1].Pagination.Limit)
	stats := resp.GetOperationRun().GetStats()
	require.EqualValues(t, 1, stats.GetCurrentPhaseStats().GetPhaseIndex())
	require.EqualValues(t, dbquery.DefaultPaginationLimit-1, stats.GetCurrentPhaseStats().GetSelectedTargets())
	require.EqualValues(t, 1, stats.GetCurrentPhaseStats().GetOutcomeCounts().GetTerminated())
	require.EqualValues(t, len(targets), stats.GetCumulativePhaseStats().GetSelectedTargets())
	require.EqualValues(t, 1, stats.GetCumulativePhaseStats().GetOutcomeCounts().GetCompleted())
	require.EqualValues(t, 1, stats.GetCumulativePhaseStats().GetOutcomeCounts().GetFailed())
	require.EqualValues(t, 1, stats.GetCumulativePhaseStats().GetOutcomeCounts().GetTerminated())
}

func TestGetOperationRunRejectsInvalidID(t *testing.T) {
	manager := &mockOperationRunManager{}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.GetOperationRun(
		context.Background(),
		&pb.GetOperationRunRequest{},
	)

	require.Nil(t, resp)
	require.Equal(t, codes.InvalidArgument, status.Code(err))
	require.Equal(t, 0, manager.getCalls)
}

func TestGetOperationRunMapsMissingRunToNotFound(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{
		getErr: fmt.Errorf("%w: %s", operationrunmanager.ErrOperationRunNotFound, runID),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.GetOperationRun(
		context.Background(),
		&pb.GetOperationRunRequest{Id: protobuf.UUIDTo(runID)},
	)

	require.Nil(t, resp)
	require.Equal(t, codes.NotFound, status.Code(err))
	require.ErrorContains(t, err, "operation run not found")
	require.ErrorContains(t, err, runID.String())
	require.Equal(t, 1, manager.getCalls)
}

func TestListOperationRunsReturnsSummaries(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	statusFilter := pb.OperationRunStatus_OPERATION_RUN_STATUS_RUNNING
	manager := &mockOperationRunManager{
		listRuns:  []*operationrun.OperationRun{testOperationRun(t, runID)},
		listTotal: 3,
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.ListOperationRuns(
		context.Background(),
		&pb.ListOperationRunsRequest{
			Filter: &pb.OperationRunFilter{
				States: []*pb.OperationRunStateFilter{
					{Status: &statusFilter},
				},
			},
			Pagination: &pb.Pagination{Offset: 5, Limit: 10},
		},
	)
	require.NoError(t, err)

	require.Equal(t, 1, manager.listCalls)
	require.EqualValues(t, 5, manager.listOpts.Pagination.Offset)
	require.EqualValues(t, 10, manager.listOpts.Pagination.Limit)
	require.Equal(t, operationrun.OperationRunStatusRunning, manager.listOpts.States[0].Status)
	require.EqualValues(t, 3, resp.GetTotal())
	require.Len(t, resp.GetOperationRuns(), 1)
	require.Equal(t, runID.String(), resp.GetOperationRuns()[0].GetId().GetId())
}

func TestListOperationRunsRejectsInvalidFilter(t *testing.T) {
	manager := &mockOperationRunManager{}
	server := &FlowServerImpl{operationRunManager: manager}
	statusFilter := pb.OperationRunStatus(999)

	resp, err := server.ListOperationRuns(
		context.Background(),
		&pb.ListOperationRunsRequest{
			Filter: &pb.OperationRunFilter{
				States: []*pb.OperationRunStateFilter{
					{Status: &statusFilter},
				},
			},
		},
	)

	require.Nil(t, resp)
	require.Equal(t, codes.InvalidArgument, status.Code(err))
	require.Equal(t, 0, manager.listCalls)
}

func TestListOperationRunTargetsReturnsTargets(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	target := testOperationRunTarget(runID, 1, operationrun.OperationRunTargetStatusBlocked)
	manager := &mockOperationRunManager{
		listTargets: []*operationrun.OperationRunTarget{
			target,
		},
		listTargetsTotal: 2,
	}
	inventory := newMockManager()
	inventory.racks[target.RackID] = &rack.Rack{
		Info:       deviceinfo.DeviceInfo{ID: target.RackID},
		ExternalID: "rack-01",
	}
	server := &FlowServerImpl{
		operationRunManager: manager,
		inventoryManager:    inventory,
	}

	resp, err := server.ListOperationRunTargets(
		context.Background(),
		&pb.ListOperationRunTargetsRequest{
			OperationRunId: protobuf.UUIDTo(runID),
			Status:         pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_BLOCKED,
			PhaseScope:     pb.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_COMPLETED_PHASES,
			Pagination:     &pb.Pagination{Offset: 1, Limit: 2},
		},
	)
	require.NoError(t, err)

	require.Equal(t, 1, manager.listTargetsCalls)
	require.Equal(t, runID, manager.listTargetsID)
	require.Equal(t, operationrun.OperationRunTargetStatusBlocked, manager.listTargetsOpts.Status)
	require.Equal(
		t,
		operationrun.TargetPhaseScopeCompletedPhases,
		manager.listTargetsOpts.PhaseScope,
	)
	require.EqualValues(t, 1, manager.listTargetsOpts.Pagination.Offset)
	require.EqualValues(t, 2, manager.listTargetsOpts.Pagination.Limit)
	require.EqualValues(t, 2, resp.GetTotal())
	require.Len(t, resp.GetTargets(), 1)
	require.Equal(
		t,
		pb.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_BLOCKED,
		resp.GetTargets()[0].GetStatus(),
	)
	require.Equal(t, "rack-01", resp.GetTargets()[0].GetRackExternalId())

	t.Run("rack without recoverable external ID is rejected", func(t *testing.T) {
		delete(inventory.racks, target.RackID)
		resp, err := server.ListOperationRunTargets(
			context.Background(),
			&pb.ListOperationRunTargetsRequest{OperationRunId: protobuf.UUIDTo(runID)},
		)
		require.Nil(t, resp)
		require.Equal(t, codes.FailedPrecondition, status.Code(err))
	})
}

func TestListOperationRunTargetsRejectsInvalidID(t *testing.T) {
	manager := &mockOperationRunManager{}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.ListOperationRunTargets(
		context.Background(),
		&pb.ListOperationRunTargetsRequest{},
	)

	require.Nil(t, resp)
	require.Equal(t, codes.InvalidArgument, status.Code(err))
	require.Equal(t, 0, manager.listTargetsCalls)
}

func TestListOperationRunTargetsMapsMissingRunToNotFound(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{
		listTargetsErr: fmt.Errorf("%w: %s", operationrunmanager.ErrOperationRunNotFound, runID),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.ListOperationRunTargets(
		context.Background(),
		&pb.ListOperationRunTargetsRequest{
			OperationRunId: protobuf.UUIDTo(runID),
		},
	)

	require.Nil(t, resp)
	require.Equal(t, codes.NotFound, status.Code(err))
	require.ErrorContains(t, err, "operation run not found")
	require.ErrorContains(t, err, runID.String())
	require.Equal(t, 1, manager.listTargetsCalls)
}

func TestPauseOperationRunCallsManager(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{
		pauseRun: testOperationRun(t, runID),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.PauseOperationRun(
		context.Background(),
		&pb.PauseOperationRunRequest{Id: protobuf.UUIDTo(runID)},
	)

	require.NoError(t, err)
	require.Equal(t, 1, manager.pauseCalls)
	require.Equal(t, runID, manager.pauseID)
	require.Equal(t, runID.String(), resp.GetSummary().GetId().GetId())
}

func TestResumeOperationRunCallsManager(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{
		resumeRun: testOperationRun(t, runID),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.ResumeOperationRun(
		context.Background(),
		&pb.ResumeOperationRunRequest{Id: protobuf.UUIDTo(runID)},
	)

	require.NoError(t, err)
	require.Equal(t, 1, manager.resumeCalls)
	require.Equal(t, runID, manager.resumeID)
	require.Equal(t, runID.String(), resp.GetSummary().GetId().GetId())
}

func TestAdvanceOperationRunPhaseCallsManager(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	expectedPhase := int32(2)
	manager := &mockOperationRunManager{
		advanceRun: testOperationRun(t, runID),
	}
	server := &FlowServerImpl{operationRunManager: manager}

	resp, err := server.AdvanceOperationRunPhase(
		context.Background(),
		&pb.AdvanceOperationRunPhaseRequest{
			Id:                 protobuf.UUIDTo(runID),
			ExpectedPhaseIndex: &expectedPhase,
		},
	)

	require.NoError(t, err)
	require.Equal(t, 1, manager.advanceCalls)
	require.Equal(t, runID, manager.advanceID)
	require.NotNil(t, manager.advanceExpectedPhaseIndex)
	require.Equal(t, expectedPhase, *manager.advanceExpectedPhaseIndex)
	require.Equal(t, runID.String(), resp.GetSummary().GetId().GetId())
}

func TestCancelOperationRunCallsManager(t *testing.T) {
	runID := uuid.MustParse("11111111-1111-1111-1111-111111111111")
	manager := &mockOperationRunManager{
		cancelRun: testOperationRun(t, runID),
	}
	server := &FlowServerImpl{
		operationRunManager: manager,
		taskManager:         &fakeOperationRunTaskManager{},
	}

	resp, err := server.CancelOperationRun(
		context.Background(),
		&pb.CancelOperationRunRequest{
			Id:     protobuf.UUIDTo(runID),
			Reason: "operator requested",
		},
	)

	require.NoError(t, err)
	require.Equal(t, 1, manager.cancelCalls)
	require.Equal(t, runID, manager.cancelID)
	require.Equal(t, "operator requested", manager.cancelReason)
	require.NotNil(t, manager.cancelCanceller)
	require.Equal(t, runID.String(), resp.GetSummary().GetId().GetId())
}

type mockOperationRunManager struct {
	createID    uuid.UUID
	createErr   error
	createCalls int
	createdRun  *operationrun.OperationRun

	getRun   *operationrun.OperationRun
	getErr   error
	getCalls int
	getID    uuid.UUID

	listRuns  []*operationrun.OperationRun
	listTotal int32
	listErr   error
	listCalls int
	listOpts  operationrun.ListOptions

	listTargets            []*operationrun.OperationRunTarget
	listTargetsTotal       int32
	listTargetsErr         error
	listTargetsCalls       int
	listTargetsID          uuid.UUID
	listTargetsOpts        operationrun.TargetListOptions
	listTargetsOptsHistory []operationrun.TargetListOptions
	pageListTargets        bool

	pauseRun   *operationrun.OperationRun
	pauseErr   error
	pauseCalls int
	pauseID    uuid.UUID

	resumeRun   *operationrun.OperationRun
	resumeErr   error
	resumeCalls int
	resumeID    uuid.UUID

	advanceRun                *operationrun.OperationRun
	advanceErr                error
	advanceCalls              int
	advanceID                 uuid.UUID
	advanceExpectedPhaseIndex *int32

	cancelRun       *operationrun.OperationRun
	cancelErr       error
	cancelCalls     int
	cancelID        uuid.UUID
	cancelReason    string
	cancelCanceller operationrunmanager.TaskCanceller
}

func (m *mockOperationRunManager) Create(
	_ context.Context,
	run *operationrun.OperationRun,
) (uuid.UUID, error) {
	m.createCalls++
	m.createdRun = run
	if m.createErr != nil {
		return uuid.Nil, m.createErr
	}

	return m.createID, nil
}

func (m *mockOperationRunManager) Get(
	_ context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	m.getCalls++
	m.getID = id
	if m.getErr != nil {
		return nil, m.getErr
	}

	return m.getRun, nil
}

func (m *mockOperationRunManager) List(
	_ context.Context,
	opts operationrun.ListOptions,
) ([]*operationrun.OperationRun, int32, error) {
	m.listCalls++
	m.listOpts = opts
	if m.listErr != nil {
		return nil, 0, m.listErr
	}

	return m.listRuns, m.listTotal, nil
}

func (m *mockOperationRunManager) ListTargets(
	_ context.Context,
	id uuid.UUID,
	opts operationrun.TargetListOptions,
) ([]*operationrun.OperationRunTarget, int32, error) {
	m.listTargetsCalls++
	m.listTargetsID = id
	m.listTargetsOpts = cloneTargetListOptions(opts)
	m.listTargetsOptsHistory = append(m.listTargetsOptsHistory, cloneTargetListOptions(opts))
	if m.listTargetsErr != nil {
		return nil, 0, m.listTargetsErr
	}

	targets := m.listTargets
	if m.pageListTargets && opts.Pagination != nil {
		offset := opts.Pagination.Offset
		if offset >= len(targets) {
			return nil, m.listTargetsTotal, nil
		}

		end := offset + opts.Pagination.Limit
		if end > len(targets) {
			end = len(targets)
		}
		targets = targets[offset:end]
	}

	return targets, m.listTargetsTotal, nil
}

func (m *mockOperationRunManager) Pause(
	_ context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	m.pauseCalls++
	m.pauseID = id
	if m.pauseErr != nil {
		return nil, m.pauseErr
	}

	return m.pauseRun, nil
}

func (m *mockOperationRunManager) Resume(
	_ context.Context,
	id uuid.UUID,
) (*operationrun.OperationRun, error) {
	m.resumeCalls++
	m.resumeID = id
	if m.resumeErr != nil {
		return nil, m.resumeErr
	}

	return m.resumeRun, nil
}

func (m *mockOperationRunManager) AdvancePhase(
	_ context.Context,
	id uuid.UUID,
	expectedPhaseIndex *int32,
) (*operationrun.OperationRun, error) {
	m.advanceCalls++
	m.advanceID = id
	m.advanceExpectedPhaseIndex = expectedPhaseIndex
	if m.advanceErr != nil {
		return nil, m.advanceErr
	}

	return m.advanceRun, nil
}

func (m *mockOperationRunManager) Cancel(
	_ context.Context,
	id uuid.UUID,
	reason string,
	canceller operationrunmanager.TaskCanceller,
) (*operationrun.OperationRun, error) {
	m.cancelCalls++
	m.cancelID = id
	m.cancelReason = reason
	m.cancelCanceller = canceller
	if m.cancelErr != nil {
		return nil, m.cancelErr
	}

	return m.cancelRun, nil
}

type fakeOperationRunTaskManager struct{}

func (*fakeOperationRunTaskManager) Start(context.Context) error {
	return nil
}

func (*fakeOperationRunTaskManager) Stop(context.Context) {}

func (*fakeOperationRunTaskManager) SubmitTask(
	context.Context,
	*operation.Request,
) ([]uuid.UUID, error) {
	return nil, nil
}

func (*fakeOperationRunTaskManager) CancelTask(context.Context, uuid.UUID) error {
	return nil
}

func cloneTargetListOptions(opts operationrun.TargetListOptions) operationrun.TargetListOptions {
	cloned := opts
	if opts.Pagination != nil {
		pagination := *opts.Pagination
		cloned.Pagination = &pagination
	}

	return cloned
}

func testOperationRun(
	t *testing.T,
	id uuid.UUID,
) *operationrun.OperationRun {
	t.Helper()
	run, err := protobuf.OperationRunFrom(validCreateOperationRunRequest())
	require.NoError(t, err)
	run.ID = id
	run.Status = operationrun.OperationRunStatusRunning
	run.CreatedAt = time.Date(2026, 6, 16, 1, 2, 3, 0, time.UTC)
	run.UpdatedAt = run.CreatedAt.Add(time.Minute)
	return run
}

func testOperationRunTarget(
	runID uuid.UUID,
	phase int32,
	status operationrun.OperationRunTargetStatus,
) *operationrun.OperationRunTarget {
	now := time.Date(2026, 6, 16, 1, 2, 3, 0, time.UTC)
	return &operationrun.OperationRunTarget{
		ID:             uuid.New(),
		OperationRunID: runID,
		RackID:         uuid.New(),
		SequenceIndex:  1,
		PhaseIndex:     phase,
		Status:         status,
		CreatedAt:      now,
		UpdatedAt:      now.Add(time.Minute),
	}
}

func validCreateOperationRunRequest() *pb.CreateOperationRunRequest {
	targetVersion := "1.2.3"

	return &pb.CreateOperationRunRequest{
		Name: "firmware canary",
		Configuration: &pb.OperationRunConfiguration{
			Selector: &pb.OperationRunSelector{
				Selector: &pb.OperationRunSelector_Percentage{
					Percentage: &pb.PercentageSelector{Percentage: 10},
				},
			},
			Options: &pb.OperationRunOptions{
				MaxConcurrentTargets: 2,
				SafetyPolicy: &pb.OperationRunSafetyPolicy{
					Gates: []*pb.OperationRunSafetyGate{
						{
							Gate: &pb.OperationRunSafetyGate_FailureRate{
								FailureRate: &pb.OperationRunFailureRateGate{
									FailureThresholdPercent: 20,
								},
							},
						},
					},
				},
			},
			Operation: &pb.OperationRunOperation{
				Operation: &pb.OperationRunOperation_UpgradeFirmware{
					UpgradeFirmware: &pb.UpgradeFirmwareRequest{
						TargetVersion: &targetVersion,
						Description:   "roll rack firmware",
						QueueOptions: &pb.QueueOptions{
							ConflictStrategy:    pb.ConflictStrategy_CONFLICT_STRATEGY_QUEUE,
							QueueTimeoutSeconds: 300,
						},
						RuleId: &pb.UUID{
							Id: "33333333-3333-3333-3333-333333333333",
						},
						SubTargets:             []string{"bmc"},
						OverrideReadinessCheck: true,
					},
				},
			},
		},
	}
}
