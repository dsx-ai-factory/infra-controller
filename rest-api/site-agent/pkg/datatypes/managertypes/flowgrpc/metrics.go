// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpctypes

import "time"

type WorkflowStatus string

const (
	// WorkflowStatusSuccess workflow has completed successfully
	WorkflowStatusSuccess WorkflowStatus = "Success"
	// WorkflowStatusActFailed workflow activity execution has failed
	WorkflowStatusActivityFailed WorkflowStatus = "ActivityFailed"
	// WorkflowStatusPubFailed workflow status publish failed
	WorkflowStatusPublishFailed WorkflowStatus = "PublishFailed"
	// WorkflowStatusActPubFailed both workflow activity execution and status publish failed
	WorkflowStatusActivityPublishFailed WorkflowStatus = "ActivityPublishFailed"
)

// WorkflowMetrics defines interface to be used for workflow metrics
type WorkflowMetrics interface {
	// RecordLatency function to record latency for a workflow
	RecordLatency(activity string, status WorkflowStatus, duration time.Duration)
}
