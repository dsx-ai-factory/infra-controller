// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package taskschedule

import (
	"encoding/json"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// TaskTemplate is the JSON-serialized operation stored in task_schedule.operation_template.
// It carries the operation type, code, and info needed to reconstruct an operation.Request
// at fire time. The target is resolved from task_schedule_scope rows at fire time,
// so it is not stored here.
type TaskTemplate struct {
	Type             taskcommon.TaskType `json:"type"`
	Code             string              `json:"code"`
	Info             json.RawMessage     `json:"information"`
	ConflictStrategy int                 `json:"conflict_strategy,omitempty"`
	QueueTimeoutSecs int64               `json:"queue_timeout_secs,omitempty"`
	RuleID           string              `json:"rule_id,omitempty"`
}

// TemplateOptions holds the scheduling-policy fields stored alongside the
// operation in a TaskTemplate. They are passed in at creation time and restored
// at fire time to reconstruct the full operation.Request.
type TemplateOptions struct {
	// ConflictStrategy is operation.ConflictStrategy stored as its underlying
	// int value. 0 = ConflictStrategyReject (default), 1 = ConflictStrategyQueue.
	ConflictStrategy int
	// QueueTimeoutSecs is operation.Request.QueueTimeout expressed in seconds.
	// Zero means use the server default.
	QueueTimeoutSecs int64
	// RuleID is the override rule UUID as a string. Empty string means no override.
	RuleID string
}

// WrapperFromTemplate unmarshals an operation_template JSON blob and returns
// the corresponding operation.Wrapper.
func WrapperFromTemplate(raw json.RawMessage) (operation.Wrapper, error) {
	var tmpl TaskTemplate
	if err := json.Unmarshal(raw, &tmpl); err != nil {
		return operation.Wrapper{}, fmt.Errorf("unmarshal operation_template: %w", err)
	}

	return operation.Wrapper{
		Type: tmpl.Type,
		Code: tmpl.Code,
		Info: tmpl.Info,
	}, nil
}

// MarshalTemplate serializes an operation into a TaskTemplate JSON blob
// suitable for storing in task_schedule.operation_template.
func MarshalTemplate(
	opType taskcommon.TaskType,
	code string,
	info json.RawMessage,
	opts TemplateOptions,
) (json.RawMessage, error) {
	tmpl := TaskTemplate{
		Type:             opType,
		Code:             code,
		Info:             info,
		ConflictStrategy: opts.ConflictStrategy,
		QueueTimeoutSecs: opts.QueueTimeoutSecs,
		RuleID:           opts.RuleID,
	}

	return json.Marshal(tmpl)
}

// OptionsFromTemplate extracts the scheduling-policy fields from a stored
// TaskTemplate JSON blob. These are restored at fire time to reconstruct the
// full operation.Request (conflict strategy, queue timeout, rule override).
func OptionsFromTemplate(raw json.RawMessage) (TemplateOptions, error) {
	var tmpl TaskTemplate
	if err := json.Unmarshal(raw, &tmpl); err != nil {
		return TemplateOptions{}, fmt.Errorf("unmarshal operation_template: %w", err)
	}

	return TemplateOptions{
		ConflictStrategy: tmpl.ConflictStrategy,
		QueueTimeoutSecs: tmpl.QueueTimeoutSecs,
		RuleID:           tmpl.RuleID,
	}, nil
}

// SummaryFromTemplate derives the operation_type and human-readable description
// that are surfaced on a TaskSchedule response. Both values are derived entirely
// from the stored operation_template so no live operation object is needed.
//
// opType is a stable SCREAMING_SNAKE_CASE string suitable for client filtering
// (e.g. "POWER_ON", "BRING_UP"). description is a short English phrase for
// display (e.g. "Power Off (forced)", "Upgrade Firmware to v2.1.0").
func SummaryFromTemplate(templateJSON json.RawMessage) (opType, description string, err error) {
	w, err := WrapperFromTemplate(templateJSON)
	if err != nil {
		return "", "", err
	}

	description, err = operations.SummaryFromWrapper(w)
	if err != nil {
		return "", "", err
	}

	return operations.OperationTypeFromWrapper(w), description, nil
}
