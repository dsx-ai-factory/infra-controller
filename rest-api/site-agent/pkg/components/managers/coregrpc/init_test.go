// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	"context"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestAPI_CheckConnection(t *testing.T) {
	versionErr := status.Error(codes.Unavailable, "connection refused")
	tests := []struct {
		name       string
		connected  bool
		versionErr error
		wantHealth computils.CompStatus
		wantErr    string
	}{
		{
			name:       "no Core gRPC client yet",
			wantHealth: computils.CompUnhealthy,
			wantErr:    client.ErrCoreGrpcClientNotConnected.Error(),
		},
		{
			name:       "Version fails",
			connected:  true,
			versionErr: versionErr,
			wantHealth: computils.CompUnhealthy,
			wantErr:    versionErr.Error(),
		},
		{name: "Version succeeds", connected: true, wantHealth: computils.CompHealthy},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			previousAccess := ManagerAccess
			t.Cleanup(func() { ManagerAccess = previousAccess })
			data := &elektratypes.Elektra{Managers: managertypes.NewManagerType(), Log: zerolog.Nop()}
			if tt.connected {
				data.Managers.CoreGrpc.Client.SwapClient(client.NewMockCoreGrpcClient())
			}
			NewCoreGrpcManager(data, nil, &managerapi.ManagerConf{})

			ctx := context.Background()
			if tt.versionErr != nil {
				ctx = client.WithMockSitePrefixVersionError(ctx, tt.versionErr)
			}
			(&API{}).CheckConnection(ctx)

			state := data.Managers.CoreGrpc.State
			assert.Equal(t, tt.wantHealth, computils.CompStatus(state.HealthStatus.Load()))
			assert.Equal(t, tt.wantErr, state.Err())
		})
	}
}
