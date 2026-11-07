// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"reflect"
	"testing"
)

func TestNewAPIHealthCheck(t *testing.T) {
	type args struct {
		isHealthy    bool
		errorMessage *string
	}
	tests := []struct {
		name string
		args args
		want *APIHealthCheck
	}{
		{
			name: "test initializing API model for HealthCheck",
			args: args{
				isHealthy:    true,
				errorMessage: nil,
			},
			want: &APIHealthCheck{
				IsHealthy: true,
				Error:     nil,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPIHealthCheck(tt.args.isHealthy, tt.args.errorMessage); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIHealthCheck() = %v, want %v", got, tt.want)
			}
		})
	}
}
