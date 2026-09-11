// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package otel provides a shared, env-driven OpenTelemetry tracing bootstrap
// for rest-api binaries. It installs a global SDK tracer provider exporting
// over OTLP, configured through standard OTEL_* environment variables plus
// bounded batch-processor settings, so the instrumentation libraries already
// wired throughout the codebase (otelecho, otelhttp, otelgrpc, bunotel,
// Temporal interceptors) emit real spans.
package otel

import (
	"context"
	"fmt"
	"os"
	"strconv"
	"strings"
	"sync/atomic"
	"time"

	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/propagators/autoprop"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

const (
	envServiceName      = "OTEL_SERVICE_NAME"
	envExporterEndpoint = "OTEL_EXPORTER_OTLP_ENDPOINT"
	envTracesEndpoint   = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"
	envExporterProtocol = "OTEL_EXPORTER_OTLP_PROTOCOL"
	envTracesProtocol   = "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL"
	envTracesSampler    = "OTEL_TRACES_SAMPLER"
	envPropagators      = "OTEL_PROPAGATORS"
	envBSPMaxQueueSize  = "OTEL_BSP_MAX_QUEUE_SIZE"
	envBSPMaxBatchSize  = "OTEL_BSP_MAX_EXPORT_BATCH_SIZE"
	envBSPScheduleDelay = "OTEL_BSP_SCHEDULE_DELAY"
	envBSPExportTimeout = "OTEL_BSP_EXPORT_TIMEOUT"

	protocolGRPC         = "grpc"
	protocolHTTPProtobuf = "http/protobuf"

	// These are count/time safety bounds, not a byte-level memory guarantee.
	// Operators still need to size span counts relative to workload pod limits.
	minBSPMaxQueueSize  = 1
	maxBSPMaxQueueSize  = 16384
	minBSPMaxBatchSize  = 1
	maxBSPMaxBatchSize  = 2048
	minBSPScheduleDelay = 100
	maxBSPScheduleDelay = 10000
	minBSPExportTimeout = 1000
	maxBSPExportTimeout = 60000
)

type batchSpanProcessorConfig struct {
	maxQueueSize       int
	maxExportBatchSize int
	batchTimeout       time.Duration
	exportTimeout      time.Duration
}

func (c batchSpanProcessorConfig) options() []sdktrace.BatchSpanProcessorOption {
	return []sdktrace.BatchSpanProcessorOption{
		sdktrace.WithMaxQueueSize(c.maxQueueSize),
		sdktrace.WithMaxExportBatchSize(c.maxExportBatchSize),
		sdktrace.WithBatchTimeout(c.batchTimeout),
		sdktrace.WithExportTimeout(c.exportTimeout),
	}
}

var (
	tracingEnabled   atomic.Bool
	transportEnabled atomic.Bool
)

// ExporterConfigured reports whether an OTLP trace exporter endpoint is
// configured via the standard environment variables. It is intended for
// binaries whose tracing enablement is entirely environment-driven.
func ExporterConfigured() bool {
	return os.Getenv(envExporterEndpoint) != "" || os.Getenv(envTracesEndpoint) != ""
}

// Enabled reports whether Bootstrap successfully installed a tracer provider.
// Recording-only instrumentation such as database hooks must use this gate.
// Transport instrumentation must use TransportEnabled so context can cross a
// service that does not export spans locally.
func Enabled() bool {
	return tracingEnabled.Load()
}

// TransportEnabled reports whether Bootstrap configured transport
// instrumentation. It remains true when local export is disabled so an
// intermediate service can still extract and forward incoming trace context.
func TransportEnabled() bool {
	return transportEnabled.Load()
}

// Bootstrap installs a global OTLP-exporting tracer provider and the
// OTEL_PROPAGATORS-configured propagator (W3C trace-context by default).
// Propagation remains active when enabled is false or no OTLP endpoint is
// configured, allowing non-exporting services to preserve distributed traces.
// serviceNameFallback is used only when the environment does not provide
// service.name.
//
// The returned shutdown function flushes pending spans and is always non-nil.
func Bootstrap(ctx context.Context, enabled bool, serviceNameFallback string) (func(context.Context) error, error) {
	tracingEnabled.Store(false)
	transportEnabled.Store(false)
	noopShutdown := func(context.Context) error {
		transportEnabled.Store(false)
		return nil
	}

	propagator, err := configuredPropagator()
	if err != nil {
		return noopShutdown, fmt.Errorf("failed to configure OTel propagators: %w", err)
	}

	if !enabled {
		otel.SetTextMapPropagator(propagator)
		transportEnabled.Store(len(propagator.Fields()) > 0)
		log.Info().Msg("tracing disabled by config")
		return noopShutdown, nil
	}

	if !ExporterConfigured() {
		otel.SetTextMapPropagator(propagator)
		transportEnabled.Store(len(propagator.Fields()) > 0)
		log.Info().Msg("tracing enabled but no OTLP exporter endpoint configured, tracer provider not installed")
		return noopShutdown, nil
	}
	batchConfig, err := batchSpanProcessorConfigFromEnv()
	if err != nil {
		return noopShutdown, err
	}

	res, err := newResource(ctx, serviceNameFallback)
	if err != nil {
		return noopShutdown, fmt.Errorf("failed to build OTel resource: %w", err)
	}

	exp, err := newExporter(ctx)
	if err != nil {
		return noopShutdown, err
	}

	// No sampler is set here. Sampling is a deployment decision, so it is left
	// to OTEL_TRACES_SAMPLER, which the SDK reads itself; absent that, the SDK
	// default ParentBased(AlwaysSample) records every root trace.
	tp := sdktrace.NewTracerProvider(
		sdktrace.WithBatcher(exp, batchConfig.options()...),
		sdktrace.WithResource(res),
	)
	otel.SetTracerProvider(tp)
	otel.SetTextMapPropagator(propagator)
	transportEnabled.Store(true)
	tracingEnabled.Store(true)

	log.Info().
		Str("serviceName", serviceName(res)).
		Str("protocol", exportProtocol()).
		Int("batchMaxQueueSize", batchConfig.maxQueueSize).
		Int("batchMaxExportSize", batchConfig.maxExportBatchSize).
		Dur("batchScheduleDelay", batchConfig.batchTimeout).
		Dur("batchExportTimeout", batchConfig.exportTimeout).
		Msg("tracing enabled, OTLP tracer provider installed")

	return func(shutdownCtx context.Context) error {
		tracingEnabled.Store(false)
		transportEnabled.Store(false)
		return tp.Shutdown(shutdownCtx)
	}, nil
}

func batchSpanProcessorConfigFromEnv() (batchSpanProcessorConfig, error) {
	maxQueueSize, err := boundedIntFromEnv(
		envBSPMaxQueueSize,
		sdktrace.DefaultMaxQueueSize,
		minBSPMaxQueueSize,
		maxBSPMaxQueueSize,
	)
	if err != nil {
		return batchSpanProcessorConfig{}, err
	}
	maxExportBatchSize, err := boundedIntFromEnv(
		envBSPMaxBatchSize,
		sdktrace.DefaultMaxExportBatchSize,
		minBSPMaxBatchSize,
		maxBSPMaxBatchSize,
	)
	if err != nil {
		return batchSpanProcessorConfig{}, err
	}
	if maxExportBatchSize > maxQueueSize {
		return batchSpanProcessorConfig{}, fmt.Errorf(
			"%s must be less than or equal to %s",
			envBSPMaxBatchSize,
			envBSPMaxQueueSize,
		)
	}
	scheduleDelayMillis, err := boundedIntFromEnv(
		envBSPScheduleDelay,
		sdktrace.DefaultScheduleDelay,
		minBSPScheduleDelay,
		maxBSPScheduleDelay,
	)
	if err != nil {
		return batchSpanProcessorConfig{}, err
	}
	exportTimeoutMillis, err := boundedIntFromEnv(
		envBSPExportTimeout,
		sdktrace.DefaultExportTimeout,
		minBSPExportTimeout,
		maxBSPExportTimeout,
	)
	if err != nil {
		return batchSpanProcessorConfig{}, err
	}

	return batchSpanProcessorConfig{
		maxQueueSize:       maxQueueSize,
		maxExportBatchSize: maxExportBatchSize,
		batchTimeout:       time.Duration(scheduleDelayMillis) * time.Millisecond,
		exportTimeout:      time.Duration(exportTimeoutMillis) * time.Millisecond,
	}, nil
}

func boundedIntFromEnv(name string, fallback, minimum, maximum int) (int, error) {
	value := strings.TrimSpace(os.Getenv(name))
	if value == "" {
		return fallback, nil
	}

	parsed, err := strconv.Atoi(value)
	if err != nil || parsed < minimum || parsed > maximum {
		return 0, fmt.Errorf(
			"%s must be an integer from %d through %d",
			name,
			minimum,
			maximum,
		)
	}
	return parsed, nil
}

// Propagator returns the installed global propagator, or the environment-
// configured propagator for clients constructed before Bootstrap. Invalid
// pre-bootstrap configuration is reported through the global OTel error
// handler and falls back to W3C trace-context propagation.
func Propagator() propagation.TextMapPropagator {
	if TransportEnabled() {
		return otel.GetTextMapPropagator()
	}
	propagator, err := configuredPropagator()
	if err != nil {
		otel.Handle(err)
		return defaultPropagator()
	}
	return propagator
}

func configuredPropagator() (propagation.TextMapPropagator, error) {
	configured := strings.TrimSpace(os.Getenv(envPropagators))
	if configured == "" {
		return defaultPropagator(), nil
	}

	names := strings.Split(strings.ToLower(configured), ",")
	for i := range names {
		names[i] = strings.TrimSpace(names[i])
	}
	return autoprop.TextMapPropagator(names...)
}

func defaultPropagator() propagation.TextMapPropagator {
	return propagation.TraceContext{}
}

// newResource merges environment-provided resource attributes over the local
// fallback without mutating process-wide environment variables.
func newResource(ctx context.Context, serviceNameFallback string) (*resource.Resource, error) {
	fallback := resource.Empty()
	if serviceNameFallback != "" {
		fallback = resource.NewSchemaless(attribute.String("service.name", serviceNameFallback))
	}

	detected, err := resource.New(ctx, resource.WithFromEnv(), resource.WithTelemetrySDK())
	if err != nil {
		return nil, err
	}
	return resource.Merge(fallback, detected)
}

func serviceName(res *resource.Resource) string {
	for _, attr := range res.Attributes() {
		if string(attr.Key) == "service.name" {
			return attr.Value.AsString()
		}
	}
	return ""
}

// exportProtocol resolves the OTLP transport protocol per the OTel spec
// precedence: traces-specific variable, then general, then http/protobuf.
func exportProtocol() string {
	if p := os.Getenv(envTracesProtocol); p != "" {
		return p
	}
	if p := os.Getenv(envExporterProtocol); p != "" {
		return p
	}
	return protocolHTTPProtobuf
}

// newExporter builds the OTLP trace exporter for the configured protocol.
// Endpoint, headers, and TLS/insecure settings are read from the standard
// OTEL_EXPORTER_OTLP_* environment variables by the exporter itself.
func newExporter(ctx context.Context) (*otlptrace.Exporter, error) {
	switch p := exportProtocol(); p {
	case protocolGRPC:
		exp, err := otlptracegrpc.New(ctx)
		if err != nil {
			return nil, fmt.Errorf("failed to create OTLP gRPC trace exporter: %w", err)
		}
		return exp, nil
	case protocolHTTPProtobuf:
		exp, err := otlptracehttp.New(ctx)
		if err != nil {
			return nil, fmt.Errorf("failed to create OTLP HTTP trace exporter: %w", err)
		}
		return exp, nil
	default:
		return nil, fmt.Errorf("unsupported OTLP exporter protocol %q", p)
	}
}
