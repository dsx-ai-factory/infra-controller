// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"google.golang.org/protobuf/types/known/emptypb"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/fieldmaskpb"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	taskschedule "github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/taskschedule"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// scheduleRPCStore captures reads and writes; unexpected operations fail through
// the embedded interface instead of silently succeeding.
type scheduleRPCStore struct {
	taskschedule.Store
	scopes        []*dbmodel.TaskScheduleScope
	row           *dbmodel.TaskSchedule
	getErr        error
	deleteErr     error
	transactions  int
	inTransaction bool
	reads         int
	writes        int
	requestedID   uuid.UUID
}

func (s *scheduleRPCStore) Get(_ context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error) {
	s.reads++
	s.requestedID = id
	return s.row, s.getErr
}

func (s *scheduleRPCStore) Update(_ context.Context, id uuid.UUID, fields taskschedule.UpdateFields) (*dbmodel.TaskSchedule, error) {
	s.writes++
	s.requestedID = id
	s.row.Name = fields.Name
	return s.row, nil
}

func (s *scheduleRPCStore) SetEnabled(_ context.Context, id uuid.UUID, enabled bool) (*dbmodel.TaskSchedule, error) {
	s.writes++
	s.requestedID = id
	s.row.Enabled = enabled
	return s.row, nil
}

func (s *scheduleRPCStore) RunInTransaction(ctx context.Context, fn func(context.Context) error) error {
	s.transactions++
	s.inTransaction = true
	defer func() { s.inTransaction = false }()
	return fn(ctx)
}

func (s *scheduleRPCStore) LockForTrigger(ctx context.Context, id uuid.UUID) (*dbmodel.TaskSchedule, error) {
	if !s.inTransaction {
		return nil, errors.New("lock outside transaction")
	}
	return s.Get(ctx, id)
}

func (s *scheduleRPCStore) Resume(_ context.Context, id uuid.UUID, nextRunAt *time.Time) (*dbmodel.TaskSchedule, error) {
	if !s.inTransaction {
		return nil, errors.New("resume outside transaction")
	}
	s.writes++
	s.requestedID = id
	s.row.Enabled = true
	if nextRunAt != nil {
		s.row.NextRunAt = nextRunAt
	}
	return s.row, nil
}

func (s *scheduleRPCStore) Delete(_ context.Context, id uuid.UUID) error {
	s.writes++
	s.requestedID = id
	return s.deleteErr
}

func TestFlowServerImpl_GetTaskSchedule(t *testing.T) {
	id := uuid.New()
	tests := []struct {
		name      string
		id        *pb.UUID
		storeErr  error
		wantCode  codes.Code
		wantReads int
	}{
		{name: "missing ID", wantCode: codes.InvalidArgument},
		{name: "empty ID", id: &pb.UUID{}, wantCode: codes.InvalidArgument},
		{name: "malformed ID", id: &pb.UUID{Id: "not-a-uuid"}, wantCode: codes.InvalidArgument},
		{name: "zero ID", id: &pb.UUID{Id: uuid.Nil.String()}, wantCode: codes.InvalidArgument},
		{name: "existing schedule", id: &pb.UUID{Id: id.String()}, wantCode: codes.OK, wantReads: 1},
		// A store failure must not be reclassified as invalid request data.
		{name: "store error is preserved", id: &pb.UUID{Id: id.String()}, storeErr: errors.New("database unavailable"), wantCode: codes.Unknown, wantReads: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{ID: id, Name: "nightly"}, getErr: tt.storeErr}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()

			response, err := client.GetTaskSchedule(ctx, &pb.GetTaskScheduleRequest{Id: tt.id})

			require.Equal(t, tt.wantCode, status.Code(err))
			require.Equal(t, tt.wantReads, store.reads)
			require.Zero(t, store.writes)
			if tt.wantReads > 0 {
				require.Equal(t, id, store.requestedID)
			}
			if tt.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.NoError(t, err)
			require.Equal(t, id.String(), response.GetId().GetId())
			require.Equal(t, "nightly", response.GetName())
		})
	}
}

