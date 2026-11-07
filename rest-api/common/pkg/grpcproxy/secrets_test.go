// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpcproxy

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRedactSecrets(t *testing.T) {
	cases := []struct {
		name    string
		payload string
		fields  []string
		// wantSecrets is the decoded secrets object, nil when nothing was
		// extracted and the payload must come back untouched.
		wantSecrets map[string]any
		wantErr     bool
	}{
		{
			name:        "extracts a named field",
			payload:     `{"credentialType":"SiteWideBmcRoot","password":"s3cr3t","macAddress":"aa:bb"}`,
			fields:      []string{"password"},
			wantSecrets: map[string]any{"password": "s3cr3t"},
		},
		{
			name:        "skips fields the payload does not carry",
			payload:     `{"name":"run-1","password":"s3cr3t","token":"t0ken"}`,
			fields:      []string{"password", "token", "absent"},
			wantSecrets: map[string]any{"password": "s3cr3t", "token": "t0ken"},
		},
		{
			name:    "no fields named",
			payload: `{"credentialType":"SiteWideBmcRoot"}`,
			fields:  nil,
		},
		{
			name:    "named field absent",
			payload: `{"credentialType":"SiteWideBmcRoot"}`,
			fields:  []string{"password"},
		},
		{
			name:    "payload is not an object",
			payload: `["not-an-object"]`,
			fields:  []string{"password"},
			wantErr: true,
		},
		{
			// JSON null decodes into a nil map without an error, so it needs
			// rejecting explicitly rather than passing through as "no secrets".
			name:    "payload is null",
			payload: `null`,
			fields:  []string{"password"},
			wantErr: true,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			redacted, secretsJSON, err := RedactSecrets([]byte(tc.payload), tc.fields)

			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)

			if tc.wantSecrets == nil {
				assert.Equal(t, []byte(tc.payload), redacted)
				assert.Nil(t, secretsJSON)
				return
			}

			var gotSecrets map[string]any
			require.NoError(t, json.Unmarshal(secretsJSON, &gotSecrets))
			assert.Equal(t, tc.wantSecrets, gotSecrets)

			// The redacted payload keeps the non-secret fields readable and
			// leaves no trace of the secret values.
			var gotRedacted map[string]any
			require.NoError(t, json.Unmarshal(redacted, &gotRedacted))
			for field, secret := range tc.wantSecrets {
				assert.NotContains(t, string(redacted), secret)
				assert.Equal(t, RedactedPlaceholder, gotRedacted[field])
			}
		})
	}
}

func TestMergeSecrets(t *testing.T) {
	cases := []struct {
		name        string
		redacted    string
		secretsJSON string
		want        map[string]any
		wantErr     bool
	}{
		{
			name:        "restores the secret values",
			redacted:    `{"credentialType":"SiteWideBmcRoot","password":"` + RedactedPlaceholder + `","macAddress":"aa:bb"}`,
			secretsJSON: `{"password":"s3cr3t"}`,
			want: map[string]any{
				"credentialType": "SiteWideBmcRoot",
				"password":       "s3cr3t",
				"macAddress":     "aa:bb",
			},
		},
		{
			name:     "no secrets leaves the payload untouched",
			redacted: `{"credentialType":"SiteWideBmcRoot"}`,
			want:     map[string]any{"credentialType": "SiteWideBmcRoot"},
		},
		{
			// Copying into the nil map that JSON null decodes to would panic.
			name:        "redacted payload is null",
			redacted:    `null`,
			secretsJSON: `{"password":"s3cr3t"}`,
			wantErr:     true,
		},
		{
			name:        "secrets payload is null",
			redacted:    `{"credentialType":"SiteWideBmcRoot"}`,
			secretsJSON: `null`,
			wantErr:     true,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			out, err := MergeSecrets([]byte(tc.redacted), []byte(tc.secretsJSON))

			if tc.wantErr {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)

			var got map[string]any
			require.NoError(t, json.Unmarshal(out, &got))
			assert.Equal(t, tc.want, got)
		})
	}
}

// TestRedactSecretsMergeSecretsRoundTrip pins the property the two functions
// exist for: what the site merges back is what the cloud started with.
func TestRedactSecretsMergeSecretsRoundTrip(t *testing.T) {
	orig := []byte(`{"credentialType":"SiteWideBmcRoot","password":"s3cr3t","macAddress":"aa:bb"}`)

	redacted, secretsJSON, err := RedactSecrets(orig, []string{"password"})
	require.NoError(t, err)
	require.NotEmpty(t, secretsJSON)

	merged, err := MergeSecrets(redacted, secretsJSON)
	require.NoError(t, err)

	var got, want map[string]any
	require.NoError(t, json.Unmarshal(merged, &got))
	require.NoError(t, json.Unmarshal(orig, &want))
	assert.Equal(t, want, got)
}
