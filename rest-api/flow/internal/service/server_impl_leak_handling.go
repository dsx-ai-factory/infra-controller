// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// populateTaskDerivedFields attaches all Task-derived component and rack data
// using batched store queries.
func (rs *FlowServerImpl) populateTaskDerivedFields(
	ctx context.Context,
	racks []*pb.Rack,
	components []*pb.Component,
) error {
	if err := rs.populateTaskStats(ctx, racks, components); err != nil {
		return err
	}

	return rs.populateLeakHandlingStatuses(ctx, racks, components)
}

// populateLeakHandlingStatuses attaches the status of the latest
// leakage-triggered forced-shutdown Task for every returned component.
func (rs *FlowServerImpl) populateLeakHandlingStatuses(
	ctx context.Context,
	racks []*pb.Rack,
	components []*pb.Component,
) error {
	allComponents := make([]*pb.Component, 0, len(components))
	for _, rack := range racks {
		if rack != nil {
			allComponents = append(allComponents, rack.GetComponents()...)
		}
	}
	allComponents = append(allComponents, components...)

	componentIDs := make([]uuid.UUID, 0, len(allComponents))
	seen := make(map[uuid.UUID]struct{}, len(allComponents))
	for _, component := range allComponents {
		if component == nil {
			continue
		}
		component.LeakHandlingStatus = pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_UNKNOWN
		id := protobuf.UUIDFrom(component.GetInfo().GetId())
		if id == uuid.Nil {
			continue
		}
		if _, exists := seen[id]; exists {
			continue
		}
		seen[id] = struct{}{}
		componentIDs = append(componentIDs, id)
	}

	if len(componentIDs) == 0 || rs.taskStore == nil {
		return nil
	}

	statuses, err := rs.taskStore.LatestLeakageShutdownTaskStatuses(ctx, componentIDs)
	if err != nil {
		return err
	}

	for _, component := range allComponents {
		if component == nil {
			continue
		}
		id := protobuf.UUIDFrom(component.GetInfo().GetId())
		if id == uuid.Nil {
			continue
		}
		status, exists := statuses[id]
		if !exists {
			component.LeakHandlingStatus = pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_NONE
			continue
		}
		component.LeakHandlingStatus = leakHandlingStatusFromTask(status)
	}

	return nil
}

func leakHandlingStatusFromTask(status taskcommon.TaskStatus) pb.LeakHandlingStatus {
	switch status {
	case taskcommon.TaskStatusWaiting,
		taskcommon.TaskStatusPending,
		taskcommon.TaskStatusRunning:
		return pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_SHUTTING_DOWN
	case taskcommon.TaskStatusCompleted:
		return pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_DOWN
	case taskcommon.TaskStatusFailed, taskcommon.TaskStatusTerminated:
		return pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_FAILED
	default:
		return pb.LeakHandlingStatus_LEAK_HANDLING_STATUS_UNKNOWN
	}
}
