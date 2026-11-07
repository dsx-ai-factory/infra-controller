// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package deviceinfo

import (
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestDeviceInfo_InfoMsg(t *testing.T) {
	di := DeviceInfo{
		ID:           uuid.New(),
		Name:         "Device1",
		Manufacturer: "NVIDIA",
		Model:        "ModelX",
		SerialNumber: "12345",
		Description:  "A test device",
	}

	typ := "TestDevice"

	testCases := map[string]struct {
		byID     bool
		expected string
	}{
		"info message based on ID": {
			byID:     true,
			expected: fmt.Sprintf("%s [id: %s]", typ, di.ID.String()),
		},
		"inf message based on serial information": {
			byID:     false,
			expected: fmt.Sprintf("%s [manufacturer: %s, serial: %s]", typ, di.Manufacturer, di.SerialNumber), //nolint
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, testCase.expected, di.InfoMsg(typ, testCase.byID))
		})
	}
}

func TestDeviceInfo_NewRandom(t *testing.T) {
	di := NewRandom("for-testing", 12)
	assert.Equal(t, "for-testing", di.Name)
	assert.Equal(t, 12, len(di.SerialNumber))
}
