// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package otel

import (
	"context"
	"os"
	"sync"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/trace"
)

// clearTracingEnv resets all env vars read by the bootstrap so tests are
// hermetic regardless of the caller's environment.
func clearTracingEnv(t *testing.T) {
	t.Helper()
	for _, key := range []string{
		envServiceName,
		envExporterEndpoint,
		envTracesEndpoint,
		envExporterProtocol,
		envTracesProtocol,
		envTracesSampler,
		envPropagators,
		envBSPMaxQueueSize,
		envBSPMaxBatchSize,
		envBSPScheduleDelay,
		envBSPExportTimeout,
		"OTEL_RESOURCE_ATTRIBUTES",
	} {
		t.Setenv(key, "")
	}
	tracingEnabled.Store(false)
}

func TestBatchSpanProcessorConfigFromEnv(t *testing.T) {
	tests := []struct {
		descr   string
		values  map[string]string
		want    batchSpanProcessorConfig
		wantErr string
	}{
		{
			descr: "explicit SDK defaults",
			want: batchSpanProcessorConfig{
				maxQueueSize:       sdktrace.DefaultMaxQueueSize,
				maxExportBatchSize: sdktrace.DefaultMaxExportBatchSize,
				batchTimeout:       time.Duration(sdktrace.DefaultScheduleDelay) * time.Millisecond,
				exportTimeout:      time.Duration(sdktrace.DefaultExportTimeout) * time.Millisecond,
			},
		},
		{
			descr: "bounded custom settings",
			values: map[string]string{
				envBSPMaxQueueSize:  "1024",
				envBSPMaxBatchSize:  "256",
				envBSPScheduleDelay: "1000",
				envBSPExportTimeout: "15000",
			},
			want: batchSpanProcessorConfig{
				maxQueueSize:       1024,
				maxExportBatchSize: 256,
				batchTimeout:       time.Second,
				exportTimeout:      15 * time.Second,
			},
		},
		{
			descr: "minimum settings",
			values: map[string]string{
				envBSPMaxQueueSize:  "1",
				envBSPMaxBatchSize:  "1",
				envBSPScheduleDelay: "100",
				envBSPExportTimeout: "1000",
			},
			want: batchSpanProcessorConfig{
				maxQueueSize:       1,
				maxExportBatchSize: 1,
				batchTimeout:       100 * time.Millisecond,
				exportTimeout:      time.Second,
			},
		},
		{
			descr: "maximum settings",
			values: map[string]string{
				envBSPMaxQueueSize:  "16384",
				envBSPMaxBatchSize:  "2048",
				envBSPScheduleDelay: "10000",
				envBSPExportTimeout: "60000",
			},
			want: batchSpanProcessorConfig{
				maxQueueSize:       16384,
				maxExportBatchSize: 2048,
				batchTimeout:       10 * time.Second,
				exportTimeout:      time.Minute,
			},
		},
		{descr: "queue must be numeric", values: map[string]string{envBSPMaxQueueSize: "many"}, wantErr: envBSPMaxQueueSize},
		{descr: "queue must be positive", values: map[string]string{envBSPMaxQueueSize: "0"}, wantErr: envBSPMaxQueueSize},
		{descr: "queue has an upper bound", values: map[string]string{envBSPMaxQueueSize: "16385"}, wantErr: envBSPMaxQueueSize},
		{descr: "batch must be positive", values: map[string]string{envBSPMaxBatchSize: "0"}, wantErr: envBSPMaxBatchSize},
		{descr: "batch has an upper bound", values: map[string]string{envBSPMaxBatchSize: "2049"}, wantErr: envBSPMaxBatchSize},
		{
			descr: "batch cannot exceed queue",
			values: map[string]string{
				envBSPMaxQueueSize: "128",
				envBSPMaxBatchSize: "256",
			},
			wantErr: envBSPMaxBatchSize,
		},
		{descr: "delay has a lower bound", values: map[string]string{envBSPScheduleDelay: "99"}, wantErr: envBSPScheduleDelay},
		{descr: "delay has an upper bound", values: map[string]string{envBSPScheduleDelay: "10001"}, wantErr: envBSPScheduleDelay},
		{descr: "timeout has a lower bound", values: map[string]string{envBSPExportTimeout: "999"}, wantErr: envBSPExportTimeout},
		{descr: "timeout has an upper bound", values: map[string]string{envBSPExportTimeout: "60001"}, wantErr: envBSPExportTimeout},
	}

	for _, tc := range tests {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			for key, value := range tc.values {
				t.Setenv(key, value)
			}

			got, err := batchSpanProcessorConfigFromEnv()

			if tc.wantErr != "" {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.wantErr)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tc.want, got)
		})
	}
}

type blockingSpanExporter struct {
	started chan struct{}
	release chan struct{}
	once    sync.Once
}

func (e *blockingSpanExporter) ExportSpans(ctx context.Context, _ []sdktrace.ReadOnlySpan) error {
	e.once.Do(func() { close(e.started) })
	select {
	case <-e.release:
		return nil
	case <-ctx.Done():
		return ctx.Err()
	}
}

