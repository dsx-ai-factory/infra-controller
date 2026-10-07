// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"testing"

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
