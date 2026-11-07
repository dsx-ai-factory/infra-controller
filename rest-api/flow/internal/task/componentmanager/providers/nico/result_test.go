// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"testing"

	"github.com/stretchr/testify/assert"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestResultIdentifier(t *testing.T) {
	componentID := "machine-1"
	macAddress := "aa:bb:cc:dd:ee:ff"
	tests := []struct {
		name          string
		result        *corev1.ComponentResult
		useMACAddress bool
		want          string
	}{
		{name: "component ID request", result: &corev1.ComponentResult{ComponentId: &componentID, MacAddress: &macAddress}, want: componentID},
		{name: "MAC request", result: &corev1.ComponentResult{ComponentId: &componentID, MacAddress: &macAddress}, useMACAddress: true, want: macAddress},
		{name: "MAC fallback", result: &corev1.ComponentResult{MacAddress: &macAddress}, want: macAddress},
		{name: "nil result", want: ""},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.Equal(t, test.want, ResultIdentifier(test.result, test.useMACAddress))
		})
	}
}
