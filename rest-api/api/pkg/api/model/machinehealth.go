// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

// MachineHealthReportMode is the API-facing apply mode for a Machine health report override.
type MachineHealthReportMode string

const (
	// MachineHealthReportModeMerge merges a health report override with the current Machine health report.
	MachineHealthReportModeMerge MachineHealthReportMode = "Merge"
	// MachineHealthReportModeReplace replaces the current Machine health report with the override.
	MachineHealthReportModeReplace MachineHealthReportMode = "Replace"
)

// ToProto converts a MachineHealthReportMode to its protobuf form.
func (mhm MachineHealthReportMode) ToProto() corev1.HealthReportApplyMode {
	switch mhm {
	case MachineHealthReportModeMerge:
		return corev1.HealthReportApplyMode_Merge
	case MachineHealthReportModeReplace:
		return corev1.HealthReportApplyMode_Replace
	}
	return corev1.HealthReportApplyMode_Merge
}

// FromProto converts a protobuf health report apply mode to its API form.
func (mhm MachineHealthReportMode) FromProto(mode corev1.HealthReportApplyMode) MachineHealthReportMode {
	switch mode {
	case corev1.HealthReportApplyMode_Merge:
		return MachineHealthReportModeMerge
	case corev1.HealthReportApplyMode_Replace:
		return MachineHealthReportModeReplace
	}
	return MachineHealthReportModeMerge
}

// APIMachineHealth is the Machine API representation of aggregate hardware health.
// ObservedAtDeprecated preserves the legacy snake_case field on Machine responses.
type APIMachineHealth struct {
	APIAggregateHealth
	ObservedAtDeprecated *string `json:"observed_at"`
}

// FromProto populates an APIMachineHealth from its Core protobuf form.
func (mh *APIMachineHealth) FromProto(protoHealth *corev1.HealthReport) {
	if protoHealth == nil {
		return
	}
	var aggregate APIAggregateHealth
	aggregate.FromProto(protoHealth)
	*mh = APIMachineHealth{
		APIAggregateHealth:   aggregate,
		ObservedAtDeprecated: aggregate.ObservedAt,
	}
}

// FromDBModel populates an APIMachineHealth from its REST DB model form.
func (mh *APIMachineHealth) FromDBModel(machineHealth *cdbm.MachineHealth) {
	if machineHealth == nil {
		return
	}
	var aggregate APIAggregateHealth
	aggregate.FromDBModel(machineHealth)
	*mh = APIMachineHealth{
		APIAggregateHealth:   aggregate,
		ObservedAtDeprecated: aggregate.ObservedAt,
	}
}

// APIMachineHealthReportEntry is the API representation of a Machine health report override entry.
type APIMachineHealthReportEntry struct {
	Source      string                  `json:"source"`
	TriggeredBy *string                 `json:"triggeredBy"`
	ObservedAt  *string                 `json:"observedAt"`
	Successes   []APIHealthProbeSuccess `json:"successes"`
	Alerts      []APIHealthProbeAlert   `json:"alerts"`
	Mode        MachineHealthReportMode `json:"mode"`
}

// FromProto populates an APIMachineHealthReportEntry from its protobuf form.
func (amhre *APIMachineHealthReportEntry) FromProto(entry *corev1.HealthReportEntry) {
	report := entry.GetReport()
	if report == nil {
		return
	}
	amhre.Source = report.GetSource()
	amhre.TriggeredBy = cutil.GetPtr(report.GetTriggeredBy())
	amhre.ObservedAt = cutil.ProtoTimePtrToStrPtr(report.GetObservedAt())

	amhre.Successes = []APIHealthProbeSuccess{}
	for _, protoSuccess := range report.GetSuccesses() {
		if protoSuccess == nil {
			continue
		}
		success := APIHealthProbeSuccess{}
		success.FromProto(protoSuccess)
		amhre.Successes = append(amhre.Successes, success)
	}

	amhre.Alerts = []APIHealthProbeAlert{}
	for _, protoAlert := range report.GetAlerts() {
		if protoAlert == nil {
			continue
		}
		alert := APIHealthProbeAlert{}
		alert.FromProto(protoAlert)
		amhre.Alerts = append(amhre.Alerts, alert)
	}

	amhre.Mode = MachineHealthReportMode("").FromProto(entry.GetMode())
}

// ToProto converts an APIMachineHealthReportEntry to its protobuf form.
func (amhre APIMachineHealthReportEntry) ToProto() *corev1.HealthReportEntry {
	successes := make([]*corev1.HealthProbeSuccess, 0, len(amhre.Successes))
	for _, success := range amhre.Successes {
		successes = append(successes, success.ToProto())
	}

	alerts := make([]*corev1.HealthProbeAlert, 0, len(amhre.Alerts))
	for _, alert := range amhre.Alerts {
		alerts = append(alerts, alert.ToProto())
	}

	return &corev1.HealthReportEntry{
		Report: &corev1.HealthReport{
			Source:      amhre.Source,
			TriggeredBy: amhre.TriggeredBy,
			ObservedAt:  cutil.StrPtrToProtoTimePtr(amhre.ObservedAt),
			Successes:   successes,
			Alerts:      alerts,
		},
		Mode: amhre.Mode.ToProto(),
	}
}

