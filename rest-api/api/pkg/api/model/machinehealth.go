// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
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

// Time when the deprecated observed_at attribute stops being returned.
// Same date as the other snake_case alias removals.
var machineHealthObservedAtDeprecationTime = time.Date(2026, time.September, 10, 0, 0, 0, 0, time.UTC)

// APIMachineHealth is the data structure to capture API representation of a Machine's health Info
type APIMachineHealth struct {
	Source               string                         `json:"source"`
	ObservedAt           *string                        `json:"observedAt"`
	ObservedAtDeprecated *string                        `json:"observed_at,omitempty"`
	Successes            []APIMachineHealthProbeSuccess `json:"successes"`
	Alerts               []APIMachineHealthProbeAlert   `json:"alerts"`
}

// FromProto populates an APIMachineHealth from its protobuf form.
func (mh *APIMachineHealth) FromProto(protoHealth *corev1.HealthReport) {
	if protoHealth == nil {
		return
	}

	mh.Source = protoHealth.Source
	if protoHealth.ObservedAt != nil {
		observed := protoHealth.ObservedAt.AsTime().Format(time.RFC3339)
		mh.ObservedAt = cutil.GetPtr(observed)
		if time.Now().Before(machineHealthObservedAtDeprecationTime) {
			mh.ObservedAtDeprecated = cutil.GetPtr(observed)
		}
	}

	mh.Alerts = []APIMachineHealthProbeAlert{}
	for _, alert := range protoHealth.Alerts {
		if alert == nil {
			continue
		}
		ahpa := APIMachineHealthProbeAlert{}
		ahpa.FromProto(alert)
		mh.Alerts = append(mh.Alerts, ahpa)
	}

	mh.Successes = []APIMachineHealthProbeSuccess{}
	for _, success := range protoHealth.Successes {
		if success == nil {
			continue
		}
		ahps := APIMachineHealthProbeSuccess{}
		ahps.FromProto(success)
		mh.Successes = append(mh.Successes, ahps)
	}
}

// FromDBModel populates an APIMachineHealth from its DB model form.
func (mh *APIMachineHealth) FromDBModel(machineHealth *cdbm.MachineHealth) {
	if machineHealth == nil {
		return
	}

	mh.Source = machineHealth.Source
	mh.ObservedAt = machineHealth.ObservedAt
	if time.Now().Before(machineHealthObservedAtDeprecationTime) {
		mh.ObservedAtDeprecated = machineHealth.ObservedAt
	}

	if len(machineHealth.Alerts) > 0 {
		mh.Alerts = []APIMachineHealthProbeAlert{}
		for _, alert := range machineHealth.Alerts {
			ahpa := APIMachineHealthProbeAlert{}
			ahpa.FromDBModel(alert)
			mh.Alerts = append(mh.Alerts, ahpa)
		}
	}

	if len(machineHealth.Successes) > 0 {
		mh.Successes = []APIMachineHealthProbeSuccess{}
		for _, success := range machineHealth.Successes {
			ahps := APIMachineHealthProbeSuccess{}
			ahps.FromDBModel(success)
			mh.Successes = append(mh.Successes, ahps)
		}
	}
}

// APIMachineHealthProbeSuccess is the data structure to capture API representation of a Machine's Health Probe Success information
type APIMachineHealthProbeSuccess struct {
	ID     string  `json:"id"`
	Target *string `json:"target"`
}

// FromProto populates an APIMachineHealthProbeSuccess from its protobuf form.
func (ahps *APIMachineHealthProbeSuccess) FromProto(protoSuccess *corev1.HealthProbeSuccess) {
	if protoSuccess == nil {
		return
	}
	ahps.ID = protoSuccess.Id
	ahps.Target = protoSuccess.Target
}

// ToProto populates a protobuf form of an APIMachineHealthProbeSuccess from its API form.
func (ahps APIMachineHealthProbeSuccess) ToProto() *corev1.HealthProbeSuccess {
	return &corev1.HealthProbeSuccess{
		Id:     ahps.ID,
		Target: ahps.Target,
	}
}

// FromDBModel populates an APIMachineHealthProbeSuccess from its DB model form.
func (ahps *APIMachineHealthProbeSuccess) FromDBModel(success cdbm.HealthProbeSuccess) {
	ahps.ID = success.Id
	ahps.Target = success.Target
}

