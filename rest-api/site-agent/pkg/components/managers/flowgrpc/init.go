// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpc

import (
	"context"
	"errors"
	"fmt"
	"time"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/prometheus/client_golang/prometheus"
)

const (
	// MetricFlowStatus is the metric name for the Flow gRPC health status
	MetricFlowStatus = "flow_grpc_health_status"
)

// Init initializes the Flow gRPC client manager
func (flowgrpc *API) Init() {
	// Check if Flow is enabled via environment variable
	if !ManagerAccess.Conf.EB.FlowGrpc.Enabled {
		ManagerAccess.Data.EB.Log.Info().Msg("Flow: Flow gRPC is disabled, skipping initialization")
		return
	}

	ManagerAccess.Data.EB.Log.Info().Msg("Flow: Initializing Flow gRPC client manager")

	gauge := prometheus.NewGaugeFunc(prometheus.GaugeOpts{
		Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
		Name:      MetricFlowStatus,
		Help:      "Flow gRPC health status",
	},
		func() float64 {
			return float64(ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Load())
		})
	if err := prometheus.Register(gauge); err != nil {
		are := prometheus.AlreadyRegisteredError{}
		if errors.As(err, &are) {
			// AlreadyRegisteredError fires when Init() is called more than once
			// (the common retry case). ExistingCollector will be the GaugeFunc we
			// registered on the first call, so the assertion below passes and we
			// continue safely — no crashloop. The panic only fires if a foreign
			// collector of a different type owns the name, which would leave flow
			// gRPC health status silently unreported.
			if _, ok := are.ExistingCollector.(prometheus.GaugeFunc); !ok {
				panic(fmt.Errorf("flowgrpc: metric %q already registered as unexpected type %T", MetricFlowStatus, are.ExistingCollector))
			}
		} else {
			panic(fmt.Errorf("flowgrpc: failed to register metric %q: %w", MetricFlowStatus, err))
		}
	}

	ManagerAccess.Data.EB.Managers.FlowGrpc.State.HealthStatus.Store(uint64(computils.CompNotKnown))

	// initialize workflow metrics
	ManagerAccess.Data.EB.Managers.FlowGrpc.State.WflowMetrics = newWorkflowMetrics()
}

// Start starts the Flow gRPC client manager
func (flowgrpc *API) Start() {
	ManagerAccess.Data.EB.Log.Info().Msg("Flow gRPC: Starting Flow gRPC client manager")

	// Check if Flow is enabled via environment variable
	if !ManagerAccess.Conf.EB.FlowGrpc.Enabled {
		ManagerAccess.Data.EB.Log.Info().Msg("Flow gRPC: Flow gRPC is disabled, skipping initialization")
		return
	}

	// Site Agent should not be able to start if the Flow gRPC is enabled but the client cannot be created
	start := time.Now()
	backoff := client.FlowGrpcConnectionBackoffInitial
	for {
		err := flowgrpc.CreateGrpcClient()
		if err == nil {
			ManagerAccess.Data.EB.Log.Info().Msg("Flow gRPC: successfully created gRPC client")
			break
		}
		if time.Since(start) >= client.FlowGrpcConnectionRetryTimeout {
			panic(fmt.Errorf("flow gRPC: failed to create gRPC client within %s: %w", client.FlowGrpcConnectionRetryTimeout, err))
		}
		ManagerAccess.Data.EB.Log.Error().Err(err).Dur("RetryIn", backoff).Msg("Flow gRPC: failed to create gRPC client, retrying")
		time.Sleep(backoff)
		backoff *= 2
		if backoff > client.FlowGrpcConnectionBackoffMax {
			backoff = client.FlowGrpcConnectionBackoffMax
		}
	}
}

// GetState returns the current state of the Flow gRPC client manager
func (flowgrpc *API) GetState() []string {
	state := ManagerAccess.Data.EB.Managers.FlowGrpc.State
	var strs []string
	strs = append(strs, fmt.Sprintln(" Flow GRPC Succeeded:", state.GrpcSucc.Load()))
	strs = append(strs, fmt.Sprintln(" Flow GRPC Failed:", state.GrpcFail.Load()))
	strs = append(strs, fmt.Sprintln(" Flow GRPC Status:", computils.CompStatus(state.HealthStatus.Load())))
	strs = append(strs, fmt.Sprintln(" Flow GRPC Last Error:", state.Err.Load()))

	return strs
}

// CheckConnection calls Flow's Version RPC and records whether it succeeded in the Flow
// gRPC state. Each Flow RPC also records its result, but readiness needs the state kept
// current while no workflow calls Flow.
func (flowgrpc *API) CheckConnection(ctx context.Context) {
	grpcServiceClient, err := ManagerAccess.Data.EB.Managers.FlowGrpc.Client.GrpcServiceClient()
	if err == nil {
		_, err = grpcServiceClient.Version(ctx, &flowv1.VersionRequest{})
	}

	state := ManagerAccess.Data.EB.Managers.FlowGrpc.State
	if err != nil {
		state.Err.Store(err.Error())
		// Logs every change to Unhealthy, including from NotKnown when Flow was never
		// reachable. The probes and the status page leave the error out.
		previous := state.HealthStatus.Swap(uint64(computils.CompUnhealthy))
		if computils.CompStatus(previous) != computils.CompUnhealthy {
			ManagerAccess.Data.EB.Log.Warn().Err(err).Msg("Flow gRPC: health check failed")
		}
		return
	}
	previous := state.HealthStatus.Swap(uint64(computils.CompHealthy))
	if computils.CompStatus(previous) != computils.CompHealthy {
		ManagerAccess.Data.EB.Log.Info().Msg("Flow gRPC: health check passed")
	}
}

// GetGrpcClientVersion returns the current version of the Flow gRPC client
func (flowgrpc *API) GetGrpcClientVersion() int64 {
	return ManagerAccess.Data.EB.Managers.FlowGrpc.Client.Version()
}
