// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"testing"
	"time"

	"github.com/spf13/viper"
	"github.com/stretchr/testify/assert"
)

func TestNewConfig(t *testing.T) {
	tests := []struct {
		name string
		want *Config
	}{
		{
			name: "initialize config",
			want: &Config{},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			NewConfig()
		})
	}
}

// TestConfig_GetTracingServiceName proves the binary supplies a service name
// when the config omits or empties it, so an export never lacks service.name.
func TestConfig_GetTracingServiceName(t *testing.T) {
	tcs := []struct {
		descr      string
		configured *string
		want       string
	}{
		{descr: "binary default when omitted", want: DefaultTracingServiceName},
		{descr: "binary default when explicitly empty", configured: new(string), want: DefaultTracingServiceName},
		{descr: "configured name wins", configured: ptr("custom-workflow"), want: "custom-workflow"},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			v := viper.New()
			if tc.configured != nil {
				v.Set(ConfigTracingServiceName, *tc.configured)
			}
			cfg := &Config{v: v}
			assert.Equal(t, tc.want, cfg.GetTracingServiceName())
		})
	}
}

func ptr(s string) *string { return &s }

func TestConfig_Validate(t *testing.T) {
	tests := []struct {
		name                    string
		values                  map[string]string
		wantPanic               bool
		wantRetention           time.Duration
		wantSuccessfulRetention time.Duration
	}{
		{
			name:                    "unset Site namespace retention uses the defaults",
			wantRetention:           DefaultSiteNamespaceRetention,
			wantSuccessfulRetention: DefaultSiteNamespaceSuccessfulInventoryRetention,
		},
		{
			name: "zero disables both Site namespace retention settings",
			values: map[string]string{
				ConfigTemporalSiteNamespaceRetention:                    "0",
				ConfigTemporalSiteNamespaceSuccessfulInventoryRetention: "0",
			},
		},
		{
			name: "Site namespace retention overrides are honored",
			values: map[string]string{
				ConfigTemporalSiteNamespaceRetention:                    "168h",
				ConfigTemporalSiteNamespaceSuccessfulInventoryRetention: "12h",
			},
			wantRetention:           168 * time.Hour,
			wantSuccessfulRetention: 12 * time.Hour,
		},
		{
			// viper reads this as 0, which would silently disable the setting
			name:      "Site namespace retention in days is rejected",
			values:    map[string]string{ConfigTemporalSiteNamespaceRetention: "7d"},
			wantPanic: true,
		},
		{
			name:      "negative successful inventory retention is rejected",
			values:    map[string]string{ConfigTemporalSiteNamespaceSuccessfulInventoryRetention: "-1h"},
			wantPanic: true,
		},
		{
			name: "successful inventory retention longer than the namespace retention is rejected",
			values: map[string]string{
				ConfigTemporalSiteNamespaceRetention:                    "72h",
				ConfigTemporalSiteNamespaceSuccessfulInventoryRetention: "96h",
			},
			wantPanic: true,
		},
		{
			name: "successful inventory retention equal to the namespace retention is rejected",
			values: map[string]string{
				ConfigTemporalSiteNamespaceRetention:                    "72h",
				ConfigTemporalSiteNamespaceSuccessfulInventoryRetention: "72h",
			},
			wantPanic: true,
		},
		{
			// Zero leaves the namespace retention unchanged, so there's nothing to compare against
			name: "successful inventory retention is not compared with an unchanged namespace retention",
			values: map[string]string{
				ConfigTemporalSiteNamespaceRetention:                    "0",
				ConfigTemporalSiteNamespaceSuccessfulInventoryRetention: "96h",
			},
			wantSuccessfulRetention: 96 * time.Hour,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			config = nil // reset package-level singleton between cases

			// NewConfig stays outside the panic check, since it also panics on unrelated missing config
			c := NewConfig()
			for key, value := range tt.values {
				c.v.Set(key, value)
			}

			if tt.wantPanic {
				assert.Panics(t, c.Validate)
				return
			}

			assert.NotPanics(t, c.Validate)
			assert.Equal(t, tt.wantRetention, c.GetTemporalSiteNamespaceRetention())
			assert.Equal(t, tt.wantSuccessfulRetention, c.GetTemporalSiteNamespaceSuccessfulInventoryRetention())
		})
	}
	config = nil // leave clean for other tests in the package
}
