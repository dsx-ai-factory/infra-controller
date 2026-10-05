// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"strings"
	"testing"

	openapi "github.com/dsx-ai-factory/infra-controller/rest-api/openapi"
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
