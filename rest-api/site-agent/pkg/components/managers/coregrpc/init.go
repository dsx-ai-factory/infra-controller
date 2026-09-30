// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	"context"
	"fmt"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/prometheus/client_golang/prometheus"
)

const (
	// MetricCoreGrpcStatus - Metric Core gRPC Status
	MetricCoreGrpcStatus = "carbide_health_status" // TODO: rename to core_grpc_health_status without breaking existing Grafana dashboards
)

// Init initializes the Core gRPC client manager
func (coregrpc *API) Init() {
	ManagerAccess.Data.EB.Log.Info().Msg("Core gRPC: Initializing Core gRPC client manager")

	prometheus.MustRegister(
		prometheus.NewGaugeFunc(prometheus.GaugeOpts{
			Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
			Name:      MetricCoreGrpcStatus,
			Help:      "Core gRPC health status",
		},
			func() float64 {
				return float64(ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Load())
			}))
	ManagerAccess.Data.EB.Managers.CoreGrpc.State.HealthStatus.Store(uint64(computils.CompNotKnown))

	// initialize workflow metrics
	ManagerAccess.Data.EB.Managers.CoreGrpc.State.WflowMetrics = newWorkflowMetrics()
}

// Start starts the Core gRPC client manager
func (coregrpc *API) Start() {
	ManagerAccess.Data.EB.Log.Info().Msg("Core gRPC: Starting Core gRPC client manager")

	// Site Agent should not be able to start if the Core gRPC client cannot be created
	start := time.Now()
	backoff := client.CoreGrpcConnectionBackoffInitial
	for {
		err := coregrpc.CreateGrpcClient()
		if err == nil {
			ManagerAccess.Data.EB.Log.Info().Msg("Core gRPC: successfully created gRPC client")
			break
		}
		if time.Since(start) >= client.CoreGrpcConnectionRetryTimeout {
			panic(fmt.Errorf("core gRPC: failed to create gRPC client within %s: %w", client.CoreGrpcConnectionRetryTimeout, err))
		}
		ManagerAccess.Data.EB.Log.Error().Err(err).Dur("RetryIn", backoff).Msg("Core gRPC: failed to create gRPC client, retrying")
		time.Sleep(backoff)
		backoff *= 2
		if backoff > client.CoreGrpcConnectionBackoffMax {
			backoff = client.CoreGrpcConnectionBackoffMax
		}
	}
}

// GetState returns the current state of the Core gRPC client manager
func (coregrpc *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.CoreGrpc.State
	var strs []string
	strs = append(strs, fmt.Sprintln(" GRPC Succeeded:", state.GrpcSucc.Load()))
	strs = append(strs, fmt.Sprintln(" GRPC Failed:", state.GrpcFail.Load()))
	strs = append(strs, fmt.Sprintln(" GRPC Status:", computils.CompStatus(state.HealthStatus.Load())))
	strs = append(strs, fmt.Sprintln(" GRPC Last Error:", state.Err()))

	return strs
}

// CheckConnection calls Core's Version RPC and records whether it succeeded in the
// Core gRPC state. Without DisplayConfig, Version only returns build information.
func (coregrpc *API) CheckConnection(ctx context.Context) {
	err := client.ErrCoreGrpcClientNotConnected
	grpcClient := ManagerAccess.Data.EB.Managers.CoreGrpc.GetClient()
	if grpcClient != nil {
		_, err = grpcClient.GrpcServiceClient().Version(ctx, &corev1.VersionRequest{})
	}

	state := ManagerAccess.Data.EB.Managers.CoreGrpc.State
	if err != nil {
		state.SetErr(err.Error())
		previous := state.HealthStatus.Swap(uint64(computils.CompUnhealthy))
		if computils.CompStatus(previous) == computils.CompHealthy {
			ManagerAccess.Data.EB.Log.Warn().Err(err).Msg("Core gRPC: health check failed")
		}
		return
	}
	previous := state.HealthStatus.Swap(uint64(computils.CompHealthy))
	if computils.CompStatus(previous) != computils.CompHealthy {
		ManagerAccess.Data.EB.Log.Info().Msg("Core gRPC: health check passed")
	}
}

// GetGrpcClientVersion returns the current version of the Core gRPC client
func (coregrpc *API) GetGrpcClientVersion() int64 {
	return ManagerAccess.Data.EB.Managers.CoreGrpc.Client.Version()
}
