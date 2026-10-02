// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSCPFirmwareScripts(t *testing.T) {
	bashPath, err := exec.LookPath("bash")
	require.NoError(t, err)
	basenamePath, err := exec.LookPath("basename")
	require.NoError(t, err)

	tests := []struct {
		name    string
		script  string
		host    string
		scpHost string
	}{
		{name: "CPLD IPv6", script: "nvswupdCPLD.sh", host: "2001:db8::1", scpHost: "[2001:db8::1]"},
		{name: "CPLD IPv4", script: "nvswupdCPLD.sh", host: "192.0.2.1", scpHost: "192.0.2.1"},
		{name: "CPLD bracketed IPv6", script: "nvswupdCPLD.sh", host: "[2001:db8::1]", scpHost: "[2001:db8::1]"},
		{name: "NVOS IPv6", script: "nvswupdNVOS.sh", host: "2001:db8::1", scpHost: "[2001:db8::1]"},
		{name: "NVOS IPv4", script: "nvswupdNVOS.sh", host: "192.0.2.1", scpHost: "192.0.2.1"},
		{name: "NVOS bracketed IPv6", script: "nvswupdNVOS.sh", host: "[2001:db8::1]", scpHost: "[2001:db8::1]"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			recordDir := t.TempDir()
			// Record SCP and the first SSH call, then stop before any firmware operation.
			recorder := "#!/bin/sh\nprintf '%s\\000' \"$@\" > \"$NSM_TEST_RECORD_DIR/$3\"\n[ \"$3\" = scp ]\n"
			err := os.WriteFile(filepath.Join(dir, "sshpass"), []byte(recorder), 0o700)
			require.NoError(t, err)
			err = os.WriteFile(filepath.Join(dir, "sleep"), []byte("#!/bin/sh\nexit 0\n"), 0o700)
			require.NoError(t, err)
			err = os.Symlink(basenamePath, filepath.Join(dir, "basename"))
			require.NoError(t, err)
			firmwarePath := filepath.Join(dir, "firmware.bin")
			err = os.WriteFile(firmwarePath, []byte("firmware"), 0o600)
			require.NoError(t, err)

			scriptPath, err := filepath.Abs(filepath.Join("..", "..", "scripts", tt.script))
			require.NoError(t, err)
			ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
			defer cancel()
			command := exec.CommandContext(ctx, bashPath, scriptPath, tt.host, "admin", "test-password", firmwarePath)
			command.Env = append(os.Environ(), "PATH="+dir, "NSM_TEST_RECORD_DIR="+recordDir)
			command.WaitDelay = time.Second
			output, err := command.CombinedOutput()
			var exitError *exec.ExitError
			require.ErrorAs(t, err, &exitError, "%s", output)
			assert.Equal(t, 1, exitError.ExitCode())
			assert.Contains(t, string(output), "Remote file not found after copy")

			for _, call := range []struct {
				name string
				args []string
			}{
				{name: "scp", args: []string{
					"-p", "test-password", "scp", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
					firmwarePath, "admin@" + tt.scpHost + ":/home/admin",
				}},
				{name: "ssh", args: []string{
					"-p", "test-password", "ssh", "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
					"admin@" + tt.host, "ls -l \"/home/admin/firmware.bin\"",
				}},
			} {
				record, err := os.ReadFile(filepath.Join(recordDir, call.name))
				require.NoError(t, err)
				assert.Equal(t, call.args, strings.Split(strings.TrimSuffix(string(record), "\x00"), "\x00"), call.name)
			}
		})
	}
}
