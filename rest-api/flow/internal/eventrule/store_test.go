// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestExecutionClaimRequest_Validate(t *testing.T) {
	tests := map[string]struct {
		request ExecutionClaimRequest
		wantErr string
	}{
		"valid": {
			request: validExecutionClaimRequest(),
		},
		"missing owner": {
			request: func() ExecutionClaimRequest {
				request := validExecutionClaimRequest()
				request.Owner = ""
				return request
			}(),
			wantErr: "invalid execution input: execution claim owner is empty",
		},
		"owner too long": {
			request: ExecutionClaimRequest{
				Owner:         strings.Repeat("x", maxExecutionClaimOwnerRunes+1),
				Limit:         1,
				ClaimDuration: time.Minute,
				MaxAttempts:   4,
			},
			wantErr: "invalid execution input: execution claim owner exceeds 128 characters",
		},
		"invalid limit": {
			request: ExecutionClaimRequest{Owner: "scheduler-1"},
			wantErr: "invalid execution input: execution claim limit must be positive",
		},
		"invalid claim duration": {
			request: func() ExecutionClaimRequest {
				request := validExecutionClaimRequest()
				request.ClaimDuration = 0
				return request
			}(),
			wantErr: "invalid execution input: execution claim duration must be positive",
		},
		"invalid max attempts": {
			request: func() ExecutionClaimRequest {
				request := validExecutionClaimRequest()
				request.MaxAttempts = 0
				return request
			}(),
			wantErr: "invalid execution input: execution claim max attempts must be positive",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}

			require.EqualError(t, err, test.wantErr)
			require.ErrorIs(t, err, ErrInvalidExecutionInput)
		})
	}
}

func TestClaimedExecution_Validate(t *testing.T) {
	createdAt := time.Date(2026, 8, 24, 12, 0, 0, 0, time.UTC)
	execution, err := NewExecution(uuid.New(), "action", &NoopPlan{}, createdAt)
	require.NoError(t, err)

	token := uuid.New()
	disposition, err := execution.AcquireClaim(
		"scheduler-1",
		token,
		createdAt,
		createdAt.Add(time.Minute),
		4,
	)
	require.NoError(t, err)
	require.Equal(t, ClaimAcquired, disposition)

	valid := ClaimedExecution{Execution: *execution, Token: token}

	tests := map[string]struct {
		mutate  func(*ClaimedExecution)
		wantErr string
	}{
		"valid": {},
		"invalid execution": {
			mutate:  func(claim *ClaimedExecution) { claim.Execution.ID = uuid.Nil },
			wantErr: "execution: execution id is required",
		},
		"missing token": {
			mutate:  func(claim *ClaimedExecution) { claim.Token = uuid.Nil },
			wantErr: "execution claim token is required",
		},
		"mismatched token": {
			mutate:  func(claim *ClaimedExecution) { claim.Token = uuid.New() },
			wantErr: "execution claim token does not match running execution",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			claim := valid
			claim.Execution = valid.Execution.Clone()

			if test.mutate != nil {
				test.mutate(&claim)
			}

			err := claim.Validate()
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}

func validExecutionClaimRequest() ExecutionClaimRequest {
	return ExecutionClaimRequest{
		Owner:         "scheduler-1",
		Limit:         1,
		ClaimDuration: time.Minute,
		MaxAttempts:   4,
	}
}

func TestRuleFilterMatches(t *testing.T) {
	eventType := Type("test.event")
	otherEventType := Type("other.event")
	enabled := true
	disabled := false
	rule := &Rule{
		EventType: eventType,
		Enabled:   enabled,
	}

	tests := map[string]struct {
		filter RuleFilter
		rule   *Rule
		want   bool
	}{
		"empty filter": {
			rule: rule,
			want: true,
		},
		"all fields match": {
			filter: RuleFilter{EventType: &eventType, Enabled: &enabled},
			rule:   rule,
			want:   true,
		},
		"event type differs": {
			filter: RuleFilter{EventType: &otherEventType},
			rule:   rule,
			want:   false,
		},
		"enabled differs": {
			filter: RuleFilter{Enabled: &disabled},
			rule:   rule,
			want:   false,
		},
		"nil rule": {
			rule: nil,
			want: false,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, test.want, test.filter.Matches(test.rule))
		})
	}
}

func TestRuleListRequest_Validate(t *testing.T) {
	tests := map[string]struct {
		request RuleListRequest
		wantErr string
	}{
		"valid": {
			request: RuleListRequest{Limit: 100},
		},
		"negative offset": {
			request: RuleListRequest{Offset: -1, Limit: 100},
			wantErr: "offset must not be negative",
		},
		"non-positive limit": {
			request: RuleListRequest{},
			wantErr: "limit must be positive",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			err := test.request.Validate()
			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}

			require.ErrorContains(t, err, test.wantErr)
		})
	}
}
