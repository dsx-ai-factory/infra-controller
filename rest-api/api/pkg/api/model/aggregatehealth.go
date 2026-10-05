// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"time"

	cutil "github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/dsx-ai-factory/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/dsx-ai-factory/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIAggregateHealth is the REST representation of aggregate hardware health.
type APIAggregateHealth struct {
	Source     string                  `json:"source"`
	ObservedAt *string                 `json:"observedAt"`
	Successes  []APIHealthProbeSuccess `json:"successes"`
	Alerts     []APIHealthProbeAlert   `json:"alerts"`
}

// FromProto populates an APIAggregateHealth from its Core protobuf form.
func (mh *APIAggregateHealth) FromProto(protoHealth *corev1.HealthReport) {
	if protoHealth == nil {
		return
	}

	converted := APIAggregateHealth{
		Source:    protoHealth.GetSource(),
		Successes: make([]APIHealthProbeSuccess, 0, len(protoHealth.GetSuccesses())),
		Alerts:    make([]APIHealthProbeAlert, 0, len(protoHealth.GetAlerts())),
	}
	if protoHealth.ObservedAt != nil {
		observed := protoHealth.ObservedAt.AsTime().Format(time.RFC3339)
		converted.ObservedAt = cutil.GetPtr(observed)
	}

	for _, alert := range protoHealth.Alerts {
		if alert == nil {
			continue
		}
		ahpa := APIHealthProbeAlert{}
		ahpa.FromProto(alert)
		converted.Alerts = append(converted.Alerts, ahpa)
	}

	for _, success := range protoHealth.Successes {
		if success == nil {
			continue
		}
		ahps := APIHealthProbeSuccess{}
		ahps.FromProto(success)
		converted.Successes = append(converted.Successes, ahps)
	}

	*mh = converted
}

// FromFlowProto populates APIAggregateHealth from Flow's synchronized Core snapshot.
func (mh *APIAggregateHealth) FromFlowProto(protoHealth *flowv1.HealthReport) {
	if protoHealth == nil {
		return
	}
	convertedHealth := APIAggregateHealth{
		Source:    protoHealth.GetSource(),
		Successes: make([]APIHealthProbeSuccess, 0, len(protoHealth.GetSuccesses())),
		Alerts:    make([]APIHealthProbeAlert, 0, len(protoHealth.GetAlerts())),
	}
	if protoHealth.GetObservedAt() != nil {
		observed := protoHealth.GetObservedAt().AsTime().Format(time.RFC3339)
		convertedHealth.ObservedAt = cutil.GetPtr(observed)
	}
	for _, success := range protoHealth.GetSuccesses() {
		if success == nil {
			continue
		}
		convertedHealth.Successes = append(convertedHealth.Successes, APIHealthProbeSuccess{
			ID:     success.GetId(),
			Target: success.Target,
		})
	}
	for _, alert := range protoHealth.GetAlerts() {
		if alert == nil {
			continue
		}
		converted := APIHealthProbeAlert{
			ID:              alert.GetId(),
			Target:          alert.Target,
			Message:         alert.GetMessage(),
			TenantMessage:   alert.TenantMessage,
			Classifications: append([]string{}, alert.GetClassifications()...),
		}
		if alert.GetInAlertSince() != nil {
			inAlertSince := alert.GetInAlertSince().AsTime().Format(time.RFC3339)
			converted.InAlertSince = cutil.GetPtr(inAlertSince)
		}
		convertedHealth.Alerts = append(convertedHealth.Alerts, converted)
	}

	*mh = convertedHealth
}

