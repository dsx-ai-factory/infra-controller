// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
)

func TestAPITaskStats_FromProto(t *testing.T) {
	tests := []struct {
		name    string
		initial APITaskStats
		stats   *flowv1.TaskStats
		want    APITaskStats
	}{
		{
			name: "nil stats clear counts",
			initial: APITaskStats{
				PendingTaskCount: 2,
				ActiveTaskCount:  3,
			},
			want: APITaskStats{},
		},
		{
			name: "combines waiting and pending counts",
			stats: &flowv1.TaskStats{
				WaitingTaskCount: 2,
				PendingTaskCount: 3,
				RunningTaskCount: 4,
			},
			want: APITaskStats{
				PendingTaskCount: 5,
				ActiveTaskCount:  4,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tt.initial
			got.FromProto(tt.stats)
			assert.Equal(t, tt.want, got)
		})
	}
}

func TestNewAPITask(t *testing.T) {
	tests := []struct {
		name     string
		task     *flowv1.Task
		expected *APITask
	}{
		{
			name:     "nil task returns empty APITask",
			task:     nil,
			expected: &APITask{},
		},
		{
			name: "task with all fields",
			task: &flowv1.Task{
				Id:          &flowv1.UUID{Id: "task-123"},
				Operation:   "power_on",
				RackId:      &flowv1.UUID{Id: "rack-456"},
				Description: "Power on rack components",
				Status:      flowv1.TaskStatus_TASK_STATUS_RUNNING,
				Message:     "Processing 3 of 5 components",
			},
			expected: &APITask{
				ID:          "task-123",
				Status:      "Running",
				Description: "Power on rack components",
				Message:     "Processing 3 of 5 components",
			},
		},
		{
			name: "task with pending status",
			task: &flowv1.Task{
				Id:          &flowv1.UUID{Id: "task-001"},
				Description: "Firmware upgrade",
				Status:      flowv1.TaskStatus_TASK_STATUS_PENDING,
			},
			expected: &APITask{
				ID:          "task-001",
				Status:      "Pending",
				Description: "Firmware upgrade",
			},
		},
		{
			name: "task with completed status maps to succeeded",
			task: &flowv1.Task{
				Id:          &flowv1.UUID{Id: "task-002"},
				Description: "Bring up rack",
				Status:      flowv1.TaskStatus_TASK_STATUS_COMPLETED,
				Message:     "All components ready",
			},
			expected: &APITask{
				ID:          "task-002",
				Status:      "Succeeded",
				Description: "Bring up rack",
				Message:     "All components ready",
			},
		},
		{
			name: "task with failed status",
			task: &flowv1.Task{
				Id:          &flowv1.UUID{Id: "task-003"},
				Description: "Power off rack",
				Status:      flowv1.TaskStatus_TASK_STATUS_FAILED,
				Message:     "BMC unreachable",
			},
			expected: &APITask{
				ID:          "task-003",
				Status:      "Failed",
				Description: "Power off rack",
				Message:     "BMC unreachable",
			},
		},
		{
			name: "task with unknown status",
			task: &flowv1.Task{
				Id:     &flowv1.UUID{Id: "task-004"},
				Status: flowv1.TaskStatus_TASK_STATUS_UNKNOWN,
			},
			expected: &APITask{
				ID:     "task-004",
				Status: "Unknown",
			},
		},
		{
			name: "task with nil ID",
			task: &flowv1.Task{
				Description: "Orphan task",
				Status:      flowv1.TaskStatus_TASK_STATUS_PENDING,
			},
			expected: &APITask{
				Status:      "Pending",
				Description: "Orphan task",
			},
		},
		{
			name: "task with terminated status",
			task: &flowv1.Task{
				Id:      &flowv1.UUID{Id: "task-005"},
				Status:  flowv1.TaskStatus_TASK_STATUS_TERMINATED,
				Message: "Expired: queue timeout reached",
			},
			expected: &APITask{
				ID:      "task-005",
				Status:  "Terminated",
				Message: "Expired: queue timeout reached",
			},
		},
		{
			name: "task with waiting status is pending in REST",
			task: &flowv1.Task{
				Id:     &flowv1.UUID{Id: "task-006"},
				Status: flowv1.TaskStatus_TASK_STATUS_WAITING,
			},
			expected: &APITask{
				ID:     "task-006",
				Status: "Pending",
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := NewAPITask(tt.task)
			assert.NotNil(t, result)
			assert.Equal(t, tt.expected.ID, result.ID)
			assert.Equal(t, tt.expected.Status, result.Status)
			assert.Equal(t, tt.expected.Description, result.Description)
			assert.Equal(t, tt.expected.Message, result.Message)
			assert.Nil(t, result.Started)
			assert.Nil(t, result.Finished)
			assert.Nil(t, result.RuleID)
		})
	}
}

