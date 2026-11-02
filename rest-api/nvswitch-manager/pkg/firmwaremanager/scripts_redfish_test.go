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

func TestRedfishScriptURLs(t *testing.T) {
	cases := []struct {
		name string
		ip   string
		host string
	}{
		{name: "IPv6", ip: "2001:db8::1", host: "[2001:db8::1]"},
		{name: "IPv4", ip: "192.0.2.1", host: "192.0.2.1"},
	}
	for _, tc := range cases {
		for _, script := range []struct {
			name      string
			urls      map[string]string
			pingCount int
		}{
			{name: "nvswfwupd.sh", urls: map[string]string{"POST": "/redfish/v1/UpdateService", "GET": "/redfish/v1/TaskService/Tasks/1"}, pingCount: 1},
			{name: "nvswpwrcyc.sh", urls: map[string]string{"POST": "/redfish/v1/Systems/System_0/Actions/ComputerSystem.Reset"}, pingCount: 2},
		} {
			t.Run(tc.name+"/"+script.name, func(t *testing.T) {
				dir := t.TempDir()
				// Only the harmless parsing tools and our network stubs are reachable.
				for _, tool := range []string{"bash", "mktemp", "grep", "head", "sed", "rm"} {
					path, err := exec.LookPath(tool)
					require.NoError(t, err)
					require.NoError(t, os.Symlink(path, filepath.Join(dir, tool)))
				}
				stubs := map[string]string{
					"sleep": "#!/bin/sh\nexit 0\n",
					"ping": `#!/bin/sh
printf '%s\000' "$@" >> "$NSM_TEST_RECORD/ping-args"
if [ "$NSM_TEST_SCRIPT" = nvswpwrcyc.sh ] && [ ! -e "$NSM_TEST_RECORD/down" ]; then
  : > "$NSM_TEST_RECORD/down"
  exit 1
fi
`,
					"curl": `#!/usr/bin/env bash
args=("$@")
while (($#)); do
  case "$1" in
    -X) method="$2"; shift ;;
    -o) output="$2"; shift ;;
    https://*) url="$1" ;;
  esac
  shift
done
printf '%s\000' "${args[@]}" > "$NSM_TEST_RECORD/curl-$method"
case "$url" in
  */UpdateService) response='{"@odata.id":"/redfish/v1/TaskService/Tasks/1"}' ;;
  */Tasks/1) response='{"TaskState":"Completed","TaskStatus":"OK","PercentComplete":100}' ;;
  */ComputerSystem.Reset) response='{"MessageSeverity":"OK","MessageId":"Base.1.18.1.Success"}' ;;
  *) exit 1 ;;
esac
printf '%s\n' "$response" > "$output"
`,
				}
				for name, body := range stubs {
					require.NoError(t, os.WriteFile(filepath.Join(dir, name), []byte(body), 0o700))
				}
				firmware := filepath.Join(dir, "firmware.bin")
				require.NoError(t, os.WriteFile(firmware, []byte("firmware"), 0o600))
				t.Setenv("PATH", dir)
				t.Setenv("TMPDIR", dir)
				t.Setenv("NSM_TEST_RECORD", dir)
				t.Setenv("NSM_TEST_SCRIPT", script.name)
				ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
				defer cancel()
				cmd := exec.CommandContext(ctx, filepath.Join(dir, "bash"), filepath.Join("..", "..", "scripts", script.name), tc.ip, "admin", "test-password", firmware)
				cmd.WaitDelay = time.Second
				output, err := cmd.CombinedOutput()
				require.NoError(t, err, "%s", output)

				for method, path := range script.urls {
					record, err := os.ReadFile(filepath.Join(dir, "curl-"+method))
					require.NoError(t, err)
					args := strings.Split(strings.TrimSuffix(string(record), "\x00"), "\x00")
					assert.Contains(t, args, "https://"+tc.host+path)
					assert.Contains(t, args, "admin:test-password")
				}
				ping, err := os.ReadFile(filepath.Join(dir, "ping-args"))
				require.NoError(t, err)
				assert.Equal(t, strings.Repeat("-W\x001\x00-c\x001\x00"+tc.ip+"\x00", script.pingCount), string(ping))
			})
		}
	}
}
