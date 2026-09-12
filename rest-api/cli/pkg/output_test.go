// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"bytes"
	"io"
	"os"
	"regexp"
	"strings"
	"testing"

	openapi "github.com/NVIDIA/infra-controller/rest-api/openapi"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestValidateOutputFormat(t *testing.T) {
	cases := []struct {
		name    string
		input   string
		wantErr bool
	}{
		{name: "json is allowed", input: "json"},
		{name: "yaml is allowed", input: "yaml"},
		{name: "table is allowed", input: "table"},
		{name: "empty string is allowed (default flag value path)", input: ""},
		{name: "uppercase is not silently normalized -- enum is case-sensitive", input: "JSON", wantErr: true},
		{name: "xml is rejected (the bug filer's example)", input: "xml", wantErr: true},
		{name: "garbage is rejected", input: "foobar", wantErr: true},
		{name: "leading whitespace is rejected (no implicit trim)", input: " json", wantErr: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := ValidateOutputFormat(tc.input)
			if tc.wantErr {
				require.Error(t, err)
				assert.Contains(t, err.Error(), "invalid value")
				assert.Contains(t, err.Error(), "json, yaml, table",
					"error message must list allowed values so the user can fix the typo without checking docs")
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestFormatOutput_RejectsUnknownFormatAsDefenseInDepth(t *testing.T) {
	// FormatOutput is called from generated commands and from the --all
	// pagination path; if a future code path forgets to attach the
	// validateOutputFlag Action, FormatOutput should still fail loudly
	// rather than silently picking JSON.
	err := FormatOutput([]byte(`{"id":"x"}`), "xml")
	require.Error(t, err)
	assert.Contains(t, err.Error(), "invalid value")
	assert.Contains(t, err.Error(), "xml")
}

func TestFormatOutput_AcceptsKnownFormatsIncludingEmpty(t *testing.T) {
	cases := []string{"", "json", "yaml", "table"}
	for _, format := range cases {
		t.Run(format, func(t *testing.T) {
			err := FormatOutput([]byte(`{"id":"x","name":"n","status":"ok"}`), format)
			assert.NoError(t, err, "FormatOutput must accept all values that pass validation")
		})
	}
}

// TestNewApp_OutputFlagRejectsUnknownValueAtRuntime exercises the full app
// stack with the embedded OpenAPI spec to confirm that the StringFlag.Action
// validator is wired up on every generated --output flag. Without the
// validator this invocation exited 0 and silently produced JSON.
func TestNewApp_OutputFlagRejectsUnknownValueAtRuntime(t *testing.T) {
	app, err := NewApp(openapi.Spec)
	require.NoError(t, err, "NewApp failed")

	// Pick a leaf command that has --output. site list is a stable list
	// command and is the smallest command surface that takes --output.
	err = app.Run([]string{"nicocli", "site", "list", "--output", "xml"})
	require.Error(t, err, "passing --output xml must NOT silently fall back to JSON")
	assert.True(t,
		strings.Contains(err.Error(), "invalid value") || strings.Contains(err.Error(), "xml"),
		"error must mention the invalid value to be actionable, got: %v", err,
	)
}

// TestFormatOutputWithOperation_VpcPeering_OperationSpec tests the custom
// table columns for vpc-peering list/get operations.
func TestFormatOutputWithOperation_VpcPeering_OperationSpec(t *testing.T) {
	cases := []struct {
		name        string
		operationID string
		input       string
		wantHeaders []string
		wantRows    [][]string
	}{
		{
			name:        "get-all-vpc-peering happy path",
			operationID: "get-all-vpc-peering",
			input:       `[{"id":"peering-1","vpc1":{"id":"vpc-1","name":"App-VPC"},"vpc2":{"id":"vpc-2","name":"Storage-VPC"},"status":"Ready"}]`,
			wantHeaders: []string{"ID", "VPC1 Name", "VPC1 ID", "VPC2 Name", "VPC2 ID"},
			wantRows:    [][]string{{"peering-1", "App-VPC", "vpc-1", "Storage-VPC", "vpc-2"}},
		},
		{
			name:        "get-vpc-peering single object",
			operationID: "get-vpc-peering",
			input:       `{"id":"peering-2","vpc1":{"id":"vpc-3","name":"Mgmt-VPC"},"vpc2":{"id":"vpc-4","name":"Data-VPC"},"status":"Configuring"}`,
			wantHeaders: []string{"ID", "VPC1 Name", "VPC1 ID", "VPC2 Name", "VPC2 ID"},
			wantRows:    [][]string{{"peering-2", "Mgmt-VPC", "vpc-3", "Data-VPC", "vpc-4"}},
		},
		{
			name:        "empty list",
			operationID: "get-all-vpc-peering",
			input:       `[]`,
			wantHeaders: []string{"ID", "VPC1 Name", "VPC1 ID", "VPC2 Name", "VPC2 ID"},
			wantRows:    [][]string{},
		},
		{
			name:        "missing nested fields renders empty cells",
			operationID: "get-all-vpc-peering",
			input:       `[{"id":"peering-3","vpc1":null,"vpc2":{"id":"vpc-5"},"status":"Error"}]`,
			wantHeaders: []string{"ID", "VPC1 Name", "VPC1 ID", "VPC2 Name", "VPC2 ID"},
			// tabwriter collapses empty columns; only non-empty columns appear
			wantRows: [][]string{{"peering-3", "vpc-5"}},
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// Capture stdout using a pipe
			r, w, _ := os.Pipe()
			stdout := os.Stdout
			os.Stdout = w
			err := FormatOutputWithOperation([]byte(tc.input), "table", tc.operationID)
			w.Close()
			os.Stdout = stdout

			var buf bytes.Buffer
			_, _ = io.Copy(&buf, r)
			require.NoError(t, err)

			output := strings.TrimSpace(buf.String())
			if len(tc.wantRows) == 0 {
				assert.Contains(t, output, "(no results)")
				return
			}

			lines := strings.Split(output, "\n")
			require.GreaterOrEqual(t, len(lines), 2, "expected header + at least one row")

			// Check headers - tabwriter uses tabs; split on tab and handle multi-word headers
			headerParts := splitTabLine(lines[0], len(tc.wantHeaders))
			assert.Equal(t, tc.wantHeaders, headerParts, "header mismatch: %q", lines[0])

			// Check data rows
			require.Equal(t, len(tc.wantRows), len(lines)-1, "row count mismatch")
			for i, wantRow := range tc.wantRows {
				rowParts := splitTabLine(lines[i+1], len(tc.wantHeaders))
				assert.Equal(t, wantRow, rowParts, "row %d mismatch: %q", i, lines[i+1])
			}
		})
	}
}

// splitTabLine splits a line on tabs, preserving empty cells.
// If tabs are not found or count mismatches, falls back to splitting on
// 2+ spaces (tabwriter column separator) while preserving single spaces
// within column headers.
var multiSpace = regexp.MustCompile(`\s{2,}`)

func splitTabLine(line string, expectedCols int) []string {
	parts := strings.Split(line, "\t")
	if len(parts) == expectedCols {
		return parts
	}
	// Tabwriter uses 2+ spaces for column separation. Split on that.
	parts = multiSpace.Split(line, -1)
	if len(parts) == expectedCols {
		return parts
	}
	// Return what we actually got - don't pad to expectedCols
	// because tabwriter collapses empty columns
	fields := strings.Fields(line)
	return fields
}

// TestFormatOutputWithOperation_UnknownOperationFallback tests that
// unregistered operations fall back to generic tableFields.
func TestFormatOutputWithOperation_UnknownOperationFallback(t *testing.T) {
	input := `[{"id":"x","name":"n","status":"ok","created":"2024-01-01","updated":"2024-01-02"}]`
	r, w, _ := os.Pipe()
	stdout := os.Stdout
	os.Stdout = w
	err := FormatOutputWithOperation([]byte(input), "table", "unknown-operation")
	w.Close()
	os.Stdout = stdout

	var buf bytes.Buffer
	_, _ = io.Copy(&buf, r)
	require.NoError(t, err)

	output := buf.String()
	assert.Contains(t, output, "id")
	assert.Contains(t, output, "name")
	assert.Contains(t, output, "status")
}

// TestFormatOutput_DefaultFormat_ForListActions tests that list actions
// default to "table" and non-list actions default to "json".
func TestFormatOutput_DefaultFormat_ForListActions(t *testing.T) {
	// We test via the flag default logic in commands.go by checking
	// the isListAction function directly.
	listActions := []string{"list", "list-foo", "list-bar"}
	nonListActions := []string{"get", "create", "update", "delete", "get-current", "stats", "status-history"}

	for _, a := range listActions {
		assert.True(t, isListAction(a), "expected %q to be list action", a)
	}
	for _, a := range nonListActions {
		assert.False(t, isListAction(a), "expected %q NOT to be list action", a)
	}
}

func TestNestedString(t *testing.T) {
	cases := []struct {
		name     string
		item     map[string]any
		path     []string
		expected string
	}{
		{"top-level string", map[string]any{"id": "abc"}, []string{"id"}, "abc"},
		{"nested string", map[string]any{"vpc1": map[string]any{"name": "App-VPC"}}, []string{"vpc1", "name"}, "App-VPC"},
		{"missing top-level", map[string]any{}, []string{"id"}, ""},
		{"missing nested", map[string]any{"vpc1": map[string]any{}}, []string{"vpc1", "name"}, ""},
		{"nil nested map", map[string]any{"vpc1": nil}, []string{"vpc1", "name"}, ""},
		{"non-string value", map[string]any{"id": 123}, []string{"id"}, ""},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.expected, nestedString(tc.item, tc.path...))
		})
	}
}
