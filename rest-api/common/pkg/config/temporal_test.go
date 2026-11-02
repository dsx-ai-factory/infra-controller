// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestTemporalConfig_GetHostPort(t *testing.T) {
	tests := []struct {
		name   string
		config TemporalConfig
		want   string
	}{
		{
			name:   "IPv6 with configured port",
			config: TemporalConfig{Host: "2001:db8::1", Port: 17233},
			want:   "[2001:db8::1]:17233",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, tt.config.GetHostPort())
		})
	}
}
