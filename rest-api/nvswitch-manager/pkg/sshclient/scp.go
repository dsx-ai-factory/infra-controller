// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sshclient

import "strings"

// SCPDestination formats a remote file argument for scp, preserving the path.
// IPv6 hosts need brackets so scp can distinguish address colons from the
// remote path delimiter. Hosts that already have brackets are left unchanged.
func SCPDestination(user, host, remotePath string) string {
	if strings.Contains(host, ":") && !strings.HasPrefix(host, "[") {
		host = "[" + host + "]"
	}
	return user + "@" + host + ":" + remotePath
}
