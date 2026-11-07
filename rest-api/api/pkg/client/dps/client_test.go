// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dps

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestTokenCredentials_GetRequestMetadata(t *testing.T) {
	tests := []struct {
		name        string
		contents    string
		wantHeader  string
		wantError   string
		missingFile bool
	}{
		{
			name:       "returns bearer token and trims file whitespace",
			contents:   "  rotated-token\n",
			wantHeader: "Bearer rotated-token",
		},
		{
			name:      "rejects empty token",
			contents:  " \n\t",
			wantError: "DPS token file is empty",
		},
		{
			name:        "reports missing token file",
			wantError:   "read DPS token",
			missingFile: true,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			tokenPath := filepath.Join(t.TempDir(), "token")
			if !test.missingFile {
				require.NoError(t, os.WriteFile(tokenPath, []byte(test.contents), 0o600))
			}

			metadata, err := NewTokenCredentials(tokenPath).GetRequestMetadata(context.Background())
			if test.wantError != "" {
				require.ErrorContains(t, err, test.wantError)
				assert.Nil(t, metadata)
				return
			}

			require.NoError(t, err)
			assert.Equal(t, test.wantHeader, metadata[authorizationHeader])
		})
	}
}

func TestTokenCredentials_RequireTransportSecurity(t *testing.T) {
	tests := []struct {
		name string
		path string
	}{
		{name: "requires TLS", path: "unused"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			assert.True(t, NewTokenCredentials(test.path).RequireTransportSecurity())
		})
	}
}

func TestNewConnection(t *testing.T) {
	tests := []struct {
		name      string
		ca        string
		wantError string
	}{
		{name: "rejects invalid CA", ca: "not a certificate", wantError: "contains no valid certificates"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			caPath := filepath.Join(t.TempDir(), "ca.crt")
			require.NoError(t, os.WriteFile(caPath, []byte(test.ca), 0o600))

			connection, err := NewConnection(Config{
				Endpoint:  "dps.example.com:443",
				TokenPath: filepath.Join(t.TempDir(), "token"),
				CAPath:    caPath,
			})

			require.ErrorContains(t, err, test.wantError)
			assert.Nil(t, connection)
		})
	}
}
