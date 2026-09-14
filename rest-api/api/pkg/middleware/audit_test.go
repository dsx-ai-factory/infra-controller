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
		body map[string]interface{}
		want map[string]interface{}
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
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			obfuscateRequestBody(tt.body)
			assert.Equal(t, tt.want, tt.body)
		})
	}
}
