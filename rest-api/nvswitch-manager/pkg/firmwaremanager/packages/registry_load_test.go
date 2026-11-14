// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package packages_test

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestRegistry_LoadFromDirectory(t *testing.T) {
	writeFile := func(t *testing.T, path, content string) {
		t.Helper()
		require.NoError(t, os.MkdirAll(filepath.Dir(path), 0o700))
		require.NoError(t, os.WriteFile(path, []byte(content), 0o600))
	}
	packageYAML := func(version, description, firmwareFile string) string {
		return fmt.Sprintf(`version: %q
description: %q
components:
  bmc:
    version: "2.0.0"
    file: %q
    strategy: redfish
`, version, description, firmwareFile)
	}

	tests := []struct {
		name string
		run  func(t *testing.T, registry *packages.Registry, packagesDir string)
	}{
		{
			name: "mixed directory",
			run: func(t *testing.T, registry *packages.Registry, packagesDir string) {
				writeFile(t, filepath.Join(packagesDir, "00-malformed.yaml"), "version: [unterminated")
				writeFile(t, filepath.Join(packagesDir, "01-missing-firmware.yaml"), packageYAML("missing", "unavailable firmware", "missing.bin"))
				writeFile(t, filepath.Join(packagesDir, "02-first.yaml"), packageYAML("1.0.0", "first accepted", "bmc/firmware.bin"))
				writeFile(t, filepath.Join(packagesDir, "03-duplicate.yaml"), packageYAML("1.0.0", "must not overwrite", "bmc/firmware.bin"))
				writeFile(t, filepath.Join(packagesDir, "04-second.yml"), packageYAML("2.0.0", "YML package", "bmc/firmware.bin"))
				writeFile(t, filepath.Join(packagesDir, "05-ignored.txt"), packageYAML("ignored", "wrong extension", "bmc/firmware.bin"))
				writeFile(t, filepath.Join(packagesDir, "06-nested.yaml", "package.yaml"), packageYAML("nested", "subdirectory", "bmc/firmware.bin"))

				require.NoError(t, registry.LoadFromDirectory(packagesDir), "bad files must not hide valid packages")
				assert.Equal(t, 2, registry.Count())
				assert.ElementsMatch(t, []string{"1.0.0", "2.0.0"}, registry.List(), "only top-level YAML and YML packages with firmware are loaded")
				first, err := registry.Get("1.0.0")
				require.NoError(t, err)
				require.NotNil(t, first)
				assert.Equal(t, "first accepted", first.Description, "a duplicate must preserve the first accepted file")
				assert.Equal(t, map[string]packages.ComponentDef{
					"bmc": {Version: "2.0.0", File: "bmc/firmware.bin", Strategy: "redfish"},
				}, first.Components)
				second, err := registry.Get("2.0.0")
				require.NoError(t, err)
				require.NotNil(t, second)
				assert.Equal(t, "YML package", second.Description)
			},
		},
		{
			name: "reload",
			run: func(t *testing.T, registry *packages.Registry, packagesDir string) {
				path := filepath.Join(packagesDir, "bundle.yaml")
				writeFile(t, path, packageYAML("old", "stale bundle", "bmc/firmware.bin"))
				require.NoError(t, registry.LoadFromDirectory(packagesDir))
				assert.ElementsMatch(t, []string{"old"}, registry.List(), "establish the stale version before reload")

				writeFile(t, path, packageYAML("new", "replacement bundle", "bmc/firmware.bin"))
				require.NoError(t, registry.LoadFromDirectory(packagesDir))
				assert.Equal(t, 1, registry.Count())
				assert.ElementsMatch(t, []string{"new"}, registry.List(), "reload replaces the previous snapshot")
				loaded, err := registry.Get("new")
				require.NoError(t, err)
				require.NotNil(t, loaded)
				assert.Equal(t, "replacement bundle", loaded.Description)
			},
		},
		{
			name: "absent directory",
			run: func(t *testing.T, registry *packages.Registry, packagesDir string) {
				missing := filepath.Join(packagesDir, "absent")
				err := registry.LoadFromDirectory(missing)
				require.Error(t, err)
				assert.ErrorIs(t, err, os.ErrNotExist, "preserve the directory read error")
				assert.Contains(t, err.Error(), "failed to read packages directory "+missing)
				assert.Zero(t, registry.Count())
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			firmwareDir := t.TempDir()
			writeFile(t, filepath.Join(firmwareDir, "bmc", "firmware.bin"), "firmware fixture")
			tt.run(t, packages.NewRegistry(firmwareDir), t.TempDir())
		})
	}
}
