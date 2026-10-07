// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/stretchr/testify/require"
)

func TestAlertExecutor_Execute(t *testing.T) {
	request := newValidExecutionRequest(t)
	request.Plan = &eventrule.SendAlertPlan{
		Severity: eventrule.SeverityCritical,
		Message:  "leak detected",
	}

	tests := map[string]struct {
		sender             *recordingAlertSender
		wantErr            error
		wantClassification error
	}{
		"completed": {sender: &recordingAlertSender{alertID: "alert-123"}},
		"operational failure is retryable": {
			sender:  &recordingAlertSender{err: errors.New("alert service unavailable")},
			wantErr: errors.New("alert service unavailable"), wantClassification: ErrRetryable,
		},
		"interrupted call returns context error": {
			sender: &recordingAlertSender{err: context.Canceled}, wantErr: context.Canceled,
		},
		"empty alert id is terminal": {
			sender: &recordingAlertSender{}, wantErr: errors.New("empty alert id"),
			wantClassification: ErrTerminal,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			actionExecutor := &AlertExecutor{sender: test.sender}

			err := actionExecutor.Execute(context.Background(), request)

			if test.wantErr != nil {
				if errors.Is(test.wantErr, context.Canceled) {
					require.ErrorIs(t, err, test.wantErr)
				} else {
					require.ErrorContains(t, err, test.wantErr.Error())
				}

				if test.wantClassification != nil {
					require.ErrorIs(t, err, test.wantClassification)
				}

				return
			}

			require.NoError(t, err)
			require.Equal(t, []AlertRequest{{
				IdempotencyKey: alertIdempotencyKey(request.ExecutionID),
				Severity:       eventrule.SeverityCritical,
				Message:        "leak detected",
			}}, test.sender.requests)
		})
	}
}

func TestAlertExecutor_ReusesIdempotencyKey(t *testing.T) {
	request := newValidExecutionRequest(t)
	request.Plan = &eventrule.SendAlertPlan{Severity: eventrule.SeverityWarning}
	sender := &recordingAlertSender{alertID: "stable-alert"}
	actionExecutor := &AlertExecutor{sender: sender}

	for range 2 {
		require.NoError(t, actionExecutor.Execute(context.Background(), request))
	}

	require.Len(t, sender.requests, 2)
	require.Equal(t, sender.requests[0].IdempotencyKey, sender.requests[1].IdempotencyKey)
}

func TestAlertPlan(t *testing.T) {
	valid := &eventrule.SendAlertPlan{Severity: eventrule.SeverityWarning}
	tests := map[string]struct {
		plan    eventrule.ExecutionPlan
		want    *eventrule.SendAlertPlan
		wantErr string
	}{
		"valid": {
			plan: valid,
			want: valid,
		},
		"wrong plan type": {
			plan:    &eventrule.NoopPlan{},
			wantErr: "alert executor received plan *eventrule.NoopPlan",
		},
		"nil send-alert plan": {
			plan:    (*eventrule.SendAlertPlan)(nil),
			wantErr: "send-alert execution plan is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			plan, err := alertPlan(test.plan)

			if test.wantErr != "" {
				require.EqualError(t, err, test.wantErr)
				require.Nil(t, plan)

				return
			}

			require.NoError(t, err)
			require.Same(t, test.want, plan)
		})
	}
}

type recordingAlertSender struct {
	requests []AlertRequest
	alertID  string
	err      error
}

func (s *recordingAlertSender) SendAlert(
	_ context.Context,
	request AlertRequest,
) (string, error) {
	s.requests = append(s.requests, request)

	return s.alertID, s.err
}
