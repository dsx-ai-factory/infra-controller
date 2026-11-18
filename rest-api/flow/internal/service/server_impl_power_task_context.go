// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"errors"
	"slices"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// addPowerTaskWaitingContext reads metadata once for all Waiting power tasks in
// this rack. A failed context read must not discard the power tasks themselves.
// Tasks and entries are the corresponding, ordered results of readRackPowerTasks.
func (rs *FlowServerImpl) addPowerTaskWaitingContext(ctx context.Context, rackID uuid.UUID, tasks []taskdef.PowerTaskDetails, entries []*pb.PowerTaskDetails) {
	hasWaiting := slices.ContainsFunc(tasks, func(task taskdef.PowerTaskDetails) bool {
		return task.Status == taskcommon.TaskStatusWaiting
	})
	if !hasWaiting {
		return
	}
	metadata, err := rs.taskStore.ListTaskContextForRack(ctx, rackID)
	if ctx.Err() != nil {
		err = ctx.Err()
	}
	for i, task := range tasks {
		if task.Status != taskcommon.TaskStatusWaiting {
			continue
		}
		if err != nil {
			code := powerDetailsReadError(err)
			var limitError *taskcommon.TaskContextLimitError
			if errors.As(err, &limitError) {
				code = pb.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_LIMIT_EXCEEDED
			}
			entries[i].WaitingContext = &pb.PowerTaskWaitingContext{Error: &code}
			continue
		}
		entries[i].WaitingContext = powerTaskWaitingContext(task, metadata)
	}
}

// powerTaskWaitingContext separates current conflicts from earlier waiting work.
// The promoter stops at the first conflicting waiting task, so even an earlier
// task with disjoint targets is relevant. Neither list proves why a task waited.
func powerTaskWaitingContext(task taskdef.PowerTaskDetails, metadata []taskdef.TaskContextMetadata) *pb.PowerTaskWaitingContext {
	result := &pb.PowerTaskWaitingContext{}
	incoming := taskdef.TaskContextMetadata{RackID: task.RackID, Type: taskcommon.TaskTypePowerControl, Attributes: task.Attributes}
	for _, other := range metadata {
		if other.ID == task.ID {
			continue
		}
		brief := &pb.TaskBrief{
			Id: protobuf.UUIDTo(other.ID), Kind: protobuf.TaskKindTo(other.Type), Status: protobuf.TaskStatusTo(other.Status),
		}
		switch other.Status {
		case taskcommon.TaskStatusPending, taskcommon.TaskStatusRunning:
			if conflict.MetadataConflicts(incoming, other) {
				result.ActiveConflicts = append(result.ActiveConflicts, brief)
			}
		case taskcommon.TaskStatusWaiting:
			// Equal timestamps do not tell us which task was submitted first.
			if other.CreatedAt.Before(task.CreatedAt) {
				result.EarlierWaiting = append(result.EarlierWaiting, brief)
			}
		}
	}
	return result
}