// APIMachineHealthReportEntryRequest is the data structure to capture API representation of a Machine's Health Report Entry request
type APIMachineHealthReportEntryRequest struct {
	Source    string                  `json:"source"`
	Successes []APIHealthProbeSuccess `json:"successes"`
	Alerts    []APIHealthProbeAlert   `json:"alerts"`
	Mode      MachineHealthReportMode `json:"mode"`
}

// APIRackHealthReportEntryRequest is the request body for a Rack health report override.
type APIRackHealthReportEntryRequest struct {
	APIMachineHealthReportEntryRequest
	SiteID string `json:"siteId"`
}

// Validate validates a Rack health report override request.
func (r *APIRackHealthReportEntryRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error("siteId is required"),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
	); err != nil {
		return err
	}
	return r.APIMachineHealthReportEntryRequest.Validate()
}

// APITrayHealthReportEntryRequest is the request body for a Tray health report override.
type APITrayHealthReportEntryRequest struct {
	APIMachineHealthReportEntryRequest
	SiteID string `json:"siteId"`
	Type   string `json:"type"`
}

// Validate validates a Tray health report override request.
func (r *APITrayHealthReportEntryRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error("siteId is required"),
			validationis.UUID.Error(validationErrorInvalidUUID),
		),
		validation.Field(&r.Type,
			validation.Required.Error("type is required"),
			validation.In(validTrayTypesAny...).Error(
				fmt.Sprintf("type must be one of %s", strings.Join(ValidTrayTypeNames(), ", ")),
			),
		),
	); err != nil {
		return err
	}
	return r.APIMachineHealthReportEntryRequest.Validate()
}

// Validate ensures the Machine health report entry request is acceptable.
func (amhrer *APIMachineHealthReportEntryRequest) Validate() error {
	err := validation.ValidateStruct(amhrer,
		validation.Field(&amhrer.Source, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&amhrer.Mode,
			validation.Required.Error(validationErrorValueRequired),
			validation.In(MachineHealthReportModeMerge, MachineHealthReportModeReplace).Error(
				fmt.Sprintf("must be one of %v", []MachineHealthReportMode{MachineHealthReportModeMerge, MachineHealthReportModeReplace}))),
	)

	if err != nil {
		return err
	}

	for i := range amhrer.Successes {
		err = validation.ValidateStruct(&amhrer.Successes[i],
			validation.Field(&amhrer.Successes[i].ID, validation.Required.Error(validationErrorValueRequired)),
		)
		if err != nil {
			return validation.Errors{
				"successes": fmt.Errorf("invalid entry at index %d: %w", i, err),
			}
		}
	}
	for i := range amhrer.Alerts {
		err = validation.ValidateStruct(&amhrer.Alerts[i],
			validation.Field(&amhrer.Alerts[i].ID, validation.Required.Error(validationErrorValueRequired)),
			validation.Field(&amhrer.Alerts[i].Message, validation.Required.Error(validationErrorValueRequired)),
			validation.Field(&amhrer.Alerts[i].InAlertSince, validation.By(util.ValidateStrPtrTime)),
		)
		if err != nil {
			return validation.Errors{
				"alerts": fmt.Errorf("invalid entry at index %d: %w", i, err),
			}
		}
	}
	return nil
}

// ToProto converts an APIMachineHealthReportEntryRequest to its protobuf form.
func (amhrer APIMachineHealthReportEntryRequest) ToProto(machineID string, triggeredBy *cdbm.User) *corev1.InsertMachineHealthReportRequest {
	return &corev1.InsertMachineHealthReportRequest{
		MachineId:         &corev1.MachineId{Id: machineID},
		HealthReportEntry: amhrer.ToHealthReportEntryProto(triggeredBy),
	}
}

// ToHealthReportEntryProto converts an APIMachineHealthReportEntryRequest to the shared
// protobuf entry used by Machine, Rack, Switch, and Power Shelf health report RPCs.
func (amhrer APIMachineHealthReportEntryRequest) ToHealthReportEntryProto(triggeredBy *cdbm.User) *corev1.HealthReportEntry {
	observedAt := time.Now().Format(time.RFC3339Nano)

	protoEntry := &corev1.HealthReportEntry{
		Report: &corev1.HealthReport{
			Source:      amhrer.Source,
			TriggeredBy: cutil.GetPtr(triggeredBy.ID.String()),
			ObservedAt:  cutil.StrPtrToProtoTimePtr(&observedAt),
		},
		Mode: amhrer.Mode.ToProto(),
	}

	for _, success := range amhrer.Successes {
		protoEntry.Report.Successes = append(protoEntry.Report.Successes, success.ToProto())
	}

	for _, alert := range amhrer.Alerts {
		protoEntry.Report.Alerts = append(protoEntry.Report.Alerts, alert.ToProto())
	}

	return protoEntry
}
