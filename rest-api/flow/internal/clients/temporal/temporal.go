// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package temporal

import (
	"errors"
	"fmt"
	"os"
	"time"

	"go.opentelemetry.io/otel"
	"go.temporal.io/sdk/client"
	"go.temporal.io/sdk/contrib/opentelemetry"
	"go.temporal.io/sdk/converter"
	"go.temporal.io/sdk/interceptor"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/endpoint"
)

const (
	defaultKeepAliveTime    = 10 * time.Second
	defaultKeepAliveTimeout = 60 * time.Second
)

type Client struct {
	config  Config
	options client.Options
	client  client.Client
}

type Config struct {
	Endpoint   endpoint.Config
	EnableTLS  bool
	ServerName string
	Namespace  string
}

func (c *Config) Validate() error {
	if err := c.Endpoint.Validate(); err != nil {
		return err
	}

	if c.EnableTLS {
		if c.ServerName == "" {
			return errors.New("server name is required")
		}

		if c.Endpoint.CACertificatePath == "" {
			return errors.New("CA certificate path is required")
		}

		if _, err := os.Stat(c.Endpoint.CACertificatePath); os.IsNotExist(err) { //nolint
			return errors.New("CA certificate path does not exist")
		}
	}

	return nil
}

func New(c Config) (*Client, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}

	tlsConfig, err := buildTLSConfig(c)
	if err != nil {
		return nil, err
	}

	// Unconditional, matching the worker on the far end. The interceptor only
	// reads context and hands it to the global propagator; with no
	// TracerProvider the global tracer returns a non-recording span that still
	// carries the inbound SpanContext.
	tracingInterceptor, err := opentelemetry.NewTracingInterceptor(
		opentelemetry.TracerOptions{TextMapPropagator: otel.GetTextMapPropagator()})
	if err != nil {
		return nil, fmt.Errorf("creating Temporal tracing interceptor: %w", err)
	}

	options := client.Options{
		HostPort:  c.Endpoint.Target(),
		Namespace: c.Namespace,
		ConnectionOptions: client.ConnectionOptions{
			TLS:              tlsConfig,
			KeepAliveTime:    defaultKeepAliveTime,
			KeepAliveTimeout: defaultKeepAliveTimeout,
		},
		DataConverter: converter.NewCompositeDataConverter(
			converter.NewNilPayloadConverter(),
			converter.NewByteSlicePayloadConverter(),
			converter.NewProtoJSONPayloadConverterWithOptions(
				converter.ProtoJSONPayloadConverterOptions{
					AllowUnknownFields: true,
				},
			),
			converter.NewProtoPayloadConverter(),
			converter.NewJSONPayloadConverter(),
		),
		Interceptors: []interceptor.ClientInterceptor{tracingInterceptor},
	}

	client, err := client.Dial(options)
	if err != nil {
		return nil, err
	}

	return &Client{config: c, options: options, client: client}, nil
}

func (c *Client) Client() client.Client {
	return c.client
}