func TestFlowServerImpl_UpdateTaskSchedule(t *testing.T) {
	id := uuid.New()
	tests := []struct {
		name       string
		id         string
		mask       *fieldmaskpb.FieldMask
		schedule   *pb.ScheduleConfig
		storeErr   error
		existing   *dbmodel.TaskSchedule
		wantReads  int
		wantCode   codes.Code
		wantWrites int
	}{
		{name: "malformed ID", id: "not-a-uuid", wantCode: codes.InvalidArgument},
		{name: "missing update mask", id: id.String(), wantCode: codes.InvalidArgument},
		{name: "missing schedule", id: id.String(), mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.name"}}, wantCode: codes.InvalidArgument},
		{name: "empty masked name", id: id.String(), schedule: &pb.ScheduleConfig{}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.name"}}, wantCode: codes.InvalidArgument},
		{name: "unsupported mask", id: id.String(), schedule: &pb.ScheduleConfig{Name: "renamed"}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.description"}}, wantCode: codes.InvalidArgument},
		{name: "missing spec under row lock", id: id.String(), schedule: &pb.ScheduleConfig{}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec"}}, wantCode: codes.InvalidArgument, wantReads: 1},
		{name: "lock error is preserved", id: id.String(), schedule: &pb.ScheduleConfig{}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec"}}, storeErr: errors.New("database unavailable"), wantCode: codes.Unknown, wantReads: 1},
		{name: "invalid duration", id: id.String(), schedule: &pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_INTERVAL, Spec: "bad", Timezone: ""}}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec"}}, wantCode: codes.InvalidArgument, wantReads: 1, existing: nil},
		{name: "invalid timezone", id: id.String(), schedule: &pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON, Spec: "0 * * * *", Timezone: "Mars/Base"}}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec.timezone"}}, wantCode: codes.InvalidArgument, wantReads: 1, existing: &dbmodel.TaskSchedule{ID: id, Name: "old", Enabled: true, SpecType: dbmodel.SpecTypeCron, Spec: "0 * * * *", Timezone: "UTC"}},
		{name: "stored timezone error", id: id.String(), schedule: &pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON, Spec: "0 * * * *", Timezone: ""}}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec"}}, wantCode: codes.Internal, wantReads: 1, existing: &dbmodel.TaskSchedule{ID: id, Name: "old", Enabled: true, SpecType: dbmodel.SpecTypeCron, Spec: "0 * * * *", Timezone: "Mars/Base"}},
		{name: "stored cron error", id: id.String(), schedule: &pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_CRON, Spec: "", Timezone: "UTC"}}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec.timezone"}}, wantCode: codes.Internal, wantReads: 1, existing: &dbmodel.TaskSchedule{ID: id, Name: "old", Enabled: true, SpecType: dbmodel.SpecTypeCron, Spec: "bad", Timezone: "UTC"}},
		{name: "consumed one-time spec change", id: id.String(), schedule: &pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Type: pb.ScheduleSpecType_SCHEDULE_SPEC_TYPE_ONE_TIME, Spec: "2035-01-01T00:00:00Z", Timezone: ""}}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec"}}, wantCode: codes.FailedPrecondition, wantReads: 1, existing: &dbmodel.TaskSchedule{ID: id, Name: "old", Enabled: true, SpecType: dbmodel.SpecTypeOneTime}},
		{name: "unsupported spec type", id: id.String(), schedule: &pb.ScheduleConfig{Spec: &pb.ScheduleSpec{Type: pb.ScheduleSpecType(99), Spec: "1h", Timezone: ""}}, mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.spec"}}, wantCode: codes.InvalidArgument, wantReads: 1, existing: nil},
		{name: "rename schedule", schedule: &pb.ScheduleConfig{Name: "renamed"}, id: id.String(), mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.name"}}, wantCode: codes.OK, wantWrites: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{ID: id, Name: "old", Enabled: true}, getErr: tt.storeErr}
			if tt.existing != nil {
				store.row = tt.existing
			}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()

			response, err := client.UpdateTaskSchedule(ctx, &pb.UpdateTaskScheduleRequest{
				Id:         &pb.UUID{Id: tt.id},
				Schedule:   tt.schedule,
				UpdateMask: tt.mask,
			})

			require.Equal(t, tt.wantCode, status.Code(err))
			require.Equal(t, tt.wantReads, store.reads)
			require.Equal(t, tt.wantWrites, store.writes)
			require.True(t, store.row.Enabled)
			if tt.wantCode != codes.OK {
				require.Nil(t, response)
				require.Equal(t, "old", store.row.Name)
				return
			}
			require.NoError(t, err)
			require.Equal(t, id, store.requestedID)
			require.Equal(t, "renamed", store.row.Name)
			require.Equal(t, "renamed", response.GetName())
		})
	}
}

