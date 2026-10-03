// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package utils

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestStatusPort(t *testing.T) {
	tests := []struct {
		name    string
		esaPort string
		want    string
	}{
		{name: "defaults when ESA_PORT is empty", esaPort: "", want: "8080"},
		{name: "uses ESA_PORT when set", esaPort: "9080", want: "9080"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Setenv("ESA_PORT", tt.esaPort)
			assert.Equal(t, tt.want, StatusPort())
		})
	}
}
