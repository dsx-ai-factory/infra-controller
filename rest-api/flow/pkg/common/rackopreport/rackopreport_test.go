// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package rackopreport

import (
	"encoding/json"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/deviceinfo"
)

func TestRackOpReport_Finalize(t *testing.T) {
	testCases := map[string]struct {
		name         string
		setupReport  func() *RackOpReport
		validateJSON func(t *testing.T, jsonStr string)
	}{
		"empty report": {
			setupReport: func() *RackOpReport {
				rackID := uuid.New()
				serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK001"}
				return New(rackID, serialInfo)
			},
			validateJSON: func(t *testing.T, jsonStr string) {
				var report FinalizedReport
				err := json.Unmarshal([]byte(jsonStr), &report)
				require.NoError(t, err, "Should be valid JSON")

				assert.NotEmpty(t, report.RackID, "Rack ID should not be empty")
				assert.Equal(t, "NVIDIA-RACK001", report.RackSerialInfo)
				assert.Empty(t, report.RackReport, "Rack report should be empty")
				assert.Empty(t, report.Components, "Components should be empty")
			},
		},
		"rack with report only": {
			setupReport: func() *RackOpReport {
				rackID := uuid.New()
				serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK002"}
				report := New(rackID, serialInfo)
				report.UpdateReport("Rack power-on successful.\nAll systems nominal.")
				return report
			},
			validateJSON: func(t *testing.T, jsonStr string) {
				var report FinalizedReport
				err := json.Unmarshal([]byte(jsonStr), &report)
				require.NoError(t, err, "Should be valid JSON")

				assert.Equal(t, "NVIDIA-RACK002", report.RackSerialInfo)
				assert.Equal(t, "Rack power-on successful.\nAll systems nominal.", report.RackReport)
				assert.Empty(t, report.Components, "Components should be empty")
			},
		},
		"comprehensive report": {
			setupReport: func() *RackOpReport {
				rackID := uuid.New()
				serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK003"}
				report := New(rackID, serialInfo)

				// Add rack-level report
				report.UpdateReport("Rack initialization completed successfully.")

				// Add component reports
				comp1ID := uuid.New()
				comp1Serial := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "COMP001"}
				report.UpdateCompReport(comp1ID, comp1Serial, "Component 1 operational.\nTemperature: 45°C")

				comp2ID := uuid.New()
				comp2Serial := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "COMP002"}
				report.UpdateCompReport(comp2ID, comp2Serial, "Component 2 startup complete.")

				// Add BMC reports
				report.UpdateBMCReport(comp1ID, comp1Serial, "00:11:22:33:44:55", "BMC firmware loaded.\nNetwork configured.")
				report.UpdateBMCReport(comp1ID, comp1Serial, "00:11:22:33:44:66", "BMC secondary interface active.")
				report.UpdateBMCReport(comp2ID, comp2Serial, "00:11:22:33:44:77", "BMC initialization failed.")

				return report
			},
			validateJSON: func(t *testing.T, jsonStr string) {
				var report FinalizedReport
				err := json.Unmarshal([]byte(jsonStr), &report)
				require.NoError(t, err, "Should be valid JSON")

				assert.Equal(t, "NVIDIA-RACK003", report.RackSerialInfo)
				assert.Equal(t, "Rack initialization completed successfully.", report.RackReport)
				assert.Len(t, report.Components, 2, "Should have 2 components")

				// Check components
				comp1Found := false
				comp2Found := false

				for _, comp := range report.Components {
					switch comp.ComponentSerialInfo {
					case "NVIDIA-COMP001":
						comp1Found = true
						assert.Equal(t, "Component 1 operational.\nTemperature: 45°C", comp.ComponentReport)
						assert.Len(t, comp.BMCReports, 2, "Component 1 should have 2 BMC reports")
						assert.Contains(t, comp.BMCReports, "00:11:22:33:44:55")
						assert.Contains(t, comp.BMCReports, "00:11:22:33:44:66")
						assert.Equal(t, "BMC firmware loaded.\nNetwork configured.", comp.BMCReports["00:11:22:33:44:55"])
						assert.Equal(t, "BMC secondary interface active.", comp.BMCReports["00:11:22:33:44:66"])
					case "NVIDIA-COMP002":
						comp2Found = true
						assert.Equal(t, "Component 2 startup complete.", comp.ComponentReport)
						assert.Len(t, comp.BMCReports, 1, "Component 2 should have 1 BMC report")
						assert.Contains(t, comp.BMCReports, "00:11:22:33:44:77")
						assert.Equal(t, "BMC initialization failed.", comp.BMCReports["00:11:22:33:44:77"])
					}
				}

				assert.True(t, comp1Found, "Component 1 should be found")
				assert.True(t, comp2Found, "Component 2 should be found")
			},
		},
		"component with no BMC reports": {
			setupReport: func() *RackOpReport {
				rackID := uuid.New()
				serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK004"}
				report := New(rackID, serialInfo)

				compID := uuid.New()
				compSerial := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "COMP003"}
				report.UpdateCompReport(compID, compSerial, "Component without BMCs")

				return report
			},
			validateJSON: func(t *testing.T, jsonStr string) {
				var report FinalizedReport
				err := json.Unmarshal([]byte(jsonStr), &report)
				require.NoError(t, err, "Should be valid JSON")

				assert.Len(t, report.Components, 1)
				comp := report.Components[0]
				assert.Equal(t, "NVIDIA-COMP003", comp.ComponentSerialInfo)
				assert.Equal(t, "Component without BMCs", comp.ComponentReport)
				assert.Empty(t, comp.BMCReports, "BMC reports should be empty/nil")
			},
		},
		"BMC report with empty content": {
			setupReport: func() *RackOpReport {
				rackID := uuid.New()
				serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK005"}
				report := New(rackID, serialInfo)

				compID := uuid.New()
				compSerial := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "COMP004"}
				report.UpdateBMCReport(compID, compSerial, "00:11:22:33:44:88", "")

				return report
			},
			validateJSON: func(t *testing.T, jsonStr string) {
				var report FinalizedReport
				err := json.Unmarshal([]byte(jsonStr), &report)
				require.NoError(t, err, "Should be valid JSON")

				assert.Len(t, report.Components, 1)
				comp := report.Components[0]
				assert.Equal(t, "NVIDIA-COMP004", comp.ComponentSerialInfo)
				assert.Len(t, comp.BMCReports, 1)
				assert.Contains(t, comp.BMCReports, "00:11:22:33:44:88")
				assert.Equal(t, "", comp.BMCReports["00:11:22:33:44:88"], "BMC report should be empty string")
			},
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			report := tc.setupReport()
			result := report.Finalize()

			// Verify it's valid JSON
			assert.True(t, json.Valid([]byte(result)), "Output should be valid JSON")

			// Run custom validation
			tc.validateJSON(t, result)
		})
	}
}

