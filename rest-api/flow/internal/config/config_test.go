// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestReadConfig(t *testing.T) {
	for _, tc := range []struct {
		name    string
		content string
		missing bool
		enabled bool
	}{
		{name: "missing file", missing: true},
		{name: "predecessor config", content: "disable_inventory: false\n"},
		{name: "explicitly disabled", content: "tracing:\n  enabled: false\n"},
		{name: "enabled", content: "tracing:\n  enabled: true\n", enabled: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			filename := filepath.Join(t.TempDir(), "flowconfig.yaml")
			if !tc.missing {
				require.NoError(t, os.WriteFile(filename, []byte(tc.content), 0o600))
			}
			t.Setenv("FLOW_CONFIG_FILE", filename)
			t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4317")
			cfg := ReadConfig()
			assert.Equal(t, tc.enabled, cfg.Tracing.Enabled)
			assert.Equal(t, time.Minute, cfg.InventoryRunFrequency)
		})
	}
}
