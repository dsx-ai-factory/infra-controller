// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package temporal

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel"
	commonpb "go.temporal.io/api/common/v1"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/interceptor"
	"go.temporal.io/sdk/testsuite"
	"go.temporal.io/sdk/worker"
	"go.temporal.io/sdk/workflow"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

func TestClientOptions(t *testing.T) {
	tcs := []struct {
		descr            string
		bootstrapEnabled bool
		endpoint         string
		serviceName      string
		propagators      string
		wantInterceptors bool
	}{
		{
			descr:            "tracing interceptor attached when provider is active",
			bootstrapEnabled: true,
			endpoint:         "http://localhost:14318",
			wantInterceptors: true,
		},
		{
			descr:            "environment service name keeps interceptor active",
			bootstrapEnabled: true,
			endpoint:         "http://localhost:14318",
			serviceName:      "workflow-from-environment",
			wantInterceptors: true,
		},
		{
			descr:            "propagation remains when config disables export",
			endpoint:         "http://localhost:14318",
			wantInterceptors: true,
		},
		{
			descr:            "propagation remains without exporter endpoint",
			bootstrapEnabled: true,
			wantInterceptors: true,
		},
		{
			descr:       "explicitly disabled propagation without export",
			propagators: "none",
		},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			previousProvider := otel.GetTracerProvider()
			previousPropagator := otel.GetTextMapPropagator()
			t.Cleanup(func() {
				otel.SetTracerProvider(previousProvider)
				otel.SetTextMapPropagator(previousPropagator)
			})
			t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", tc.endpoint)
			t.Setenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "")
			t.Setenv("OTEL_TRACES_SAMPLER", "always_off")
			t.Setenv("OTEL_PROPAGATORS", tc.propagators)
			t.Setenv("OTEL_SERVICE_NAME", tc.serviceName)
			shutdown, err := cotel.Bootstrap(context.Background(), tc.bootstrapEnabled, "temporal-test")
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, shutdown(context.Background())) })

			opts, err := ClientOptions("temporal:7233", "cloud", nil, nil)
			require.NoError(t, err)

			assert.Equal(t, "temporal:7233", opts.HostPort)
			assert.Equal(t, "cloud", opts.Namespace)
			assert.NotNil(t, opts.DataConverter)
			if tc.wantInterceptors {
				// Exactly one: the SDK also applies it to workers built from
				// this client, so a second registration would double-wrap.
				assert.Len(t, opts.Interceptors, 1, "expected exactly one OTel tracing interceptor")
			} else {
				assert.Empty(t, opts.Interceptors)
			}
		})
	}
}

// TestTracingInterceptor proves the worker-facing contract: a worker gets an
// interceptor exactly when transport instrumentation is on, so workflow tasks
// join the trace the client started and nothing is attached when propagation
// is switched off.
func TestTracingInterceptor(t *testing.T) {
	tcs := []struct {
		descr           string
		propagators     string
		wantInterceptor bool
	}{
		{descr: "transport enabled", wantInterceptor: true},
		{descr: "propagation disabled", propagators: "none"},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			previousPropagator := otel.GetTextMapPropagator()
			t.Cleanup(func() { otel.SetTextMapPropagator(previousPropagator) })
			t.Setenv("OTEL_PROPAGATORS", tc.propagators)
			shutdown, err := cotel.Bootstrap(context.Background(), false, "temporal-test")
			require.NoError(t, err)
			t.Cleanup(func() { require.NoError(t, shutdown(context.Background())) })

			got, err := TracingInterceptor()

			require.NoError(t, err)
			assert.Equal(t, tc.wantInterceptor, got != nil)
		})
	}
}

// TestTracingInterceptorToleratesUnreadableParent proves that changing the
// propagator configuration does not fail workflows already in flight: a
// stored W3C header under OTEL_PROPAGATORS=none cannot be extracted, and the
// workflow body must still execute.
func TestTracingInterceptorToleratesUnreadableParent(t *testing.T) {
	previousProvider := otel.GetTracerProvider()
	previousPropagator := otel.GetTextMapPropagator()
	t.Cleanup(func() {
		otel.SetTracerProvider(previousProvider)
		otel.SetTextMapPropagator(previousPropagator)
	})
	t.Setenv("OTEL_EXPORTER_OTLP_ENDPOINT", "http://127.0.0.1:1")
	t.Setenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", "")
	t.Setenv("OTEL_TRACES_SAMPLER", "always_off")
	t.Setenv("OTEL_PROPAGATORS", "none")
	shutdown, err := cotel.Bootstrap(context.Background(), true, "temporal-test")
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, shutdown(context.Background())) })

	opts, err := ClientOptions("127.0.0.1:1", "test", nil, nil)
	require.NoError(t, err)
	require.Len(t, opts.Interceptors, 1)
	workerTracing, ok := opts.Interceptors[0].(interceptor.WorkerInterceptor)
	require.True(t, ok, "the shared interceptor must also serve workers")

	payload, err := converter.GetDefaultDataConverter().ToPayload(map[string]string{
		"traceparent": "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
	})
	require.NoError(t, err)

	suite := &testsuite.WorkflowTestSuite{}
	env := suite.NewTestWorkflowEnvironment()
	env.SetHeader(&commonpb.Header{Fields: map[string]*commonpb.Payload{
		"_tracer-data": payload,
	}})
	// The test environment is not built from a client, so it needs the
	// worker interceptor supplied explicitly. Production workers inherit it.
	env.SetWorkerOptions(worker.Options{
		Interceptors: []interceptor.WorkerInterceptor{workerTracing},
	})
	env.ExecuteWorkflow(func(workflow.Context) (string, error) {
		return "body executed", nil
	})
	require.NoError(t, env.GetWorkflowError())
	var result string
	require.NoError(t, env.GetWorkflowResult(&result))
	assert.Equal(t, "body executed", result)
}
