// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"encoding/json"
	"errors"
	"fmt"
)

// ExecutionID identifies a running or completed Temporal workflow execution.
// It is serialized to JSON and stored as the opaque executor execution ID
// returned to callers after a task is dispatched.
type ExecutionID struct {
	WorkflowID string `json:"workflow_id"`
	RunID      string `json:"run_id"`
}

// NewExecutionID constructs a validated ExecutionID. Both workflowID and runID
// are required; returns an error if either is empty.
func NewExecutionID(workflowID string, runID string) (*ExecutionID, error) {
	if workflowID == "" {
		return nil, errors.New("workflow ID is required")
	}

	if runID == "" {
		return nil, errors.New("run ID is required")
	}

	return &ExecutionID{
		WorkflowID: workflowID,
		RunID:      runID,
	}, nil
}

// NewFromEncoded deserializes an ExecutionID from the JSON string produced by Encode.
func NewFromEncoded(encoded string) (*ExecutionID, error) {
	var id ExecutionID

	err := json.Unmarshal([]byte(encoded), &id)
	if err != nil {
		return nil, fmt.Errorf(
			"failed to decode execution ID from JSON %s: %v",
			encoded,
			err,
		)
	}

	return &id, nil
}

// Encode serializes the ExecutionID to a JSON string suitable for storage and
// later recovery via NewFromEncoded.
func (id *ExecutionID) Encode() (string, error) {
	jsonBytes, err := json.Marshal(id)
	if err != nil {
		return "", err
	}

	return string(jsonBytes), nil
}

// String returns a human-readable representation for logging.
func (id *ExecutionID) String() string {
	return fmt.Sprintf("[workflowID: %s, runID: %s]", id.WorkflowID, id.RunID)
}