// FromDBModel populates an APIAggregateHealth from its DB model form.
func (mh *APIAggregateHealth) FromDBModel(machineHealth *cdbm.MachineHealth) {
	if machineHealth == nil {
		return
	}

	converted := APIAggregateHealth{
		Source:     machineHealth.Source,
		ObservedAt: machineHealth.ObservedAt,
		Successes:  make([]APIHealthProbeSuccess, 0, len(machineHealth.Successes)),
		Alerts:     make([]APIHealthProbeAlert, 0, len(machineHealth.Alerts)),
	}
	for _, alert := range machineHealth.Alerts {
		ahpa := APIHealthProbeAlert{}
		ahpa.FromDBModel(alert)
		converted.Alerts = append(converted.Alerts, ahpa)
	}
	for _, success := range machineHealth.Successes {
		ahps := APIHealthProbeSuccess{}
		ahps.FromDBModel(success)
		converted.Successes = append(converted.Successes, ahps)
	}

	*mh = converted
}

// APIHealthProbeSuccess is the REST representation of a successful health probe.
type APIHealthProbeSuccess struct {
	ID     string  `json:"id"`
	Target *string `json:"target"`
}

// FromProto populates an APIHealthProbeSuccess from its protobuf form.
func (ahps *APIHealthProbeSuccess) FromProto(protoSuccess *corev1.HealthProbeSuccess) {
	if protoSuccess == nil {
		return
	}
	*ahps = APIHealthProbeSuccess{
		ID:     protoSuccess.GetId(),
		Target: protoSuccess.Target,
	}
}

// ToProto populates a protobuf form of an APIHealthProbeSuccess from its API form.
func (ahps APIHealthProbeSuccess) ToProto() *corev1.HealthProbeSuccess {
	return &corev1.HealthProbeSuccess{
		Id:     ahps.ID,
		Target: ahps.Target,
	}
}

// FromDBModel populates an APIHealthProbeSuccess from its DB model form.
func (ahps *APIHealthProbeSuccess) FromDBModel(success cdbm.HealthProbeSuccess) {
	*ahps = APIHealthProbeSuccess{
		ID:     success.Id,
		Target: success.Target,
	}
}

// APIHealthProbeAlert is the REST representation of a health probe alert.
type APIHealthProbeAlert struct {
	ID              string   `json:"id"`
	Target          *string  `json:"target"`
	InAlertSince    *string  `json:"inAlertSince"`
	Message         string   `json:"message"`
	TenantMessage   *string  `json:"tenantMessage"`
	Classifications []string `json:"classifications"`
}

// FromProto populates an APIHealthProbeAlert from its protobuf form.
func (ahpa *APIHealthProbeAlert) FromProto(protoAlert *corev1.HealthProbeAlert) {
	if protoAlert == nil {
		return
	}
	converted := APIHealthProbeAlert{
		ID:              protoAlert.GetId(),
		Target:          protoAlert.Target,
		Message:         protoAlert.GetMessage(),
		TenantMessage:   protoAlert.TenantMessage,
		Classifications: append([]string{}, protoAlert.GetClassifications()...),
	}
	if protoAlert.InAlertSince != nil {
		inAlertSince := protoAlert.InAlertSince.AsTime().Format(time.RFC3339)
		converted.InAlertSince = cutil.GetPtr(inAlertSince)
	}
	*ahpa = converted
}

// ToProto populates a protobuf form of an APIHealthProbeAlert from its API form.
func (ahpa APIHealthProbeAlert) ToProto() *corev1.HealthProbeAlert {
	return &corev1.HealthProbeAlert{
		Id:              ahpa.ID,
		Target:          ahpa.Target,
		InAlertSince:    cutil.StrPtrToProtoTimePtr(ahpa.InAlertSince),
		Message:         ahpa.Message,
		TenantMessage:   ahpa.TenantMessage,
		Classifications: ahpa.Classifications,
	}
}

// FromDBModel populates an APIHealthProbeAlert from its DB model form.
func (ahpa *APIHealthProbeAlert) FromDBModel(alert cdbm.HealthProbeAlert) {
	*ahpa = APIHealthProbeAlert{
		ID:              alert.Id,
		Target:          alert.Target,
		Message:         alert.Message,
		InAlertSince:    alert.InAlertSince,
		TenantMessage:   alert.TenantMessage,
		Classifications: append([]string{}, alert.Classifications...),
	}
}
