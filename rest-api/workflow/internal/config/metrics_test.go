// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestMetricsConfig(t *testing.T) {
	type args struct {
		enabled   bool
		port      int
		namespace string
	}

	tests := []struct {
		name string
		args args
		want *MetricsConfig
	}{
		{
			name: "initialize Metrics config",
			args: args{
				enabled:   true,
				port:      6930,
				namespace: DefaultMetricsNamespace,
			},
			want: &MetricsConfig{
				Enabled:   true,
				Port:      6930,
				Namespace: DefaultMetricsNamespace,
			},
		},
		{
			name: "initialize Metrics config with an overridden namespace",
			args: args{
				enabled:   true,
				port:      6930,
				namespace: "acme_workflow",
			},
			want: &MetricsConfig{
				Enabled:   true,
				Port:      6930,
				Namespace: "acme_workflow",
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewMetricsConfig(tt.args.enabled, tt.args.port, tt.args.namespace)

			assert.Equal(t, tt.want, got)
			assert.Equal(t, tt.want.GetListenAddr(), got.GetListenAddr())
		})
	}
}

func TestConfig_GetMetricsNamespace(t *testing.T) {
	tests := []struct {
		name      string
		configure func(c *Config)
		want      string
	}{
		{
			name:      "unset falls back to the default",
			configure: func(c *Config) {},
			want:      DefaultMetricsNamespace,
		},
		{
			// An operator who blanks the key gets the default rather than
			// unprefixed metric names that would collide with another service.
			name:      "explicitly empty falls back to the default",
			configure: func(c *Config) { c.v.Set(ConfigMetricsNamespace, "") },
			want:      DefaultMetricsNamespace,
		},
		{
			name:      "override is honored",
			configure: func(c *Config) { c.v.Set(ConfigMetricsNamespace, "acme_workflow") },
			want:      "acme_workflow",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			c := NewConfig()
			tt.configure(c)
			defer c.v.Set(ConfigMetricsNamespace, DefaultMetricsNamespace)

			assert.Equal(t, tt.want, c.GetMetricsNamespace())
		})
	}
}
