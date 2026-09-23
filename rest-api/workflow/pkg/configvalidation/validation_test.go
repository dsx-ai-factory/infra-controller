// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package configvalidation

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/require"
)

const validYAML = `db:
  name: nico
  user: worker
  password: local
temporal:
  serverName: temporal.local
  namespace: cloud
  queue: cloud
  encryptionKey: local
  tls:
    certPath: /not/a/real/cert
    keyPath: /not/a/real/key
    caPath: /not/a/real/ca
`

func TestValidateYAML(t *testing.T) {
	cases := []struct{ name, yaml, errorContains string }{
		{"valid", validYAML, ""},
		{"valid upper poller", validYAML + "worker:\n  maxConcurrentActivityPollers: 20\n", ""},
		{"tls disabled", strings.Replace(validYAML, "  tls:\n    certPath: /not/a/real/cert\n    keyPath: /not/a/real/key\n    caPath: /not/a/real/ca\n", "  tls:\n    enabled: false\n", 1), ""},
		{"malformed yaml", "db: [broken", "parse workflow YAML"},
		{"missing database name", strings.Replace(validYAML, "  name: nico\n", "", 1), "db name"},
		{"missing database password", strings.Replace(validYAML, "  password: local\n", "", 1), "db password"},
		{"missing tls certificate", strings.Replace(validYAML, "    certPath: /not/a/real/cert\n", "", 1), "temporal cert path"},
		{"missing temporal key source", strings.Replace(validYAML, "  encryptionKey: local\n", "", 1), "temporal encryption key or encryption key path"},
		{"missing temporal namespace", strings.Replace(validYAML, "  namespace: cloud\n", "", 1), "temporal namespace"},
		{"poller zero", validYAML + "worker:\n  maxConcurrentActivityPollers: 0\n", "between 1 and 20"},
		{"poller 21", validYAML + "worker:\n  maxConcurrentActivityPollers: 21\n", "between 1 and 20"},
		{"missing secret paths untouched", strings.Replace(strings.Replace(validYAML, "  password: local\n", "  passwordPath: /no/such/secret\n", 1), "  encryptionKey: local\n", "  encryptionKeyPath: /no/such/key\n", 1), ""},
		{"no environment override", strings.Replace(validYAML, "  user: worker\n", "", 1), "db user"},
		{"state isolated after failure", validYAML, ""},
	}
	t.Setenv("DB_USER", "injected")
	t.Setenv("CONFIG_FILE_PATH", "/no/such/config.yaml")
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := ValidateYAML(tc.yaml)
			if tc.errorContains == "" {
				require.NoError(t, err)
			} else {
				require.ErrorContains(t, err, tc.errorContains)
			}
		})
	}
}