func TestNewAPIRackTask_RuleID(t *testing.T) {
	t.Run("nil applied_rule_id leaves RuleID unset", func(t *testing.T) {
		got := NewAPITask(&flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-a"},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
		})
		assert.Nil(t, got.RuleID)
	})
	t.Run("empty-string id is treated as unset", func(t *testing.T) {
		got := NewAPITask(&flowv1.Task{
			Id:            &flowv1.UUID{Id: "task-b"},
			Status:        flowv1.TaskStatus_TASK_STATUS_RUNNING,
			AppliedRuleId: &flowv1.UUID{Id: ""},
		})
		assert.Nil(t, got.RuleID)
	})
	t.Run("non-empty id is surfaced", func(t *testing.T) {
		got := NewAPITask(&flowv1.Task{
			Id:            &flowv1.UUID{Id: "task-c"},
			Status:        flowv1.TaskStatus_TASK_STATUS_RUNNING,
			AppliedRuleId: &flowv1.UUID{Id: "550e8400-e29b-41d4-a716-446655440000"},
		})
		require.NotNil(t, got.RuleID)
		assert.Equal(t, "550e8400-e29b-41d4-a716-446655440000", *got.RuleID)
	})
}

func TestNewAPIRackTask_Timestamps(t *testing.T) {
	createdTime := time.Date(2026, 1, 1, 9, 0, 0, 0, time.UTC)
	updatedTime := time.Date(2026, 1, 1, 9, 30, 0, 0, time.UTC)
	startTime := time.Date(2026, 1, 1, 10, 0, 0, 0, time.UTC)
	endTime := time.Date(2026, 1, 1, 11, 0, 0, 0, time.UTC)

	task := &flowv1.Task{
		Id:         &flowv1.UUID{Id: "task-ts"},
		Status:     flowv1.TaskStatus_TASK_STATUS_COMPLETED,
		CreatedAt:  timestamppb.New(createdTime),
		UpdatedAt:  timestamppb.New(updatedTime),
		StartedAt:  timestamppb.New(startTime),
		FinishedAt: timestamppb.New(endTime),
	}

	result := NewAPITask(task)

	assert.True(t, result.Created.Equal(createdTime))
	assert.True(t, result.Updated.Equal(updatedTime))
	assert.NotNil(t, result.Started)
	assert.NotNil(t, result.Finished)
	assert.True(t, result.Started.Equal(startTime))
	assert.True(t, result.Finished.Equal(endTime))
}

