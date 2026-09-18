// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tracing

import (
	"context"
	"fmt"
	"os"

	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
)

// InstallTracerProvider installs an OTLP exporter, or a no-op when
// OTEL_EXPORTER_OTLP_ENDPOINT is unset. Returns its shutdown func.
func InstallTracerProvider(ctx context.Context, serviceName string) (func(context.Context) error, error) {
	noop := func(context.Context) error { return nil }
	if os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT") == "" &&
		os.Getenv("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") == "" {
		return noop, nil
	}

	exp, err := otlptracegrpc.New(ctx)
	if err != nil {
		return noop, fmt.Errorf("creating OTLP trace exporter: %w", err)
	}

	res, err := resource.Merge(resource.Default(),
		resource.NewWithAttributes(resource.Default().SchemaURL(),
			attribute.String("service.name", serviceName)))
	if err != nil {
		return noop, fmt.Errorf("building trace resource for %s: %w", serviceName, err)
	}

	tp := sdktrace.NewTracerProvider(sdktrace.WithBatcher(exp), sdktrace.WithResource(res))
	otel.SetTracerProvider(tp)
	return tp.Shutdown, nil
}

// InstallExporter is InstallTracerProvider for a main(): one line, errors logged.
func InstallExporter(serviceName string) func() {
	shutdown, err := InstallTracerProvider(context.Background(), serviceName)
	if err != nil {
		log.Warn().Err(err).Str("service", serviceName).
			Msg("tracing: no TracerProvider installed; spans will not be exported")
		return func() {}
	}
	return func() { _ = shutdown(context.Background()) }
}
