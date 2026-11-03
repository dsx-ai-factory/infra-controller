// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// GetComponentPowerDetails uses the shared client and keeps a shorter caller
// deadline. A tray request selects one ID, within Core's 1–64 target contract.
func (c *grpcClient) GetComponentPowerDetails(ctx context.Context, request *corev1.GetComponentPowerDetailsRequest) (*corev1.GetComponentPowerDetailsResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.GetComponentPowerDetails(ctx, request)
}