// APIMachineHealthProbeAlert is the data structure to capture API representation of a Machine's Health Probe Alert information
type APIMachineHealthProbeAlert struct {
	ID              string   `json:"id"`
	Target          *string  `json:"target"`
	InAlertSince    *string  `json:"inAlertSince"`
	Message         string   `json:"message"`
	TenantMessage   *string  `json:"tenantMessage"`
	Classifications []string `json:"classifications"`
}

// FromProto populates an APIMachineHealthProbeAlert from its protobuf form.
func (ahpa *APIMachineHealthProbeAlert) FromProto(protoAlert *corev1.HealthProbeAlert) {
	if protoAlert == nil {
		return
	}
	ahpa.ID = protoAlert.Id
	ahpa.Target = protoAlert.Target
	ahpa.Message = protoAlert.Message
	if protoAlert.InAlertSince != nil {
		inAlertSince := protoAlert.InAlertSince.AsTime().Format(time.RFC3339)
		ahpa.InAlertSince = cutil.GetPtr(inAlertSince)
	}
	ahpa.TenantMessage = protoAlert.TenantMessage
	ahpa.Classifications = protoAlert.Classifications
}

// ToProto populates a protobuf form of an APIMachineHealthProbeAlert from its API form.
func (ahpa APIMachineHealthProbeAlert) ToProto() *corev1.HealthProbeAlert {
	return &corev1.HealthProbeAlert{
		Id:              ahpa.ID,
		Target:          ahpa.Target,
		InAlertSince:    cutil.StrPtrToProtoTimePtr(ahpa.InAlertSince),
		Message:         ahpa.Message,
		TenantMessage:   ahpa.TenantMessage,
		Classifications: ahpa.Classifications,
	}
}

// FromDBModel populates an APIMachineHealthProbeAlert from its DB model form.
func (ahpa *APIMachineHealthProbeAlert) FromDBModel(alert cdbm.HealthProbeAlert) {
	ahpa.ID = alert.Id
	ahpa.Target = alert.Target
	ahpa.Message = alert.Message
	ahpa.InAlertSince = alert.InAlertSince
	ahpa.TenantMessage = alert.TenantMessage
	ahpa.Classifications = alert.Classifications
}

// APIMachineHealthReportEntry is the API representation of a Machine health report override entry.
type APIMachineHealthReportEntry struct {
	Source      string                         `json:"source"`
	TriggeredBy *string                        `json:"triggeredBy"`
	ObservedAt  *string                        `json:"observedAt"`
	Successes   []APIMachineHealthProbeSuccess `json:"successes"`
	Alerts      []APIMachineHealthProbeAlert   `json:"alerts"`
	Mode        MachineHealthReportMode        `json:"mode"`
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

	amhre.Successes = []APIMachineHealthProbeSuccess{}
	for _, protoSuccess := range report.GetSuccesses() {
		if protoSuccess == nil {
			continue
		}
		success := APIMachineHealthProbeSuccess{}
		success.FromProto(protoSuccess)
		amhre.Successes = append(amhre.Successes, success)
	}

	amhre.Alerts = []APIMachineHealthProbeAlert{}
	for _, protoAlert := range report.GetAlerts() {
		if protoAlert == nil {
			continue
		}
		alert := APIMachineHealthProbeAlert{}
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
	Source    string                         `json:"source"`
	Successes []APIMachineHealthProbeSuccess `json:"successes"`
	Alerts    []APIMachineHealthProbeAlert   `json:"alerts"`
	Mode      MachineHealthReportMode        `json:"mode"`
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
	observedAt := time.Now().Format(time.RFC3339Nano)

	protoRequest := &corev1.InsertMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		HealthReportEntry: &corev1.HealthReportEntry{
			Report: &corev1.HealthReport{
				Source:      amhrer.Source,
				TriggeredBy: cutil.GetPtr(triggeredBy.ID.String()),
				ObservedAt:  cutil.StrPtrToProtoTimePtr(&observedAt),
			},
			Mode: amhrer.Mode.ToProto(),
		},
	}

	for _, success := range amhrer.Successes {
		protoRequest.HealthReportEntry.Report.Successes = append(protoRequest.HealthReportEntry.Report.Successes, success.ToProto())
	}

	for _, alert := range amhrer.Alerts {
		protoRequest.HealthReportEntry.Report.Alerts = append(protoRequest.HealthReportEntry.Report.Alerts, alert.ToProto())
	}

	return protoRequest
}
