// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/types/known/fieldmaskpb"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	taskschedule "github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/taskschedule"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// scheduleRPCStore captures reads and writes; unexpected operations fail through
// the embedded interface instead of silently succeeding.
type scheduleRPCStore struct {
	taskschedule.Store
	row         *dbmodel.TaskSchedule
	getErr      error
	reads       int
	writes      int
	requestedID uuid.UUID
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
		wantCode   codes.Code
		wantWrites int
	}{
		{name: "malformed ID", id: "not-a-uuid", wantCode: codes.InvalidArgument},
		{name: "missing update mask", id: id.String(), wantCode: codes.InvalidArgument},
		{name: "rename schedule", id: id.String(), mask: &fieldmaskpb.FieldMask{Paths: []string{"schedule.name"}}, wantCode: codes.OK, wantWrites: 1},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			store := &scheduleRPCStore{row: &dbmodel.TaskSchedule{ID: id, Name: "old", Enabled: true}}
			client := flowRPCClient(t, &FlowServerImpl{taskScheduleStore: store})
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()

			response, err := client.UpdateTaskSchedule(ctx, &pb.UpdateTaskScheduleRequest{
				Id:         &pb.UUID{Id: tt.id},
				Schedule:   &pb.ScheduleConfig{Name: "renamed"},
				UpdateMask: tt.mask,
			})

			require.Equal(t, tt.wantCode, status.Code(err))
			require.Zero(t, store.reads)
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