func TestFlowServerImpl_PauseTaskSchedule(t *testing.T) {
	id := uuid.New()
	next := time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC)
	tests := []struct {
		name       string
		id         string
		specType   dbmodel.SpecType
		enabled    bool
		nextRunAt  *time.Time
		wantCode   codes.Code
		wantReads  int
		wantWrites int
	}{
		{name: "zero ID", id: uuid.Nil.String(), wantCode: codes.InvalidArgument},
		{name: "fired one-time schedule", id: id.String(), specType: dbmodel.SpecTypeOneTime, wantCode: codes.FailedPrecondition, wantReads: 1},
		{name: "pause before first firing", id: id.String(), specType: dbmodel.SpecTypeOneTime, enabled: true, nextRunAt: &next, wantCode: codes.OK, wantReads: 1, wantWrites: 1},
		{name: "already paused is idempotent", id: id.String(), specType: dbmodel.SpecTypeInterval, nextRunAt: &next, wantCode: codes.OK, wantReads: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{
				ID: id, SpecType: tt.specType, Enabled: tt.enabled, NextRunAt: tt.nextRunAt,
			}}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()

			response, err := client.PauseTaskSchedule(ctx, &pb.PauseTaskScheduleRequest{Id: &pb.UUID{Id: tt.id}})

			require.Equal(t, tt.wantCode, status.Code(err))
			require.Equal(t, tt.wantReads, store.reads)
			require.Equal(t, tt.wantWrites, store.writes)
			require.Equal(t, tt.nextRunAt, store.row.NextRunAt)
			if tt.wantReads > 0 {
				require.Equal(t, id, store.requestedID)
			}
			if tt.wantCode != codes.OK {
				require.Nil(t, response)
				require.Equal(t, tt.enabled, store.row.Enabled)
				return
			}
			require.NoError(t, err)
			require.False(t, store.row.Enabled)
			require.False(t, response.GetEnabled())
			require.Equal(t, id.String(), response.GetId().GetId())
		})
	}
}

func TestFlowServerImpl_ResumeTaskSchedule(t *testing.T) {
	id := uuid.New()
	next := time.Date(2030, 1, 1, 0, 0, 0, 0, time.UTC)
	tests := []struct {
		name       string
		id         *pb.UUID
		enabled    bool
		nextRunAt  *time.Time
		storeErr   error
		wantCode   codes.Code
		wantReads  int
		wantWrites int
	}{
		{name: "missing ID", wantCode: codes.InvalidArgument},
		{name: "malformed ID", id: &pb.UUID{Id: "not-a-uuid"}, wantCode: codes.InvalidArgument},
		{name: "zero ID", id: &pb.UUID{Id: uuid.Nil.String()}, wantCode: codes.InvalidArgument},
		{name: "fired one-time schedule", id: &pb.UUID{Id: id.String()}, wantCode: codes.FailedPrecondition, wantReads: 1},
		{name: "resume before first firing", id: &pb.UUID{Id: id.String()}, nextRunAt: &next, wantCode: codes.OK, wantReads: 1, wantWrites: 1},
		{name: "already enabled is idempotent", id: &pb.UUID{Id: id.String()}, enabled: true, nextRunAt: &next, wantCode: codes.OK, wantReads: 1},
		{name: "lock error is preserved", id: &pb.UUID{Id: id.String()}, storeErr: errors.New("database unavailable"), wantCode: codes.Unknown, wantReads: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{ID: id, SpecType: dbmodel.SpecTypeOneTime, Enabled: tt.enabled, NextRunAt: tt.nextRunAt}, getErr: tt.storeErr}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			response, err := client.ResumeTaskSchedule(ctx, &pb.ResumeTaskScheduleRequest{Id: tt.id})
			require.Equal(t, tt.wantCode, status.Code(err))
			require.Equal(t, tt.wantReads, store.reads)
			require.Equal(t, tt.wantReads, store.transactions)
			require.Equal(t, tt.wantWrites, store.writes)
			require.Equal(t, tt.nextRunAt, store.row.NextRunAt)
			if tt.wantReads > 0 {
				require.Equal(t, id, store.requestedID)
			}
			if tt.wantCode != codes.OK {
				require.Nil(t, response)
				require.Equal(t, tt.enabled, store.row.Enabled)
				return
			}
			require.True(t, store.row.Enabled)
			require.True(t, response.GetEnabled())
			require.Equal(t, id.String(), response.GetId().GetId())
			require.Equal(t, next, response.GetNextRunAt().AsTime())
		})
	}
}

