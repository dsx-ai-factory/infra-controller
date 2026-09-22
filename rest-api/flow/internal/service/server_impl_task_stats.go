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

// populateTaskStats attaches one batched snapshot of non-terminal task counts
// to rack and component protobufs. Rack stats include every task persisted
// against the rack; component stats include only tasks whose attributes
// explicitly target that component.
func (rs *FlowServerImpl) populateTaskStats(
	ctx context.Context,
	racks []*pb.Rack,
	components []*pb.Component,
) error {
	rackIDs := make([]uuid.UUID, 0, len(racks)+len(components))
	seenRackIDs := make(map[uuid.UUID]struct{}, len(racks)+len(components))

	addRackID := func(id uuid.UUID) {
		if id == uuid.Nil {
			return
		}
		if _, exists := seenRackIDs[id]; exists {
			return
		}
		seenRackIDs[id] = struct{}{}
		rackIDs = append(rackIDs, id)
	}

	for _, r := range racks {
		if r == nil {
			continue
		}
		r.TaskStats = &pb.TaskStats{}
		addRackID(protobuf.UUIDFrom(r.GetInfo().GetId()))
	}
	for _, c := range components {
		if c == nil {
			continue
		}
		c.TaskStats = &pb.TaskStats{}
		addRackID(protobuf.UUIDFrom(c.GetRackId()))
	}

	// Many focused service tests construct FlowServerImpl without a store.
	// Preserve their inventory-only setup while production instances always
	// populate stats through the configured store.
	if len(rackIDs) == 0 || rs.taskStore == nil {
		return nil
	}

	tasks, err := rs.taskStore.ListNonTerminalTasksForRacks(ctx, rackIDs)
	if err != nil {
		return err
	}

	rackTaskStats := make(map[uuid.UUID]*pb.TaskStats, len(rackIDs))
	componentTaskStats := make(map[uuid.UUID]*pb.TaskStats)
	for _, task := range tasks {
		if task == nil {
			continue
		}
		rackStats := rackTaskStats[task.RackID]
		if rackStats == nil {
			rackStats = &pb.TaskStats{}
			rackTaskStats[task.RackID] = rackStats
		}
		incrementTaskStats(rackStats, task.Status)

		seenComponents := make(map[uuid.UUID]struct{})
		for _, componentID := range task.Attributes.AllComponentUUIDs() {
			if componentID == uuid.Nil {
				continue
			}
			if _, exists := seenComponents[componentID]; exists {
				continue
			}
			seenComponents[componentID] = struct{}{}
			componentStats := componentTaskStats[componentID]
			if componentStats == nil {
				componentStats = &pb.TaskStats{}
				componentTaskStats[componentID] = componentStats
			}
			incrementTaskStats(componentStats, task.Status)
		}
	}

	for _, r := range racks {
		if r == nil {
			continue
		}
		rackID := protobuf.UUIDFrom(r.GetInfo().GetId())
		if stats := rackTaskStats[rackID]; stats != nil {
			r.TaskStats = stats
		}
	}
	for _, c := range components {
		if c == nil {
			continue
		}
		componentID := protobuf.UUIDFrom(c.GetInfo().GetId())
		if stats := componentTaskStats[componentID]; stats != nil {
			c.TaskStats = stats
		}
	}

	return nil
}

func incrementTaskStats(stats *pb.TaskStats, status taskcommon.TaskStatus) {
	switch status {
	case taskcommon.TaskStatusWaiting:
		stats.WaitingTaskCount++
	case taskcommon.TaskStatusPending:
		stats.PendingTaskCount++
	case taskcommon.TaskStatusRunning:
		stats.RunningTaskCount++
	}
}
