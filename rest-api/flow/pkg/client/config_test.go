// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestConfig_Validate(t *testing.T) {
	testCases := map[string]struct {
		config  Config
		wantErr bool
	}{
		"valid config": {
			config:  Config{Host: "localhost", Port: 8080},
			wantErr: false,
		},
		"missing host": {
			config:  Config{Port: 8080},
			wantErr: true,
		},
		"invalid port (zero)": {
			config:  Config{Host: "localhost", Port: 0},
			wantErr: true,
		},
		"invalid port (negative)": {
			config:  Config{Host: "localhost", Port: -1},
			wantErr: true,
		},
		"invalid port (out of range)": {
			config:  Config{Host: "localhost", Port: 70000},
			wantErr: true,
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			err := testCase.config.Validate()
			if testCase.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestConfig_Target(t *testing.T) {
	testCases := map[string]struct {
		host string
		want string
	}{
		"hostname": {
			host: "localhost",
			want: "localhost:8080",
		},
		"IPv6": {
			host: "2001:db8::1",
			want: "[2001:db8::1]:8080",
		},
		"bracketed IPv6": {
			host: "[2001:db8::1]",
			want: "[2001:db8::1]:8080",
		},
	}

	for name, testCase := range testCases {
		t.Run(name, func(t *testing.T) {
			config := Config{Host: testCase.host, Port: 8080}
			assert.Equal(t, testCase.want, config.Target())
		})
	}
}
