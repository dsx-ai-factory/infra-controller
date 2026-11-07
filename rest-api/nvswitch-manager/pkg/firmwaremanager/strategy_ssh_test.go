// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSSHStrategy_executeCopy(t *testing.T) {
	tests := []struct {
		name        string
		ip          string
		port        int
		wantPort    string
		destination string
	}{
		{name: "IPv6 with custom port", ip: "2001:db8::1", port: 2222, wantPort: "2222", destination: "admin@[2001:db8::1]:/tmp/firmware.bin"},
		{name: "IPv4 with default port", ip: "192.0.2.1", wantPort: "22", destination: "admin@192.0.2.1:/tmp/firmware.bin"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			recordPath := filepath.Join(dir, "scp-arguments")
			script := "#!/bin/sh\nprintf '%s\\000' \"$SSHPASS\" \"$@\" > \"$NSM_TEST_SCP_RECORD\"\n"
			err := os.WriteFile(filepath.Join(dir, "sshpass"), []byte(script), 0o700)
			require.NoError(t, err)
			t.Setenv("PATH", dir)
			t.Setenv("NSM_TEST_SCP_RECORD", recordPath)
			t.Setenv("SSHPASS", "old-password")
			firmwarePath := filepath.Join(dir, "firmware.bin")
			err = os.WriteFile(firmwarePath, []byte("firmware"), 0o600)
			require.NoError(t, err)

			cred := credential.New("admin", "test-password")
			n, err := nvos.New("00:11:22:33:44:55", tt.ip, &cred)
			require.NoError(t, err)
			n.SetPort(tt.port)
			strategy := NewSSHStrategy(&packages.SSHConfig{RemoteDir: "/tmp"})
			strategy.SetFirmwarePath(firmwarePath)
			result := strategy.executeCopy(t.Context(), &FirmwareUpdate{}, &nvswitch.NVSwitchTray{NVOS: n})
			require.NotNil(t, result.ExecContext, "SCP must start: %v", result.Error)
			require.Positive(t, result.ExecContext.PID)

			// Reap the recorder before reading its output. Bound this wait so a
			// broken recorder cannot leave the asynchronous child running.
			process, err := os.FindProcess(result.ExecContext.PID)
			require.NoError(t, err)
			timer := time.AfterFunc(5*time.Second, func() { _ = process.Kill() })
			state, err := process.Wait()
			timer.Stop()
			require.NoError(t, err)
			require.True(t, state.Success())
			assert.Equal(t, OutcomeWait, result.Type)
			assert.Equal(t, tt.ip, result.ExecContext.TargetIP)

			record, err := os.ReadFile(recordPath)
			require.NoError(t, err)
			assert.Equal(t, []string{
				"test-password",
				"-e", "scp",
				"-P", tt.wantPort,
				"-o", "StrictHostKeyChecking=no",
				"-o", "UserKnownHostsFile=/dev/null",
				"-o", "ConnectTimeout=30",
				firmwarePath, tt.destination,
			}, strings.Split(strings.TrimSuffix(string(record), "\x00"), "\x00"))
		})
	}
}
