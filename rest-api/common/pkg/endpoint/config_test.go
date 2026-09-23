// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package endpoint

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestConfig_Target(t *testing.T) {
	testCases := map[string]struct {
		host string
		want string
	}{
		"hostname": {
			host: "temporal.example.com",
			want: "temporal.example.com:17233",
		},
		"IPv4": {
			host: "192.0.2.1",
			want: "192.0.2.1:17233",
		},
		"IPv6": {
			host: "2001:db8::1",
			want: "[2001:db8::1]:17233",
		},
		"bracketed IPv6": {
			host: "[2001:db8::1]",
			want: "[2001:db8::1]:17233",
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			config := Config{Host: testCase.host, Port: 17233}
			target := config.Target()
			assert.Equal(t, testCase.want, target)
		})
	}
}
