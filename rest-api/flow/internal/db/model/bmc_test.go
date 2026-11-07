// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

type testBMC struct {
	b *BMC
}

func newTestBMC(b BMC) *testBMC {
	return &testBMC{b: &b}
}

func (b *testBMC) BMC() *BMC {
	return b.b
}

func (b *testBMC) modifyIP(ip *string) *testBMC {
	b.b.IPAddress = ip
	return b
}

func (b *testBMC) modifyUser(user *string) *testBMC {
	b.b.User = user
	return b
}

func (b *testBMC) modifyPassword(password *string) *testBMC {
	b.b.Password = password
	return b
}

func TestBMCBuildPatch(t *testing.T) {
	componentID := uuid.New()

	// Helper function to create pointers to strings and BMCs
	strPtr := func(s string) *string { return &s }
	/*
		bmcPtr := func(b BMC) *BMC { return &b }
		modifyIP := func(b *BMC, ip *string) *BMC { b.IPAddress = ip; return b }
		modifyUser := func(b *BMC, user *string) *BMC { b.User = user; return b }
		modifyPassword := func(b *BMC, password *string) *BMC { b.Password = password; return b }
	*/
	shareBMC := BMC{
		MacAddress:  "00:11:22:33:44:55",
		Type:        devicetypes.BMCTypeHost.String(),
		ComponentID: componentID,
		IPAddress:   strPtr("192.168.1.100"),
		User:        strPtr("admin"),
		Password:    strPtr("password"),
	}

	testCases := map[string]struct {
		cur      *BMC
		input    *BMC
		expected *BMC
	}{
		"nil input BMC returns nil": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    nil,
			expected: nil,
		},
		"nil current BMC returns nil": {
			cur:      nil,
			input:    newTestBMC(shareBMC).BMC(),
			expected: nil,
		},
		"both BMCs nil returns nil": {
			cur:      nil,
			input:    nil,
			expected: nil,
		},
		"no changes returns nil": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).BMC(),
			expected: nil,
		},
		"IP address change": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyIP(strPtr("192.168.1.101")).BMC(),
			expected: newTestBMC(shareBMC).modifyIP(strPtr("192.168.1.101")).BMC(),
		},
		"user change": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyUser(strPtr("root")).BMC(),
			expected: newTestBMC(shareBMC).modifyUser(strPtr("root")).BMC(),
		},
		"password change": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
			expected: newTestBMC(shareBMC).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
		},
		"multiple changes": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyIP(strPtr("192.168.1.101")).modifyUser(strPtr("root")).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
			expected: newTestBMC(shareBMC).modifyIP(strPtr("192.168.1.101")).modifyUser(strPtr("root")).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
		},
		"change from nil to non-nil IP": {
			cur:      newTestBMC(shareBMC).modifyIP(nil).BMC(),
			input:    newTestBMC(shareBMC).modifyIP(strPtr("192.168.1.101")).BMC(), //nolint:gosec
			expected: newTestBMC(shareBMC).modifyIP(strPtr("192.168.1.101")).BMC(), //nolint:gosec
		},
		"change from nil to non-nil user": {
			cur:      newTestBMC(shareBMC).modifyUser(nil).BMC(),
			input:    newTestBMC(shareBMC).modifyUser(strPtr("root")).BMC(),
			expected: newTestBMC(shareBMC).modifyUser(strPtr("root")).BMC(),
		},
		"change from nil to non-nil password": {
			cur:      newTestBMC(shareBMC).modifyPassword(nil).BMC(),
			input:    newTestBMC(shareBMC).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
			expected: newTestBMC(shareBMC).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
		},
		"no change when IP input has nil value and current has value": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyIP(nil).BMC(),
			expected: nil,
		},
		"no change when user input has nil value and current has value": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyUser(nil).BMC(),
			expected: nil,
		},
		"no change when password input has nil value and current has value": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyPassword(nil).BMC(),
			expected: nil,
		},
		"mixed changes with some nil values": {
			cur:      newTestBMC(shareBMC).BMC(),
			input:    newTestBMC(shareBMC).modifyIP(nil).modifyUser(strPtr("root")).modifyPassword(strPtr("newpassword")).BMC(), //nolint:gosec
			expected: newTestBMC(shareBMC).modifyUser(strPtr("root")).modifyPassword(strPtr("newpassword")).BMC(),               //nolint:gosec
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			result := tc.input.BuildPatch(tc.cur)
			assert.Equal(t, tc.expected, result)
		})
	}
}
