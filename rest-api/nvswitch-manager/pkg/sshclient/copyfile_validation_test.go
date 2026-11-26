// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshclient

import (
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestNVOSClient_CopyFile(t *testing.T) {
	localPath := filepath.Join(t.TempDir(), "missing-firmware.bin")
	tests := []struct {
		name       string
		remotePath string
		wantError  string
	}{
		{
			name:       "relative remote path rejected before local open",
			remotePath: "firmware/image.bin",
			wantError:  "remote path must be absolute: firmware/image.bin",
		},
		{
			name:       "unsafe remote directory rejected before local open",
			remotePath: "/tmp/firmware;invalid/image.bin",
			wantError:  "remote directory contains invalid characters: /tmp/firmware;invalid",
		},
		{
			name:       "unsafe remote filename rejected before local open",
			remotePath: "/tmp/firmware/image;invalid.bin",
			wantError:  "remote filename contains invalid characters: image;invalid.bin",
		},
		{
			name:       "safe absolute path reaches local open before SSH",
			remotePath: "/tmp/firmware/image.bin",
			wantError:  "failed to open local file: open " + localPath,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// A nil SSH transport makes unexpected session access fail loudly.
			var client NVOSClient
			err := client.CopyFile(localPath, tt.remotePath)
			require.ErrorContains(t, err, tt.wantError)
		})
	}
}