func TestFlowServerImpl_DeleteTaskSchedule(t *testing.T) {
	id := uuid.New()
	tests := []struct {
		name       string
		id         *pb.UUID
		storeErr   error
		wantCode   codes.Code
		wantWrites int
	}{
		{name: "missing ID", wantCode: codes.InvalidArgument},
		{name: "malformed ID", id: &pb.UUID{Id: "not-a-uuid"}, wantCode: codes.InvalidArgument},
		{name: "zero ID", id: &pb.UUID{Id: uuid.Nil.String()}, wantCode: codes.InvalidArgument},
		{name: "delete existing schedule", id: &pb.UUID{Id: id.String()}, wantCode: codes.OK, wantWrites: 1},
		{name: "store error is preserved", id: &pb.UUID{Id: id.String()}, storeErr: errors.New("database unavailable"), wantCode: codes.Unknown, wantWrites: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{deleteErr: tt.storeErr}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			response, err := client.DeleteTaskSchedule(ctx, &pb.DeleteTaskScheduleRequest{Id: tt.id})
			require.Equal(t, tt.wantCode, status.Code(err))
			require.Zero(t, store.reads)
			require.Equal(t, tt.wantWrites, store.writes)
			if tt.wantWrites > 0 {
				require.Equal(t, id, store.requestedID)
			}
			if tt.wantCode != codes.OK {
				require.Nil(t, response)
				return
			}
			require.NotNil(t, response)
		})
	}
}

func (s *scheduleRPCStore) GetScope(_ context.Context, id uuid.UUID) (*dbmodel.TaskScheduleScope, error) {
	s.reads++
	s.requestedID = id
	if s.getErr != nil {
		return nil, s.getErr
	}
	return &dbmodel.TaskScheduleScope{ID: id, ScheduleID: s.row.ID}, nil
}
func (s *scheduleRPCStore) ListScopes(_ context.Context, _ uuid.UUID) ([]*dbmodel.TaskScheduleScope, error) {
	return s.scopes, nil
}
func (s *scheduleRPCStore) DeleteScope(_ context.Context, _ uuid.UUID) error { s.writes++; return nil }

