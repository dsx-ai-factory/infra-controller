// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package log

import (
	"bytes"
	"context"
	"encoding/json"
	"testing"

	"github.com/rs/zerolog"
	zlog "github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/activity"
	temporallog "go.temporal.io/sdk/log"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/workflow"
)

// captureGlobalLogger points the global zerolog logger at a buffer for the
// duration of the test, matching how the services configure it: a timestamp on
// every line.
func captureGlobalLogger(t *testing.T) *bytes.Buffer {
	t.Helper()

	buf := &bytes.Buffer{}
	previous := zlog.Logger
	zlog.Logger = zerolog.New(buf).With().Timestamp().Logger()
	t.Cleanup(func() { zlog.Logger = previous })

	return buf
}

// findLine returns the fields of the first captured line with the given message.
// The buffer also holds the Temporal SDK's own lines, so tests match on their
// own message rather than assuming a line position.
func findLine(t *testing.T, buf *bytes.Buffer, msg string) map[string]any {
	t.Helper()

	decoder := json.NewDecoder(bytes.NewReader(buf.Bytes()))
	for decoder.More() {
		fields := map[string]any{}
		require.NoError(t, decoder.Decode(&fields))

		if fields[zerolog.MessageFieldName] == msg {
			return fields
		}
	}

	t.Fatalf("no log line with message %q in:\n%s", msg, buf.String())
	return nil
}

func TestTemporalClientLogger(t *testing.T) {
	tests := []struct {
		name    string
		emit    func(logger temporallog.Logger)
		message string
		want    map[string]any
	}{
		{
			name:    "AddsTimestampFromGlobalLogger",
			emit:    func(l temporallog.Logger) { l.Info("plain line") },
			message: "plain line",
		},
		{
			name:    "MapsKeyvalsToFields",
			emit:    func(l temporallog.Logger) { l.Info("tagged line", "SiteID", "site-1") },
			message: "tagged line",
			want:    map[string]any{"SiteID": "site-1"},
		},
		{
			name:    "ToleratesOddKeyvals",
			emit:    func(l temporallog.Logger) { l.Info("odd line", "SiteID") },
			message: "odd line",
			want:    map[string]any{"SiteID": nil},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			buf := captureGlobalLogger(t)

			tt.emit(TemporalClientLogger())

			fields := findLine(t, buf, tt.message)
			assert.Contains(t, fields, zerolog.TimestampFieldName,
				"Temporal logs must carry the same timestamp as the rest of the service")
			for key, value := range tt.want {
				assert.Equal(t, value, fields[key])
			}
		})
	}
}

func TestTemporalWorkflowLogger(t *testing.T) {
	tests := []struct {
		name       string
		wf         func(ctx workflow.Context) error
		message    string
		wantFields map[string]any
	}{
		{
			name: "TagsWorkflowIdentity",
			wf: func(ctx workflow.Context) error {
				logger := TemporalWorkflowLogger(ctx)
				logger.Info().Msg("identity line")
				return nil
			},
			message: "identity line",
		},
		{
			name: "PreservesCallerFields",
			wf: func(ctx workflow.Context) error {
				logger := TemporalWorkflowLogger(ctx).With().Str("SiteID", "site-1").Logger()
				logger.Info().Msg("caller line")
				return nil
			},
			message:    "caller line",
			wantFields: map[string]any{"SiteID": "site-1"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			buf := captureGlobalLogger(t)

			suite := &testsuite.WorkflowTestSuite{}
			suite.SetLogger(TemporalClientLogger())
			env := suite.NewTestWorkflowEnvironment()
			env.RegisterWorkflowWithOptions(tt.wf, workflow.RegisterOptions{Name: "TestWorkflow"})
			env.ExecuteWorkflow("TestWorkflow")

			require.True(t, env.IsWorkflowCompleted())
			require.NoError(t, env.GetWorkflowError())

			fields := findLine(t, buf, tt.message)
			assert.Equal(t, "TestWorkflow", fields[fieldWorkflow])
			assert.NotEmpty(t, fields[fieldWorkflowID])
			assert.NotEmpty(t, fields[fieldRunID])
			for key, value := range tt.wantFields {
				assert.Equal(t, value, fields[key])
			}
		})
	}
}

func TestTemporalActivityLogger(t *testing.T) {
	tests := []struct {
		name        string
		inActivity  bool
		message     string
		wantTagged  bool
		wantFields  map[string]any
		wantMissing []string
	}{
		{
			name:       "TagsActivityIdentity",
			inActivity: true,
			message:    "activity line",
			wantTagged: true,
			wantFields: map[string]any{fieldActivity: "TestActivity", fieldAttempt: float64(1)},
		},
		{
			// Activity methods are unit tested by calling them directly, where
			// activity.GetInfo would panic.
			name:        "UntaggedOutsideActivityContext",
			inActivity:  false,
			message:     "direct call line",
			wantMissing: []string{fieldActivity, fieldActivityID, fieldAttempt},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			buf := captureGlobalLogger(t)
			message := tt.message

			act := func(ctx context.Context) error {
				logger := TemporalActivityLogger(ctx)
				logger.Info().Msg(message)
				return nil
			}

			if tt.inActivity {
				suite := &testsuite.WorkflowTestSuite{}
				suite.SetLogger(TemporalClientLogger())
				env := suite.NewTestActivityEnvironment()
				env.RegisterActivityWithOptions(act, activity.RegisterOptions{Name: "TestActivity"})

				_, err := env.ExecuteActivity("TestActivity")
				require.NoError(t, err)
			} else {
				require.NoError(t, act(context.Background()))
			}

			fields := findLine(t, buf, tt.message)
			for key, value := range tt.wantFields {
				assert.Equal(t, value, fields[key])
			}
			for _, key := range tt.wantMissing {
				assert.NotContains(t, fields, key)
			}
			if tt.wantTagged {
				assert.NotEmpty(t, fields[fieldWorkflowID])
				assert.NotEmpty(t, fields[fieldRunID])
			}
		})
	}
}