func TestRackOpReport_Finalize_MultilinePreservation(t *testing.T) {
	// Test that the finalize method preserves multi-line reports in JSON
	rackID := uuid.New()
	serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK_INDENT"}
	report := New(rackID, serialInfo)

	multiLineRackReport := `Line 1 of rack report
Line 2 of rack report
  Indented line 3`

	multiLineCompReport := `Line 1 of component report
Line 2 of component report
  Indented line 3
    Double indented line 4`

	multiLineBMCReport := `BMC Line 1
BMC Line 2
  BMC Indented line`

	report.UpdateReport(multiLineRackReport)

	compID := uuid.New()
	compSerial := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "COMP_INDENT"}
	report.UpdateCompReport(compID, compSerial, multiLineCompReport)
	report.UpdateBMCReport(compID, compSerial, "00:11:22:33:44:99", multiLineBMCReport)

	result := report.Finalize()

	// Parse the JSON result
	var finalizedReport FinalizedReport
	err := json.Unmarshal([]byte(result), &finalizedReport)
	require.NoError(t, err, "Should be valid JSON")

	// Verify that multi-line content is preserved exactly as provided
	assert.Equal(t, multiLineRackReport, finalizedReport.RackReport)
	assert.Len(t, finalizedReport.Components, 1)

	comp := finalizedReport.Components[0]
	assert.Equal(t, multiLineCompReport, comp.ComponentReport)
	assert.Len(t, comp.BMCReports, 1)
	assert.Equal(t, multiLineBMCReport, comp.BMCReports["00:11:22:33:44:99"])
}

func TestRackOpReport_Finalize_ErrorHandling(t *testing.T) {
	// This test is more for documentation purposes since it's hard to force JSON marshaling to fail
	// with our simple structures, but it shows the error handling exists
	rackID := uuid.New()
	serialInfo := deviceinfo.SerialInfo{Manufacturer: "NVIDIA", SerialNumber: "RACK_ERROR"}
	report := New(rackID, serialInfo)

	// Normal case should work fine
	result := report.Finalize()
	assert.True(t, json.Valid([]byte(result)), "Should produce valid JSON")
	assert.NotContains(t, result, `"error"`, "Should not contain error field in normal case")
}

func TestFallbackErrorJSON_SafeQuoting(t *testing.T) {
	// The fallback must produce valid JSON regardless of the contents of err,
	// including cases where the error message contains characters that would
	// otherwise break out of a hand-constructed quoted string literal.
	testCases := map[string]error{
		"plain":           errors.New("something failed"),
		"double quote":    errors.New(`bad "value" here`),
		"backslash":       errors.New(`back\slash`),
		"json injection":  errors.New(`","injected":"yes`),
		"control chars":   errors.New("nul\x00tab\there"),
		"newline":         errors.New("multi\nline"),
		"unicode":         errors.New("café 你好 \U0001F600"),
		"trailing quote":  errors.New(`ends with quote"`),
		"empty":           errors.New(""),
		"only quotes":     errors.New(`"""`),
		"only backslash":  errors.New(`\`),
		"escaped already": errors.New(`\"`),
	}

	for name, inErr := range testCases {
		t.Run(name, func(t *testing.T) {
			got := fallbackErrorJSON(inErr)

			require.True(t, json.Valid([]byte(got)),
				"output must be valid JSON, got: %q", got)

			var parsed map[string]string
			require.NoError(t, json.Unmarshal([]byte(got), &parsed),
				"output must unmarshal as map[string]string, got: %q", got)

			assert.Equal(t,
				"Failed to marshal report to JSON: "+inErr.Error(),
				parsed["error"],
				"error message should round-trip through JSON unchanged")
		})
	}
}
