// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpc

import (
	"bytes"
	"context"
	"testing"

	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/managerapi"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/elektratypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
)

func TestAPI_CheckConnection(t *testing.T) {
	tests := []struct {
		name      string
		connected bool
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
			wantErr:    client.ErrFlowGrpcClientNotConnected.Error(),
			wantLog:    "Flow gRPC: health check failed",
		},
		{
			name:       "fails again",
			previous:   computils.CompUnhealthy,
			wantHealth: computils.CompUnhealthy,
			wantErr:    client.ErrFlowGrpcClientNotConnected.Error(),
		},
		{
			name:       "Version succeeds",
			connected:  true,
			previous:   computils.CompUnhealthy,
			wantHealth: computils.CompHealthy,
			wantLog:    "Flow gRPC: health check passed",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			previousAccess := ManagerAccess
			t.Cleanup(func() { ManagerAccess = previousAccess })
			var logged bytes.Buffer
			data := &elektratypes.Elektra{Managers: managertypes.NewManagerType(), Log: zerolog.New(&logged)}
			data.Managers.FlowGrpc.State.HealthStatus.Store(uint64(tt.previous))
			if tt.connected {
				data.Managers.FlowGrpc.Client.SwapClient(client.NewMockFlowGrpcClient())
			}
			NewFlowGrpcManager(data, nil, &managerapi.ManagerConf{})

			(&API{}).CheckConnection(context.Background())

			state := data.Managers.FlowGrpc.State
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
