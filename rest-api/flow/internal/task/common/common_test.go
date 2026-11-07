// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

// --- TaskType ---

func TestTaskType_IsZero(t *testing.T) {
	assert.True(t, TaskType("").IsZero(), "empty string must be zero")
	assert.False(t, TaskTypeUnknown.IsZero(), "\"unknown\" is not the zero value")
	assert.False(t, TaskTypePowerControl.IsZero())
}

func TestTaskType_IsValid(t *testing.T) {
	valid := []TaskType{
		TaskTypeInjectExpectation,
		TaskTypePowerControl,
		TaskTypeFirmwareControl,
		TaskTypeBringUp,
	}
	for _, tt := range valid {
		assert.True(t, tt.IsValid(), "%q should be valid", tt)
	}

	invalid := []TaskType{
		TaskType(""),        // zero value
		TaskTypeUnknown,     // sentinel
		TaskType("garbage"), // unrecognised string
	}
	for _, tt := range invalid {
		assert.False(t, tt.IsValid(), "%q should not be valid", tt)
	}
}

func TestTaskTypeFromString(t *testing.T) {
	cases := []struct {
		input    string
		expected TaskType
	}{
		{"inject_expectation", TaskTypeInjectExpectation},
		{"power_control", TaskTypePowerControl},
		{"firmware_control", TaskTypeFirmwareControl},
		{"bring_up", TaskTypeBringUp},
		{"unknown", TaskTypeUnknown},
		{"", TaskTypeUnknown},
		{"garbage", TaskTypeUnknown},
	}
	for _, c := range cases {
		assert.Equal(t, c.expected, TaskTypeFromString(c.input), "input %q", c.input)
	}
}

// --- TaskStatus ---

func TestTaskStatus_IsFinished(t *testing.T) {
	finished := []TaskStatus{TaskStatusCompleted, TaskStatusFailed, TaskStatusTerminated}
	for _, s := range finished {
		assert.True(t, s.IsFinished(), "%q should be finished", s)
	}

	notFinished := []TaskStatus{
		TaskStatusUnknown, TaskStatusPending, TaskStatusRunning, TaskStatusWaiting,
	}
	for _, s := range notFinished {
		assert.False(t, s.IsFinished(), "%q should not be finished", s)
	}
}

func TestNonTerminalTaskStatuses(t *testing.T) {
	assert.Equal(t, []TaskStatus{
		TaskStatusWaiting,
		TaskStatusPending,
		TaskStatusRunning,
	}, NonTerminalTaskStatuses())
}
