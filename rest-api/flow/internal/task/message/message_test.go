/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

package message

import (
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestForStatus(t *testing.T) {
	t.Parallel()
	assert.Equal(t, "Pending", ForStatus(taskcommon.TaskStatusPending))
	assert.Equal(t, "Running", ForStatus(taskcommon.TaskStatusRunning))
	assert.Equal(t, "Succeeded", ForStatus(taskcommon.TaskStatusCompleted))
}

func TestForFailure(t *testing.T) {
	t.Parallel()
	assert.Equal(t, "disk full", ForFailure(errors.New("disk full")))
	assert.Equal(t, "stage failed", ForFailure(
		errors.New("stage failed\ncomponent type Compute failed"),
	))
}
