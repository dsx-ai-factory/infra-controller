// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package middleware

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestObfuscateRequestBody(t *testing.T) {
	tests := []struct {
		name string
		body interface{}
		want interface{}
	}{
		{
			name: "obfuscates authenticationData and preserves non-secret fields",
			body: map[string]interface{}{
				"siteId": "site-1",
				"authenticationData": map[string]interface{}{
					"shared": "download-token",
				},
			},
			want: map[string]interface{}{
				"siteId":             "site-1",
				"authenticationData": auditObfuscatedValue,
			},
		},
		{
			// Regression: the BMC credential password field must never be
			// persisted in plaintext in the audit body. It is not redacted by
			// the handler's Temporal-payload redaction, which is a separate path.
			name: "obfuscates BMC credential password",
			body: map[string]interface{}{
				"siteId":             "site-1",
				"kind":               "SiteWideRoot",
				"password":           "synthetic-secret",
				"defaultBmcPassword": "synthetic-default",
			},
			want: map[string]interface{}{
				"siteId":             "site-1",
				"kind":               "SiteWideRoot",
				"password":           auditObfuscatedValue,
				"defaultBmcPassword": auditObfuscatedValue,
			},
		},
		{
			name: "obfuscates credential fields case-insensitively",
			body: map[string]interface{}{
				"DefaultBmcPassword": "synthetic-default",
				"CLIENTSECRET":       "synthetic-secret",
				"clientSecret":       "synthetic-second-secret",
			},
			want: map[string]interface{}{
				"DefaultBmcPassword": auditObfuscatedValue,
				"CLIENTSECRET":       auditObfuscatedValue,
				"clientSecret":       auditObfuscatedValue,
			},
		},
		{
			// Regression: the expected-switch NVOS password field must never be
			// persisted in plaintext in the audit body.
			name: "obfuscates expected switch nvOsPassword",
			body: map[string]interface{}{
				"nvOsUsername": "admin",
				"nvOsPassword": "synthetic-secret",
			},
			want: map[string]interface{}{
				"nvOsUsername": "admin",
				"nvOsPassword": auditObfuscatedValue,
			},
		},
		{
			name: "obfuscates image authentication token",
			body: map[string]interface{}{
				"imageAuthType":  "Bearer",
				"imageAuthToken": "synthetic-token",
			},
			want: map[string]interface{}{
				"imageAuthType":  "Bearer",
				"imageAuthToken": auditObfuscatedValue,
			},
		},
		{
			name: "obfuscates authentication token nested in an array",
			body: []interface{}{
				map[string]interface{}{
					"name":      "first",
					"authToken": "synthetic-token",
				},
			},
			want: []interface{}{
				map[string]interface{}{
					"name":      "first",
					"authToken": auditObfuscatedValue,
				},
			},
		},
		{
			name: "obfuscates tenant identity client secret",
			body: map[string]interface{}{
				"clientSecretBasic": map[string]interface{}{
					"clientId":     "client-1",
					"clientSecret": "synthetic-secret",
				},
			},
			want: map[string]interface{}{
				"clientSecretBasic": map[string]interface{}{
					"clientId":     "client-1",
					"clientSecret": auditObfuscatedValue,
				},
			},
		},
		{
			name: "obfuscates sensitive fields nested in arrays and objects",
			body: []interface{}{
				map[string]interface{}{
					"name": "first",
					"credentials": map[string]interface{}{
						"password": "synthetic-secret",
					},
				},
			},
			want: []interface{}{
				map[string]interface{}{
					"name": "first",
					"credentials": map[string]interface{}{
						"password": auditObfuscatedValue,
					},
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			obfuscateRequestBody(tt.body)
			assert.Equal(t, tt.want, tt.body)
		})
	}
}
