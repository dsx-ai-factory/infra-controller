// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/metadata"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIMetadata(t *testing.T) {
	tests := []struct {
		name string
		want *APIMetadata
	}{
		{
			name: "test initializing API model for HealthCheck",
			want: &APIMetadata{
				Version:   metadata.Version,
				BuildTime: metadata.BuildTime,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIMetadata()

			assert.Equal(t, tt.want.Version, got.Version)
			assert.Equal(t, tt.want.BuildTime, got.BuildTime)
		})
	}
}
