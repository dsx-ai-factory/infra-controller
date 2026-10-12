// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	"bytes"
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
		// previous is the health the last check left behind.
		previous   computils.CompStatus
		wantHealth computils.CompStatus
		wantErr    string
		// wantLog is the message the check logs along with the error, or empty for none.
		wantLog string
	}{
		{
			name:       "first failure after startup",
			previous:   computils.CompNotKnown,
			wantHealth: computils.CompUnhealthy,
			wantErr:    client.ErrCoreGrpcClientNotConnected.Error(),
			wantLog:    "Core gRPC: health check failed",
		},
		{
			name:       "Version fails again",
			connected:  true,
			versionErr: versionErr,
			previous:   computils.CompUnhealthy,
			wantHealth: computils.CompUnhealthy,
			wantErr:    versionErr.Error(),
		},
		{
			name:       "Version succeeds",
			connected:  true,
			previous:   computils.CompUnhealthy,
			wantHealth: computils.CompHealthy,
			wantLog:    "Core gRPC: health check passed",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			previousAccess := ManagerAccess
			t.Cleanup(func() { ManagerAccess = previousAccess })
			var logged bytes.Buffer
			data := &elektratypes.Elektra{Managers: managertypes.NewManagerType(), Log: zerolog.New(&logged)}
			data.Managers.CoreGrpc.State.HealthStatus.Store(uint64(tt.previous))
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
			assert.Equal(t, tt.wantErr, state.Err.Load())
			if tt.wantLog == "" {
				assert.Empty(t, logged.String())
				return
			}
			assert.Contains(t, logged.String(), tt.wantLog)
			assert.Contains(t, logged.String(), tt.wantErr)
		})
	}
}
