// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package message

import (
	"strings"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

const maxLen = 512

// ForStatus returns the default message for a status transition. Callers may
// override Waiting / Terminated with more specific text before persisting.
func ForStatus(status taskcommon.TaskStatus) string {
	switch status {
	case taskcommon.TaskStatusWaiting:
		return "Queued: waiting for rack to become available"
	case taskcommon.TaskStatusPending:
		return "Pending"
	case taskcommon.TaskStatusRunning:
		return "Running"
	case taskcommon.TaskStatusCompleted:
		return "Succeeded"
	case taskcommon.TaskStatusFailed:
		return "Failed"
	case taskcommon.TaskStatusTerminated:
		return "Terminated"
	default:
		return ""
	}
}

// ForFailure returns a one-line failure summary suitable for the message field.
func ForFailure(err error) string {
	if err == nil {
		return ForStatus(taskcommon.TaskStatusFailed)
	}
	msg := strings.TrimSpace(err.Error())
	if msg == "" {
		return ForStatus(taskcommon.TaskStatusFailed)
	}
	idx := strings.IndexByte(msg, '\n')
	if idx >= 0 {
		msg = strings.TrimSpace(msg[:idx])
	}
	return truncate(msg)
}

func truncate(msg string) string {
	if len(msg) <= maxLen {
		return msg
	}
	return msg[:maxLen]
}
