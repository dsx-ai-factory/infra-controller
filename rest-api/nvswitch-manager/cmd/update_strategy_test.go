// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestUSSSHCopyCmd_Run(t *testing.T) {
	tests := []struct {
		name        string
		host        string
		port        string
		portArgs    []string
		destination string
	}{
		{name: "IPv6 with custom port", host: "2001:db8::1", port: "2222", portArgs: []string{"-P", "2222"}, destination: "admin@[2001:db8::1]:/tmp/firmware.bin"},
		{name: "bracketed IPv6", host: "[2001:db8::1]", port: "22", destination: "admin@[2001:db8::1]:/tmp/firmware.bin"},
		{name: "hostname", host: "nvos.example.com", port: "22", destination: "admin@nvos.example.com:/tmp/firmware.bin"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			recordPath := filepath.Join(dir, "scp-arguments")
			script := "#!/bin/sh\nprintf '%s\\000' \"$@\" > \"$NSM_TEST_SCP_RECORD\"\n"
			err := os.WriteFile(filepath.Join(dir, "sshpass"), []byte(script), 0o700)
			require.NoError(t, err)
			t.Setenv("PATH", dir)
			t.Setenv("NSM_TEST_SCP_RECORD", recordPath)
			firmwarePath := filepath.Join(dir, "firmware.bin")
			err = os.WriteFile(firmwarePath, []byte("firmware"), 0o600)
			require.NoError(t, err)

			for _, name := range []string{"nvos-ip", "nvos-port", "nvos-user", "nvos-pass", "file", "remote-dir"} {
				flag := updateStrategyCmd.PersistentFlags().Lookup(name)
				oldValue, oldChanged := flag.Value.String(), flag.Changed
				t.Cleanup(func() {
					assert.NoError(t, flag.Value.Set(oldValue))
					flag.Changed = oldChanged
				})
			}
			rootCmd.SetArgs([]string{
				"update-strategy", "ssh", "copy",
				"--nvos-ip", tt.host, "--nvos-port", tt.port,
				"--nvos-user", "admin", "--nvos-pass", "test-password",
				"--file", firmwarePath, "--remote-dir", "/tmp",
			})
			t.Cleanup(func() { rootCmd.SetArgs(nil) })
			command, err := rootCmd.ExecuteC()
			require.NoError(t, err)
			assert.Same(t, usSSHCopyCmd, command)

			record, err := os.ReadFile(recordPath)
			require.NoError(t, err)
			wantArgs := []string{
				"-p", "test-password", "scp",
				"-o", "StrictHostKeyChecking=no",
				"-o", "UserKnownHostsFile=/dev/null",
				"-o", "ConnectTimeout=30",
			}
			wantArgs = append(wantArgs, tt.portArgs...)
			wantArgs = append(wantArgs, firmwarePath, tt.destination)
			assert.Equal(t, wantArgs, strings.Split(strings.TrimSuffix(string(record), "\x00"), "\x00"))
		})
	}
}
