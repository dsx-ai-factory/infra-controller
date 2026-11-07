// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"testing"
)

func TestHealthzConfig(t *testing.T) {
	type args struct {
		enabled bool
		port    int
	}

	hccfg := HealthzConfig{
		Enabled: true,
		Port:    6930,
	}

	tests := []struct {
		name string
		args args
		want *HealthzConfig
	}{
		{
			name: "initialize Healthz config",
			args: args{
				enabled: true,
				port:    hccfg.Port,
			},
			want: &hccfg,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewHealthzConfig(tt.args.enabled, tt.args.port)

			if p := got.Port; p != tt.want.Port {
				t.Errorf("got.Port = %v, want %v", p, tt.want.Port)
			}

			if got := got.GetListenAddr(); got != tt.want.GetListenAddr() {
				t.Errorf("GetListenAddr() = %v, want %v", got, tt.want.GetListenAddr())
			}
		})
	}
}