func checkScheduleIDValidation(t *testing.T, call func(context.Context, pb.FlowClient, *pb.UUID) error) {
	t.Helper()
	tests := []struct {
		name string
		id   *pb.UUID
	}{
		{name: "missing"}, {name: "malformed", id: &pb.UUID{Id: "bad"}}, {name: "zero", id: &pb.UUID{Id: uuid.Nil.String()}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			err := call(ctx, client, tt.id)
			require.Equal(t, codes.InvalidArgument, status.Code(err))
			require.Zero(t, store.reads)
			require.Zero(t, store.writes)
			require.Zero(t, store.transactions)
		})
	}
}
func TestFlowServerImpl_AddTaskScheduleScope(t *testing.T) {
	checkScheduleIDValidation(t, func(ctx context.Context, c pb.FlowClient, id *pb.UUID) error {
		_, err := c.AddTaskScheduleScope(ctx, &pb.AddTaskScheduleScopeRequest{ScheduleId: id})
		return err
	})
}
func TestFlowServerImpl_UpdateTaskScheduleScope(t *testing.T) {
	checkScheduleIDValidation(t, func(ctx context.Context, c pb.FlowClient, id *pb.UUID) error {
		_, err := c.UpdateTaskScheduleScope(ctx, &pb.UpdateTaskScheduleScopeRequest{ScheduleId: id})
		return err
	})
}
func TestFlowServerImpl_ListTaskScheduleScopes(t *testing.T) {
	checkScheduleIDValidation(t, func(ctx context.Context, c pb.FlowClient, id *pb.UUID) error {
		_, err := c.ListTaskScheduleScopes(ctx, &pb.ListTaskScheduleScopesRequest{ScheduleId: id})
		return err
	})
}
func TestFlowServerImpl_TriggerTaskSchedule(t *testing.T) {
	id := uuid.New()
	tests := []struct {
		name          string
		id            *pb.UUID
		getErr        error
		submissionErr error
		code          codes.Code
		reads         int
		writes        int
	}{
		{name: "missing ID", code: codes.InvalidArgument},
		{name: "all scope queues full", id: &pb.UUID{Id: id.String()}, submissionErr: taskmanager.ErrQueueFull, code: codes.ResourceExhausted, reads: 1, writes: 1},
		{name: "consumed", id: &pb.UUID{Id: id.String()}, code: codes.FailedPrecondition, reads: 1},
		{name: "store failure", id: &pb.UUID{Id: id.String()}, getErr: errors.New("database unavailable"), code: codes.Unknown, reads: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{ID: id, SpecType: dbmodel.SpecTypeOneTime}, getErr: tt.getErr}
			manager := &powerControlTaskManager{err: tt.submissionErr}
			if tt.submissionErr != nil {
				info := &operations.PowerControlTaskInfo{Operation: operations.PowerOperationPowerOn}
				raw, marshalErr := info.Marshal()
				require.NoError(t, marshalErr)
				template, marshalErr := taskschedule.MarshalTemplate(info.Type(), info.CodeString(), raw, taskschedule.TemplateOptions{})
				require.NoError(t, marshalErr)
				store.row.Enabled = true
				store.row.OperationTemplate = template
				store.scopes = []*dbmodel.TaskScheduleScope{{ID: uuid.New(), ScheduleID: id, RackID: uuid.New()}}
			}
			dispatcher := taskschedule.NewDispatcher(taskschedule.Config{Store: store, TaskManager: manager})
			c := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store, taskScheduleDispatcher: dispatcher})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			response, err := c.TriggerTaskSchedule(ctx, &pb.TriggerTaskScheduleRequest{Id: tt.id})
			require.Equal(t, tt.code, status.Code(err))
			require.Nil(t, response)
			require.Equal(t, tt.reads, store.reads)
			require.Equal(t, tt.writes, store.writes)
			if tt.submissionErr != nil {
				require.False(t, store.row.Enabled)
				require.Nil(t, store.row.NextRunAt)
			}
		})
	}
}
func TestFlowServerImpl_RemoveTaskScheduleScope(t *testing.T) {
	id := uuid.New()
	tests := []struct {
		name   string
		id     *pb.UUID
		count  int
		code   codes.Code
		writes int
	}{
		{name: "missing ID", code: codes.InvalidArgument},
		{name: "last scope", id: &pb.UUID{Id: id.String()}, count: 1, code: codes.FailedPrecondition},
		{name: "other scopes remain", id: &pb.UUID{Id: id.String()}, count: 2, code: codes.OK, writes: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{ID: uuid.New()}, scopes: make([]*dbmodel.TaskScheduleScope, tt.count)}
			c := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			var response *emptypb.Empty
			response, err := c.RemoveTaskScheduleScope(ctx, &pb.RemoveTaskScheduleScopeRequest{ScopeId: tt.id})
			require.Equal(t, tt.code, status.Code(err))
			require.Equal(t, tt.writes, store.writes)
			if tt.code == codes.OK {
				require.NotNil(t, response)
			} else {
				require.Nil(t, response)
			}
		})
	}
}

func (s *scheduleRPCStore) UpdateAfterFire(_ context.Context, _ uuid.UUID, u taskschedule.AfterFireUpdate) error {
	s.writes++
	s.row.Enabled = u.Enabled
	s.row.NextRunAt = u.NextRunAt
	return nil
}
