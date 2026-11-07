// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestPolicyCatalog_Contains(t *testing.T) {
	catalog := NewPolicyCatalog([]string{" Max-Q ", "Max-P", "", "Max-Q"})

	tests := []struct {
		name         string
		powerProfile string
		want         bool
	}{
		{name: "matches exact policy", powerProfile: "Max-Q", want: true},
		{name: "requires normalized request", powerProfile: " Max-P ", want: false},
		{name: "is case sensitive", powerProfile: "max-q", want: false},
		{name: "rejects empty", powerProfile: "", want: false},
		{name: "rejects unknown", powerProfile: "Balanced", want: false},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, catalog.Contains(test.powerProfile))
		})
	}

	assert.Len(t, catalog, 2)
}

type policyProviderFunc func(context.Context) ([]string, error)

func (f policyProviderFunc) ListPowerProfiles(ctx context.Context) ([]string, error) {
	return f(ctx)
}

func TestValidatePowerProfile(t *testing.T) {
	upstreamError := errors.New("DPS unavailable")
	tests := []struct {
		name        string
		provider    PolicyProvider
		profile     string
		want        string
		wantErrorIs error
	}{
		{
			name:    "normalizes and accepts known profile",
			profile: " Max-Q ",
			provider: policyProviderFunc(func(context.Context) ([]string, error) {
				return []string{"Max-P", "Max-Q"}, nil
			}),
			want: "Max-Q",
		},
		{
			name:    "rejects unknown profile",
			profile: "Balanced",
			provider: policyProviderFunc(func(context.Context) ([]string, error) {
				return []string{"Max-P", "Max-Q"}, nil
			}),
			wantErrorIs: ErrPowerProfileNotFound,
		},
		{
			name:        "rejects empty profile without calling DPS",
			profile:     "  ",
			provider:    policyProviderFunc(func(context.Context) ([]string, error) { panic("unexpected call") }),
			wantErrorIs: ErrPowerProfileRequired,
		},
		{
			name:    "fails closed when DPS is unavailable",
			profile: "Max-Q",
			provider: policyProviderFunc(func(context.Context) ([]string, error) {
				return nil, upstreamError
			}),
			wantErrorIs: upstreamError,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got, err := ValidatePowerProfile(context.Background(), test.provider, test.profile)
			if test.wantErrorIs != nil {
				require.ErrorIs(t, err, test.wantErrorIs)
				return
			}

			require.NoError(t, err)
			assert.Equal(t, test.want, got)
		})
	}
}