func TestNewAPITask_Report(t *testing.T) {
	t.Run("report omitted by default", func(t *testing.T) {
		task := &flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-rep-1"},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			Report: `{"version":1,"stages":[]}`,
		}

		result := NewAPITask(task)

		assert.Nil(t, result.Report, "Report must default to nil so the JSON field is omitted")
	})

	t.Run("WithTaskReport decodes a v1 payload into the typed struct with camelCase keys", func(t *testing.T) {
		body := `{
			"version": 1,
			"stages": [
				{
					"number": 1,
					"status": "completed",
					"started_at": "2026-06-08T18:00:00Z",
					"finished_at": "2026-06-08T18:00:42Z",
					"steps": [
						{
							"component_type": "Compute",
							"status": "completed",
							"total_components": 4,
							"started_at": "2026-06-08T18:00:00Z",
							"finished_at": "2026-06-08T18:00:42Z"
						}
					]
				}
			]
		}`
		task := &flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-rep-2"},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			Report: body,
		}

		result := NewAPITask(task, WithTaskReport())

		require.NotNil(t, result.Report)
		assert.Equal(t, 1, result.Report.Version)
		require.Len(t, result.Report.Stages, 1)
		assert.Equal(t, 1, result.Report.Stages[0].Number)
		assert.Equal(t, APITaskReportV1StatusCompleted, result.Report.Stages[0].Status)
		require.Len(t, result.Report.Stages[0].Steps, 1)
		assert.Equal(t, "Compute", result.Report.Stages[0].Steps[0].ComponentType)
		assert.Equal(t, 4, result.Report.Stages[0].Steps[0].TotalComponents)
		assert.Equal(t, "2026-06-08T18:00:00Z", result.Report.Stages[0].Steps[0].StartedAt)

		// Round-trip through json.Marshal to verify camelCase keys land on the wire.
		out, err := json.Marshal(result.Report)
		require.NoError(t, err)
		var wire map[string]any
		require.NoError(t, json.Unmarshal(out, &wire))
		stages := wire["stages"].([]any)
		step := stages[0].(map[string]any)["steps"].([]any)[0].(map[string]any)
		assert.Contains(t, step, "componentType", "must use camelCase on the wire, not component_type")
		assert.Contains(t, step, "totalComponents")
		assert.Contains(t, step, "startedAt")
		assert.NotContains(t, step, "component_type")
	})

	t.Run("WithTaskReport on empty proto report yields nil", func(t *testing.T) {
		task := &flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-rep-3"},
			Status: flowv1.TaskStatus_TASK_STATUS_PENDING,
		}

		result := NewAPITask(task, WithTaskReport())

		assert.Nil(t, result.Report, "Empty proto report must not surface as an empty JSON value")
	})

	t.Run("WithTaskReport on malformed JSON yields nil", func(t *testing.T) {
		task := &flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-rep-4"},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			Report: `{`,
		}

		result := NewAPITask(task, WithTaskReport())

		assert.Nil(t, result.Report, "Malformed report must not surface as a partial struct")
	})

	t.Run("WithTaskReport on non-v1 payload yields nil", func(t *testing.T) {
		task := &flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-rep-5"},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			Report: `{"version":2,"stages":[]}`,
		}

		result := NewAPITask(task, WithTaskReport())

		assert.Nil(t, result.Report, "v2+ payload must not be exposed behind the v1 contract")
	})
}

func TestAPIGetTasksRequest_TaskOptions(t *testing.T) {
	t.Run("default request yields no options", func(t *testing.T) {
		req := APIGetTasksRequest{SiteID: "s"}
		assert.Empty(t, req.TaskOptions())
	})

	t.Run("includeReport=true yields WithTaskReport()", func(t *testing.T) {
		req := APIGetTasksRequest{SiteID: "s", IncludeReport: true}
		opts := req.TaskOptions()
		require.Len(t, opts, 1)

		// The option must decode Task.report when the proto report is non-empty.
		task := &flowv1.Task{
			Id:     &flowv1.UUID{Id: "task-built"},
			Status: flowv1.TaskStatus_TASK_STATUS_RUNNING,
			Report: `{"version":1,"stages":[]}`,
		}
		got := NewAPITask(task, opts...)
		require.NotNil(t, got.Report)
		assert.Equal(t, 1, got.Report.Version)
	})
}

func TestAPIGetTasksRequest_QueryValues(t *testing.T) {
	t.Run("includeReport=true surfaces in query values", func(t *testing.T) {
		req := APIGetTasksRequest{SiteID: "site-x", IncludeReport: true}
		v := req.QueryValues(pagination.PageRequest{})

		assert.Equal(t, "true", v.Get("includeReport"))
		assert.Equal(t, "site-x", v.Get("siteId"))
	})

	t.Run("includeReport=false is omitted from query values", func(t *testing.T) {
		req := APIGetTasksRequest{SiteID: "site-y"}
		v := req.QueryValues(pagination.PageRequest{})

		assert.Empty(t, v.Get("includeReport"))
		assert.False(t, v.Has("includeReport"), "Default-false includeReport must not affect deterministic workflow ID hashing")
	})
}

func TestAPIGetTaskRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIGetTaskRequest
		wantErr bool
	}{
		{
			name:    "valid request",
			request: APIGetTaskRequest{SiteID: "550e8400-e29b-41d4-a716-446655440000"},
			wantErr: false,
		},
		{
			name:    "missing siteId",
			request: APIGetTaskRequest{},
			wantErr: true,
		},
		{
			name:    "empty siteId",
			request: APIGetTaskRequest{SiteID: ""},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
