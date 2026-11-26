// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/component"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestStripTaskReports(t *testing.T) {
	t.Run("clears report on each non-nil task", func(t *testing.T) {
		tasks := []*pb.Task{
			{Id: &pb.UUID{Id: "a"}, Message: "running", Report: `{"version":1}`},
			{Id: &pb.UUID{Id: "b"}, Message: "failed", Report: `{"version":1,"stages":[]}`},
		}

		stripTaskReports(tasks)

		for _, tk := range tasks {
			assert.Empty(t, tk.GetReport())
		}
		// Other fields must survive so callers still see status / message.
		assert.Equal(t, "running", tasks[0].GetMessage())
		assert.Equal(t, "failed", tasks[1].GetMessage())
	})

	t.Run("tolerates nil entries", func(t *testing.T) {
		tasks := []*pb.Task{nil, {Id: &pb.UUID{Id: "c"}, Report: "{}"}}

		assert.NotPanics(t, func() { stripTaskReports(tasks) })
		assert.Empty(t, tasks[1].GetReport())
	})

	t.Run("empty slice is a no-op", func(t *testing.T) {
		assert.NotPanics(t, func() { stripTaskReports(nil) })
		assert.NotPanics(t, func() { stripTaskReports([]*pb.Task{}) })
	})
}

type eventTasksStore struct {
	taskstore.Store
	options *taskcommon.TaskListOptions
	page    *dbquery.Pagination
	err     error
	tasks   []*taskdef.Task
}

func (s *eventTasksStore) ListTasks(_ context.Context, options *taskcommon.TaskListOptions, page *dbquery.Pagination) ([]*taskdef.Task, int32, error) {
	s.options = options
	s.page = page
	return s.tasks, 7, s.err
}

func TestFlowServerImpl_ListTasks(t *testing.T) {
	policyID, rackID, componentID := uuid.New(), uuid.New(), uuid.New()
	externalID := uuid.NewString() // A UUID-shaped external ID must not bypass inventory resolution.
	for _, tc := range []struct {
		name       string
		err        error
		code       codes.Code
		filter     *pb.EventTaskFilter
		rack       string
		component  string
		withReport bool
		skipStore  bool
	}{
		{name: "ordinary Tasks"},
		{name: "all event Tasks", filter: &pb.EventTaskFilter{}},
		{name: "historical policy and type with target", filter: &pb.EventTaskFilter{EventType: proto.String("hardware.leak.detected"), PolicyId: &pb.UUID{Id: policyID.String()}}, rack: externalID, withReport: true},
		{name: "historical policy and Tray target", filter: &pb.EventTaskFilter{PolicyId: &pb.UUID{Id: policyID.String()}}, component: externalID},
		{name: "unknown Tray", component: "retired-tray", filter: &pb.EventTaskFilter{}, code: codes.NotFound, skipStore: true},
		{name: "unknown Rack", rack: "retired", code: codes.NotFound, skipStore: true},
		{name: "empty event type", filter: &pb.EventTaskFilter{EventType: proto.String("")}, code: codes.InvalidArgument, skipStore: true},
		{name: "unsupported event type", filter: &pb.EventTaskFilter{EventType: proto.String("future.event")}, code: codes.InvalidArgument, skipStore: true},
		{name: "invalid policy UUID", filter: &pb.EventTaskFilter{PolicyId: &pb.UUID{Id: "invalid"}}, code: codes.InvalidArgument, skipStore: true},
		{name: "zero policy UUID", filter: &pb.EventTaskFilter{PolicyId: &pb.UUID{Id: uuid.Nil.String()}}, code: codes.InvalidArgument, skipStore: true},
		{name: "wrapped database failure", err: fmt.Errorf("list event tasks: %w", &pgconn.PgError{Code: "08006", Message: "database connection failed"}), code: codes.Internal},
		{name: "conversion failure", err: fmt.Errorf("decode event task: %w", errors.New("invalid persisted task trigger")), code: codes.Internal},
	} {
		t.Run(tc.name, func(t *testing.T) {
			id := uuid.New()
			store := &eventTasksStore{err: tc.err, tasks: []*taskdef.Task{{ID: id, Report: []byte("private report")}}}
			inventory := newMockManager()
			inventory.racks[rackID] = &rack.Rack{ExternalID: externalID}
			inventory.racks[rackID].Info.ID = rackID
			inventory.components[componentID] = &component.Component{ComponentID: externalID, RackID: rackID}
			inventory.components[componentID].Info.ID = componentID
			server := &FlowServerImpl{eventRuleManager: &mockEventRuleManager{}, taskStore: store, inventoryManager: inventory}
			request := &pb.ListTasksRequest{Pagination: &pb.Pagination{Offset: 2, Limit: 3}, ActiveOnly: true, EventFilter: tc.filter, WithReport: tc.withReport}
			if tc.rack != "" {
				request.RackId = &pb.UUID{Id: tc.rack}
			}
			if tc.component != "" {
				request.ComponentId = &pb.UUID{Id: tc.component}
			}
			response, err := server.ListTasks(t.Context(), request)
			require.Equal(t, tc.code, status.Code(err))
			if tc.skipStore {
				require.Nil(t, store.options)
				require.Nil(t, response)
				return
			}
			require.Equal(t, &dbquery.Pagination{Offset: 2, Limit: 3}, store.page)
			require.True(t, store.options.ActiveOnly)
			if tc.filter == nil {
				require.Nil(t, store.options.EventFilter)
			} else {
				require.NotNil(t, store.options.EventFilter)
				require.Equal(t, tc.filter.GetEventType(), store.options.EventFilter.EventType)
				if tc.filter.PolicyId != nil {
					require.Equal(t, policyID, store.options.EventFilter.PolicyID)
				}
			}
			if tc.rack != "" {
				require.Equal(t, rackID, store.options.RackID)
			}
			if tc.component != "" {
				require.Equal(t, componentID, store.options.ComponentID)
			}
			if tc.err != nil {
				require.Nil(t, response)
				require.NotContains(t, status.Convert(err).Message(), tc.err.Error())
				return
			}
			require.NoError(t, err)
			require.EqualValues(t, 7, response.GetTotal())
			require.True(t, proto.Equal(&pb.TaskListAppliedFilters{RackId: request.RackId, ComponentId: request.ComponentId, EventFilter: request.EventFilter}, response.AppliedFilters))
			require.Len(t, response.GetTasks(), 1)
			require.Equal(t, id.String(), response.GetTasks()[0].GetId().GetId())
			if tc.withReport {
				require.Equal(t, "private report", response.GetTasks()[0].GetReport())
			} else {
				require.Empty(t, response.GetTasks()[0].GetReport())
			}
		})
	}
}
