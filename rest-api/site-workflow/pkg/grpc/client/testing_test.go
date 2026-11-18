// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"testing"

	"github.com/stretchr/testify/require"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestMockCoreGrpcServiceClient_FindVpcIds(t *testing.T) {
	ctx := context.WithValue(context.Background(), MockWantCount, 2)
	ctx = context.WithValue(ctx, "wantCount", 3) //nolint:staticcheck // Exercise isolation from caller-owned string keys.

	ids, err := (&MockCoreGrpcServiceClient{}).FindVpcIds(ctx, &corev1.VpcSearchFilter{})
	require.NoError(t, err)
	require.Len(t, ids.VpcIds, 2)
}