func (*blockingSpanExporter) Shutdown(context.Context) error { return nil }

func TestBatchSpanProcessorDropsInsteadOfBlockingWhenQueueIsFull(t *testing.T) {
	exporter := &blockingSpanExporter{
		started: make(chan struct{}),
		release: make(chan struct{}),
	}
	config := batchSpanProcessorConfig{
		maxQueueSize:       1,
		maxExportBatchSize: 1,
		batchTimeout:       time.Hour,
		exportTimeout:      time.Minute,
	}
	processor := sdktrace.NewBatchSpanProcessor(exporter, config.options()...)
	provider := sdktrace.NewTracerProvider(
		sdktrace.WithSpanProcessor(processor),
		sdktrace.WithSampler(sdktrace.AlwaysSample()),
	)
	tracer := provider.Tracer("non-blocking-test")

	_, first := tracer.Start(context.Background(), "first")
	first.End()
	select {
	case <-exporter.started:
	case <-time.After(time.Second):
		t.Fatal("first export did not start")
	}

	_, queued := tracer.Start(context.Background(), "queued")
	queued.End()
	dropped := make(chan struct{})
	go func() {
		_, span := tracer.Start(context.Background(), "dropped")
		span.End()
		close(dropped)
	}()
	select {
	case <-dropped:
	case <-time.After(time.Second):
		t.Fatal("ending a span blocked on a full export queue")
	}

	close(exporter.release)
	shutdownCtx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	require.NoError(t, provider.Shutdown(shutdownCtx))
}

func TestConfiguredPropagator(t *testing.T) {
	tcs := []struct {
		descr         string
		value         string
		fields        []string
		injectedField string
		wantErr       bool
	}{
		{
			descr:  "default W3C trace-context propagator",
			fields: []string{"traceparent", "tracestate"},
		},
		{
			descr:         "B3 single-header propagator",
			value:         " b3 ",
			fields:        []string{"x-b3-traceid", "x-b3-spanid", "x-b3-sampled", "x-b3-flags"},
			injectedField: "b3",
		},
		{
			descr:  "propagation disabled",
			value:  "none",
			fields: []string{},
		},
		{
			descr:   "invalid propagator",
			value:   "not-a-propagator",
			wantErr: true,
		},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			t.Setenv(envPropagators, tc.value)

			propagator, err := configuredPropagator()
			if tc.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.ElementsMatch(t, tc.fields, propagator.Fields())
			if tc.injectedField != "" {
				spanContext := trace.NewSpanContext(trace.SpanContextConfig{
					TraceID:    trace.TraceID{1},
					SpanID:     trace.SpanID{1},
					TraceFlags: trace.FlagsSampled,
				})
				carrier := propagation.MapCarrier{}
				propagator.Inject(trace.ContextWithSpanContext(context.Background(), spanContext), carrier)
				assert.NotEmpty(t, carrier.Get(tc.injectedField))
			}
		})
	}
}

// restoreGlobals snapshots the global tracer provider and propagator and
// restores them after the test, since Bootstrap mutates OTel globals.
func restoreGlobals(t *testing.T) {
	t.Helper()
	prevTP := otel.GetTracerProvider()
	prevProp := otel.GetTextMapPropagator()
	t.Cleanup(func() {
		otel.SetTracerProvider(prevTP)
		otel.SetTextMapPropagator(prevProp)
	})
}

func TestExporterConfigured(t *testing.T) {
	tcs := []struct {
		descr          string
		endpoint       string
		tracesEndpoint string
		want           bool
	}{
		{descr: "no endpoints", want: false},
		{descr: "general endpoint", endpoint: "http://collector:4318", want: true},
		{descr: "traces endpoint", tracesEndpoint: "http://collector:4318/v1/traces", want: true},
		{descr: "both endpoints", endpoint: "http://collector:4318", tracesEndpoint: "http://collector:4318/v1/traces", want: true},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			t.Setenv(envExporterEndpoint, tc.endpoint)
			t.Setenv(envTracesEndpoint, tc.tracesEndpoint)

			assert.Equal(t, tc.want, ExporterConfigured())
		})
	}
}

func TestExportProtocol(t *testing.T) {
	tcs := []struct {
		descr          string
		protocol       string
		tracesProtocol string
		want           string
	}{
		{descr: "defaults to http/protobuf", want: protocolHTTPProtobuf},
		{descr: "general protocol", protocol: protocolGRPC, want: protocolGRPC},
		{descr: "traces protocol overrides general", protocol: protocolGRPC, tracesProtocol: protocolHTTPProtobuf, want: protocolHTTPProtobuf},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			t.Setenv(envExporterProtocol, tc.protocol)
			t.Setenv(envTracesProtocol, tc.tracesProtocol)

			assert.Equal(t, tc.want, exportProtocol())
		})
	}
}

