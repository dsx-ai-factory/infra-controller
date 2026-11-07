// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rackopreport

import (
	"encoding/json"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
)

type RackOpReport struct {
	id         uuid.UUID
	serialInfo deviceinfo.SerialInfo
	report     string

	components map[string]*componentReport
}

type componentReport struct {
	id         uuid.UUID
	serialInfo deviceinfo.SerialInfo
	report     string
	bmcReports map[string]string
}

func New(id uuid.UUID, serialInfo deviceinfo.SerialInfo) *RackOpReport {
	return &RackOpReport{
		id:         id,
		serialInfo: serialInfo,
		components: make(map[string]*componentReport),
	}
}

func (rr *RackOpReport) UpdateReport(report string) {
	rr.report = report
}

func (rr *RackOpReport) UpdateCompReport(
	id uuid.UUID,
	serialInfo deviceinfo.SerialInfo,
	report string,
) {
	rid := compReportID(id, serialInfo)
	if cr := rr.components[rid]; cr != nil {
		cr.report = report
		return
	}

	rr.components[rid] = &componentReport{
		id:         id,
		serialInfo: serialInfo,
		report:     report,
		bmcReports: make(map[string]string),
	}
}

func (rr *RackOpReport) UpdateBMCReport(
	id uuid.UUID,
	serialInfo deviceinfo.SerialInfo,
	macAddress string,
	report string,
) {
	rid := compReportID(id, serialInfo)
	if cr := rr.components[rid]; cr == nil {
		// Create a new component report
		rr.components[rid] = &componentReport{
			id:         id,
			serialInfo: serialInfo,
			report:     report,
			bmcReports: make(map[string]string),
		}
	}

	// Update the BMC report
	rr.components[rid].bmcReports[macAddress] = report
}

// FinalizedReport represents the complete rack operation report in JSON format
type FinalizedReport struct {
	RackID         string                     `json:"rack_id"`
	RackSerialInfo string                     `json:"rack_serial_info"`
	RackReport     string                     `json:"rack_report,omitempty"`
	Components     []FinalizedComponentReport `json:"components"`
}

// FinalizedComponentReport represents a component report in JSON format
type FinalizedComponentReport struct {
	ComponentID         string            `json:"component_id"`
	ComponentSerialInfo string            `json:"component_serial_info"`
	ComponentReport     string            `json:"component_report,omitempty"`
	BMCReports          map[string]string `json:"bmc_reports,omitempty"`
}

func (rr *RackOpReport) Finalize() string {
	// Create the finalized report structure
	finalizedReport := FinalizedReport{
		RackID:         rr.id.String(),
		RackSerialInfo: rr.serialInfo.String(),
		RackReport:     rr.report,
		Components:     make([]FinalizedComponentReport, 0, len(rr.components)),
	}

	// Add components to the finalized report
	for _, comp := range rr.components {
		componentReport := FinalizedComponentReport{
			ComponentID:         comp.id.String(),
			ComponentSerialInfo: comp.serialInfo.String(),
			ComponentReport:     comp.report,
		}

		// Only include BMC reports if they exist
		if len(comp.bmcReports) > 0 {
			componentReport.BMCReports = make(map[string]string)
			for macAddr, bmcReport := range comp.bmcReports {
				componentReport.BMCReports[macAddr] = bmcReport
			}
		}

		finalizedReport.Components = append(finalizedReport.Components, componentReport)
	}

	jsonBytes, err := json.MarshalIndent(finalizedReport, "", "  ")
	if err != nil {
		return fallbackErrorJSON(err)
	}

	return string(jsonBytes)
}

// fallbackErrorJSON returns a JSON-safe error report string used when
// marshaling the FinalizedReport unexpectedly fails. The error message is
// embedded via json.Marshal so any double quotes, backslashes, or control
// characters in err are properly escaped and the returned value is always
// valid JSON.
func fallbackErrorJSON(err error) string {
	payload := struct {
		Error string `json:"error"`
	}{
		Error: "Failed to marshal report to JSON: " + err.Error(),
	}
	b, mErr := json.Marshal(payload)
	if mErr != nil {
		return `{"error":"Failed to marshal report to JSON"}`
	}
	return string(b)
}

func compReportID(id uuid.UUID, serialInfo deviceinfo.SerialInfo) string {
	return id.String() + "-" + serialInfo.String()
}
