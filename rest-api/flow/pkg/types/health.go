// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"reflect"
	"time"
)

// HealthReport is Flow's persisted snapshot of a Core aggregate health report.
type HealthReport struct {
	Source      string               `json:"source"`
	TriggeredBy *string              `json:"triggered_by,omitempty"`
	ObservedAt  *time.Time           `json:"observed_at,omitempty"`
	Successes   []HealthProbeSuccess `json:"successes"`
	Alerts      []HealthProbeAlert   `json:"alerts"`
}

// Equal reports whether two health snapshots have identical observable data.
func (h *HealthReport) Equal(other *HealthReport) bool {
	return reflect.DeepEqual(h, other)
}

// HealthProbeSuccess records a successful probe and its optional target.
type HealthProbeSuccess struct {
	ID     string  `json:"id"`
	Target *string `json:"target,omitempty"`
}

// HealthProbeAlert records an alert raised by a health probe.
type HealthProbeAlert struct {
	ID              string     `json:"id"`
	Target          *string    `json:"target,omitempty"`
	InAlertSince    *time.Time `json:"in_alert_since,omitempty"`
	Message         string     `json:"message"`
	TenantMessage   *string    `json:"tenant_message,omitempty"`
	Classifications []string   `json:"classifications"`
}
