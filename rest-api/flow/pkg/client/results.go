// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

// UpgradeFirmwareResult represents the result of a firmware upgrade operation.
type UpgradeFirmwareResult struct {
	TaskIDs []uuid.UUID // Multiple task IDs (1 task per rack)
}

// PowerControlResult represents the result of a power control operation.
type PowerControlResult struct {
	TaskIDs []uuid.UUID // Multiple task IDs (1 task per rack)
}

// GetExpectedComponentsResult contains the result of GetExpectedComponents operation.
type GetExpectedComponentsResult struct {
	Components []*types.Component
	Total      int
}

// ValidateComponentsResult represents the result of ValidateComponents call.
type ValidateComponentsResult struct {
	Diffs           []*types.ComponentDiff
	TotalDiffs      int
	MissingCount    int
	UnexpectedCount int
	MismatchCount   int
	MatchCount      int
}

// IngestRackResult represents the result of an IngestRack operation.
type IngestRackResult struct {
	TaskIDs []uuid.UUID
}

// DecommissionRackResult represents the result of a DecommissionRack call.
type DecommissionRackResult struct {
	TaskIDs []uuid.UUID
}

// ListTasksResult represents the result of ListTasks call.
type ListTasksResult struct {
	Tasks []*types.Task
	Total int
}