func TestNewExporter(t *testing.T) {
	tcs := []struct {
		descr    string
		protocol string
		wantErr  bool
	}{
		{descr: "grpc", protocol: protocolGRPC},
		{descr: "http/protobuf", protocol: protocolHTTPProtobuf},
		{descr: "unsupported protocol", protocol: "http/json", wantErr: true},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			t.Setenv(envExporterProtocol, tc.protocol)

			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()

			exp, err := newExporter(ctx)
			if tc.wantErr {
				assert.Error(t, err)
				assert.Nil(t, exp)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, exp)
			assert.NoError(t, exp.Shutdown(ctx))
		})
	}
}

func TestBootstrapNoop(t *testing.T) {
	tcs := []struct {
		descr    string
		enabled  bool
		endpoint string
	}{
		{descr: "disabled by config", enabled: false, endpoint: "http://collector:4318"},
		{descr: "enabled but no endpoint configured", enabled: true},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			restoreGlobals(t)
			t.Setenv(envExporterEndpoint, tc.endpoint)

			prevTP := otel.GetTracerProvider()

			shutdown, err := Bootstrap(context.Background(), tc.enabled, "fallback-service")
			require.NoError(t, err)
			require.NotNil(t, shutdown)

			// The global tracer provider must be untouched
			assert.Same(t, prevTP, otel.GetTracerProvider())
			assert.False(t, Enabled())
			assert.True(t, TransportEnabled())
			assert.ElementsMatch(t, []string{"traceparent", "tracestate"}, otel.GetTextMapPropagator().Fields())

			assert.NoError(t, shutdown(context.Background()))
			assert.False(t, TransportEnabled())
		})
	}
}

func TestBootstrapServiceName(t *testing.T) {
	tcs := []struct {
		descr           string
		envServiceName  string
		fallback        string
		wantServiceName string
	}{
		{descr: "fallback used when OTEL_SERVICE_NAME unset", fallback: "nico-rest-api", wantServiceName: "nico-rest-api"},
		{descr: "OTEL_SERVICE_NAME overrides fallback", envServiceName: "nico-rest-cloud-worker", fallback: "nico-rest-workflow", wantServiceName: "nico-rest-cloud-worker"},
	}

	for _, tc := range tcs {
		t.Run(tc.descr, func(t *testing.T) {
			clearTracingEnv(t)
			restoreGlobals(t)
			t.Setenv(envServiceName, tc.envServiceName)
			res, err := newResource(context.Background(), tc.fallback)
			require.NoError(t, err)

			assert.Equal(t, tc.wantServiceName, serviceName(res))
			assert.Equal(t, tc.envServiceName, os.Getenv(envServiceName), "bootstrap must not mutate process environment")
		})
	}
}

func TestBootstrapInstallsGlobals(t *testing.T) {
	clearTracingEnv(t)
	restoreGlobals(t)
	t.Setenv(envExporterEndpoint, "http://localhost:14318")

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	shutdown, err := Bootstrap(ctx, true, "test-service")
	require.NoError(t, err)

	_, ok := otel.GetTracerProvider().(*sdktrace.TracerProvider)
	assert.True(t, ok, "expected an SDK tracer provider to be installed")
	assert.True(t, Enabled())
	assert.True(t, TransportEnabled())

	// Baggage is opt-in so untrusted baggage is not forwarded by default.
	fields := otel.GetTextMapPropagator().Fields()
	assert.Contains(t, fields, "traceparent")
	assert.NotContains(t, fields, "baggage")

	// Transports created after Bootstrap keep using the installed instance,
	// even if process environment is mutated later.
	t.Setenv(envPropagators, "b3")
	assert.ElementsMatch(t, fields, Propagator().Fields())

	require.NoError(t, shutdown(ctx))
	assert.False(t, Enabled())
	assert.False(t, TransportEnabled())
}

func TestBootstrapRejectsInvalidPropagator(t *testing.T) {
	clearTracingEnv(t)
	restoreGlobals(t)
	t.Setenv(envExporterEndpoint, "http://localhost:14318")
	t.Setenv(envPropagators, "not-a-propagator")
	previousProvider := otel.GetTracerProvider()

	shutdown, err := Bootstrap(context.Background(), true, "test-service")

	require.Error(t, err)
	require.NotNil(t, shutdown)
	assert.NoError(t, shutdown(context.Background()))
	assert.Same(t, previousProvider, otel.GetTracerProvider())
	assert.False(t, Enabled())
	assert.False(t, TransportEnabled())
}

func TestBootstrapRejectsInvalidBatchSpanProcessorConfig(t *testing.T) {
	clearTracingEnv(t)
	restoreGlobals(t)
	t.Setenv(envExporterEndpoint, "http://localhost:14318")
	t.Setenv(envBSPMaxQueueSize, "128")
	t.Setenv(envBSPMaxBatchSize, "256")
	previousProvider := otel.GetTracerProvider()

	shutdown, err := Bootstrap(context.Background(), true, "test-service")

	require.Error(t, err)
	require.NotNil(t, shutdown)
	assert.NoError(t, shutdown(context.Background()))
	assert.Same(t, previousProvider, otel.GetTracerProvider())
	assert.False(t, Enabled())
}
