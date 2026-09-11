// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package temporal provides Temporal client configuration shared by every
// binary in the repo, so all clients agree on the payload converter and all
// of them attach the OpenTelemetry tracing interceptor consistently.
package temporal

import (
	"crypto/tls"
	"fmt"

	"go.opentelemetry.io/otel"
	tsdkClient "go.temporal.io/sdk/client"
	"go.temporal.io/sdk/contrib/opentelemetry"
	tsdkConverter "go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/interceptor"
	tsdkLog "go.temporal.io/sdk/log"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"
)

// DataConverter returns the composite payload converter shared by every
// Temporal client in the repo. Producers and consumers must agree on it.
func DataConverter() tsdkConverter.DataConverter {
	return tsdkConverter.NewCompositeDataConverter(
		tsdkConverter.NewNilPayloadConverter(),
		tsdkConverter.NewByteSlicePayloadConverter(),
		tsdkConverter.NewProtoJSONPayloadConverterWithOptions(tsdkConverter.ProtoJSONPayloadConverterOptions{
			AllowUnknownFields: true,
		}),
		tsdkConverter.NewProtoPayloadConverter(),
		tsdkConverter.NewJSONPayloadConverter(),
	)
}

// ClientOptions assembles the Temporal client options shared by all services:
// host/port, TLS, the composite data converter, and — when tracing is
// active — the OpenTelemetry interceptor so workflow starts and worker
// executions join the trace. It deliberately accepts primitive connection
// data instead of importing application configuration packages.
func ClientOptions(hostPort, namespace string, tlsConfig *tls.Config, logger tsdkLog.Logger) (tsdkClient.Options, error) {
	opts := tsdkClient.Options{
		HostPort:  hostPort,
		Namespace: namespace,
		ConnectionOptions: tsdkClient.ConnectionOptions{
			TLS: tlsConfig,
		},
		DataConverter: DataConverter(),
		Logger:        logger,
	}

	return ConfigureClientOptions(opts)
}

// ConfigureClientOptions adds the shared converter when absent and appends the
// tracing interceptor after Bootstrap has enabled transport instrumentation.
// This preserves workflow trace context even when the process does not export
// spans locally.
func ConfigureClientOptions(opts tsdkClient.Options) (tsdkClient.Options, error) {
	if opts.DataConverter == nil {
		opts.DataConverter = DataConverter()
	}
	tracing, err := TracingInterceptor()
	if err != nil {
		return opts, err
	}
	if tracing != nil {
		opts.Interceptors = append(opts.Interceptors, tracing)
	}
	return opts, nil
}

// TracingInterceptor returns the OpenTelemetry Temporal interceptor once
// Bootstrap has enabled transport instrumentation, and nil otherwise. The
// value satisfies both the client and the worker interceptor interfaces, so a
// worker attaches the same instrumentation as the client it polls through.
func TracingInterceptor() (interceptor.Interceptor, error) {
	if !cotel.TransportEnabled() {
		return nil, nil
	}

	otelInterceptor, err := opentelemetry.NewTracingInterceptor(opentelemetry.TracerOptions{
		TextMapPropagator: otel.GetTextMapPropagator(),
		DisableBaggage:    true,
	})
	if err != nil {
		return nil, fmt.Errorf("failed to create Temporal tracing interceptor: %w", err)
	}
	return otelInterceptor, nil
}
