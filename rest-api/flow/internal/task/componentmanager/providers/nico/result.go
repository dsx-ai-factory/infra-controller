// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

// ResultIdentifier returns the identifier used by the request. MAC-targeted
// batches use Core's echoed MAC even when Core also resolved an external ID.
func ResultIdentifier(result *corev1.ComponentResult, useMACAddress bool) string {
	if result == nil {
		return ""
	}
	if useMACAddress {
		if mac := result.GetMacAddress(); mac != "" {
			return mac
		}
	}
	if id := result.GetComponentId(); id != "" {
		return id
	}
	return result.GetMacAddress()
}
