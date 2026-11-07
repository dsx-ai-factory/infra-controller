// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

type leakHandlingStore struct {
	taskstore.Store
	statuses     map[uuid.UUID]taskcommon.TaskStatus
	componentIDs []uuid.UUID
	err          error
}

func (s *leakHandlingStore) LatestLeakageShutdownTaskStatuses(
	_ context.Context,
	componentIDs []uuid.UUID,
) (map[uuid.UUID]taskcommon.TaskStatus, error) {
	s.componentIDs = append([]uuid.UUID{}, componentIDs...)
	return s.statuses, s.err
}

func TestPopulateLeakHandlingStatuses(t *testing.T) {
	waitingID := uuid.New()
	pendingID := uuid.New()
	runningID := uuid.New()
	completedID := uuid.New()
	failedID := uuid.New()
	terminatedID := uuid.New()
	unknownID := uuid.New()
	noneID := uuid.New()
	storeErr := errors.New("task store unavailable")

	component := func(id uuid.UUID) *pb.Component {
		return &pb.Component{Info: &pb.DeviceInfo{Id: &pb.UUID{Id: id.String()}}}
	}

	tests := []struct {
		name       string
		store      *leakHandlingStore
		racks      []*pb.Rack
		components []*pb.Component
		want       []pb.LeakHandlingStatus
		wantIDs    []uuid.UUID
		wantErr    error
	}{
		{
			name:       "missing store leaves status unknown",
			components: []*pb.Component{component(unknownID)},
			want:       []pb.LeakHandlingStatus{pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_UNKNOWN},
		},
		{
			name:  "store error",
			store: &leakHandlingStore{err: storeErr},
			components: []*pb.Component{
				component(unknownID),
			},
			want:    []pb.LeakHandlingStatus{pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_UNKNOWN},
			wantIDs: []uuid.UUID{unknownID},
			wantErr: storeErr,
		},
		{
			name: "maps latest task statuses in one batch",
			store: &leakHandlingStore{statuses: map[uuid.UUID]taskcommon.TaskStatus{
				waitingID:    taskcommon.TaskStatusWaiting,
				pendingID:    taskcommon.TaskStatusPending,
				runningID:    taskcommon.TaskStatusRunning,
				completedID:  taskcommon.TaskStatusCompleted,
				failedID:     taskcommon.TaskStatusFailed,
				terminatedID: taskcommon.TaskStatusTerminated,
				unknownID:    taskcommon.TaskStatusUnknown,
			}},
			racks: []*pb.Rack{{Components: []*pb.Component{
				component(waitingID), component(pendingID), component(runningID), component(completedID),
			}}},
			components: []*pb.Component{
				component(failedID), component(terminatedID), component(unknownID), component(noneID),
			},
			want: []pb.LeakHandlingStatus{
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_SHUTTING_DOWN,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_SHUTTING_DOWN,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_SHUTTING_DOWN,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_DOWN,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_FAILED,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_FAILED,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_UNKNOWN,
				pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_NONE,
			},
			wantIDs: []uuid.UUID{
				waitingID, pendingID, runningID, completedID,
				failedID, terminatedID, unknownID, noneID,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := &FlowServerImpl{}
			if tt.store != nil {
				server.taskStore = tt.store
			}
			err := server.populateLeakHandlingStatuses(t.Context(), tt.racks, tt.components)
			if tt.wantErr != nil {
				require.ErrorIs(t, err, tt.wantErr)
			} else {
				require.NoError(t, err)
			}
			if tt.store != nil {
				assert.Equal(t, tt.wantIDs, tt.store.componentIDs)
			}

			got := make([]pb.LeakHandlingStatus, 0, len(tt.want))
			for _, rack := range tt.racks {
				for _, component := range rack.GetComponents() {
					got = append(got, component.GetLeakHandlingStatus())
				}
			}
			for _, component := range tt.components {
				got = append(got, component.GetLeakHandlingStatus())
			}
			assert.Equal(t, tt.want, got)
		})
	}
}
