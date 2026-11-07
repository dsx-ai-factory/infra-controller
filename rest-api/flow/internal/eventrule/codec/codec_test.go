// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package codec

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestDecodeVersion(t *testing.T) {
	tests := map[string]struct {
		data        string
		wantVersion int
		wantErr     bool
	}{
		"version": {
			data:        `{"version":2,"value":"test"}`,
			wantVersion: 2,
		},
		"malformed document": {
			data:    `{"version":`,
			wantErr: true,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			version, err := decodeVersion([]byte(test.data), "test document")
			if test.wantErr {
				require.Error(t, err)
				return
			}

			require.NoError(t, err)
			require.Equal(t, test.wantVersion, version)
		})
	}
}

func TestDecodeStrict(t *testing.T) {
	tests := map[string]struct {
		data    string
		wantErr bool
	}{
		"valid": {
			data: `{"value":"test"}`,
		},
		"unknown field": {
			data:    `{"value":"test","unknown":true}`,
			wantErr: true,
		},
		"trailing value": {
			data:    `{"value":"test"} {"value":"other"}`,
			wantErr: true,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			var decoded struct {
				Value string `json:"value"`
			}
			err := decodeStrict([]byte(test.data), &decoded)
			if test.wantErr {
				require.Error(t, err)
				return
			}

			require.NoError(t, err)
			require.Equal(t, "test", decoded.Value)
		})
	}
}
