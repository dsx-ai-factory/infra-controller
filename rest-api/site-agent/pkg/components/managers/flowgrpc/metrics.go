// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package flowgrpc

import (
	"context"
	"errors"
	"fmt"
	"time"

	flowgrpctypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/flowgrpc"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/prometheus/client_golang/prometheus"
)

const (
	metricFlowGrpcLatency     = "flow_grpc_client_latency_seconds"
	metricFlowWorkflowLatency = "flow_workflow_latency_seconds"
)

type grpcClientMetrics struct {
	responseLatency *prometheus.HistogramVec
}

func makeGrpcClientMetrics() client.Metrics {
	metrics := &grpcClientMetrics{
		responseLatency: prometheus.NewHistogramVec(
			prometheus.HistogramOpts{
				Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
				Name:      metricFlowGrpcLatency,
				Help:      "Response latency of each RPC",
				Buckets:   []float64{0.0005, 0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0},
			},
			[]string{"grpc_method", "grpc_status_code"}),
	}
	// Use Register (not MustRegister) and tolerate a duplicate registration: the
	// manager-level CreateGrpcClient is retried until it succeeds (e.g. while
	// the Flow service is not yet reachable at startup), and each attempt
	// re-enters this function. MustRegister would panic on the second attempt
	// and turn an otherwise-recoverable retry into a crash loop. On a duplicate,
	// reuse the already-registered collector.
	if err := prometheus.Register(metrics.responseLatency); err != nil {
		are := prometheus.AlreadyRegisteredError{}
		if errors.As(err, &are) {
			existing, ok := are.ExistingCollector.(*prometheus.HistogramVec)
			if !ok {
				panic(fmt.Errorf("flowgrpc: metric %q already registered as unexpected type %T", metricFlowGrpcLatency, are.ExistingCollector))
			}
			metrics.responseLatency = existing
		} else {
			panic(err)
		}
	}
	return metrics
}

func (m *grpcClientMetrics) RecordRpcResponse(ctx context.Context, method, code string, duration time.Duration) {
	event := ManagerAccess.Data.EB.Log.Debug()
	// Tag the log with the RPC's trace id so an operator can thread this call back to the
	// workflow that issued it. Omitted when the call carries no span (TraceIDFromContext is
	// panic-safe and returns "" in that case).
	if traceID := client.TraceIDFromContext(ctx); traceID != "" {
		event = event.Str("trace_id", traceID)
	}
	event.Msgf("method=%s, code=%s, duration=%v", method, code, duration)
	m.responseLatency.WithLabelValues(method, code).Observe(duration.Seconds())
}

type wflowMetrics struct {
	latency *prometheus.HistogramVec
}

func newWorkflowMetrics() flowgrpctypes.WorkflowMetrics {
	metrics := &wflowMetrics{
		latency: prometheus.NewHistogramVec(
			prometheus.HistogramOpts{
				Namespace: ManagerAccess.Conf.EB.MetricsNamespace,
				Name:      metricFlowWorkflowLatency,
				Help:      "Latency of each workflow",
				Buckets:   []float64{0.0005, 0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0},
			},
			[]string{"activity", "status"}),
	}
	// See makeGrpcClientMetrics: tolerate a duplicate registration on retry
	// instead of panicking, reusing the already-registered collector.
	if err := prometheus.Register(metrics.latency); err != nil {
		are := prometheus.AlreadyRegisteredError{}
		if errors.As(err, &are) {
			existing, ok := are.ExistingCollector.(*prometheus.HistogramVec)
			if !ok {
				panic(fmt.Errorf("flowgrpc: metric %q already registered as unexpected type %T", metricFlowWorkflowLatency, are.ExistingCollector))
			}
			metrics.latency = existing
		} else {
			panic(err)
		}
	}
	return metrics
}

func (m *wflowMetrics) RecordLatency(activity string, status flowgrpctypes.WorkflowStatus, duration time.Duration) {
	ManagerAccess.Data.EB.Log.Debug().Msgf("activity=%s, status=%s, duration=%v", activity, status, duration)
	m.latency.WithLabelValues(activity, string(status)).Observe(duration.Seconds())
}
