// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package alert provides an abstraction for sending alerts/notifications
// from Flow workflows and activities. The concrete implementation can be
// replaced later (Slack, PagerDuty, etc.).
package alert

import (
	"context"
	"fmt"

	"github.com/rs/zerolog/log"
)

// Severity represents the urgency level of an alert.
type Severity string

const (
	SeverityInfo     Severity = "info"
	SeverityWarning  Severity = "warning"
	SeverityCritical Severity = "critical"
)

// Alert represents a single alert to be sent through the alerting system.
type Alert struct {
	Severity  Severity          `json:"severity"`
	Message   string            `json:"message"`
	Component string            `json:"component,omitempty"`
	Operation string            `json:"operation,omitempty"`
	TaskID    string            `json:"task_id,omitempty"`
	Details   map[string]string `json:"details,omitempty"`
}

func (a Alert) String() string {
	return fmt.Sprintf("[%s] %s (component=%s, operation=%s, task=%s)",
		a.Severity, a.Message, a.Component, a.Operation, a.TaskID)
}

// Send delivers an alert. Currently just logs it.
// TODO: Replace with real alerting backend (Slack, PagerDuty, etc.) when ready.
func Send(_ context.Context, a Alert) error {
	log.Warn().
		Str("severity", string(a.Severity)).
		Str("component", a.Component).
		Str("operation", a.Operation).
		Str("task_id", a.TaskID).
		Msg("ALERT: " + a.Message)
	return nil
}
