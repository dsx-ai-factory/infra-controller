// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestAPI_CheckReadiness(t *testing.T) {
	versionErr := status.Error(codes.Unavailable, "connection refused")
	tests := []struct {
		name       string
		connected  bool
		versionErr error
		wantErr    error
	}{
		{name: "no Core gRPC client yet", wantErr: client.ErrCoreGrpcClientNotConnected},
		{name: "Version fails", connected: true, versionErr: versionErr, wantErr: versionErr},
		{name: "Version succeeds", connected: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			previousAccess := ManagerAccess
			t.Cleanup(func() { ManagerAccess = previousAccess })
			data := &elektratypes.Elektra{Managers: managertypes.NewManagerType()}
			if tt.connected {
				data.Managers.CoreGrpc.Client.SwapClient(client.NewMockCoreGrpcClient())
			}
			NewCoreGrpcManager(data, nil, &managerapi.ManagerConf{})

			ctx := context.Background()
			if tt.versionErr != nil {
				ctx = client.WithMockSitePrefixVersionError(ctx, tt.versionErr)
			}
			assert.ErrorIs(t, (&API{}).CheckReadiness(ctx), tt.wantErr)
		})
	}
}
