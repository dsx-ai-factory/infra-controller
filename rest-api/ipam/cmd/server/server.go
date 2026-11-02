/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package main

import (
	"fmt"
	"log/slog"
	"net/http"
	"time"

	"connectrpc.com/connect"
	"connectrpc.com/otelconnect"
	goipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/NVIDIA/infra-controller/rest-api/ipam/api/v1/apiv1connect"
	"github.com/NVIDIA/infra-controller/rest-api/ipam/pkg/service"
	compress "github.com/klauspost/connect-compress/v2"
	"github.com/metal-stack/v"
	"github.com/prometheus/client_golang/prometheus/promhttp"
	"go.opentelemetry.io/otel/exporters/prometheus"
	"go.opentelemetry.io/otel/sdk/metric"

	"connectrpc.com/grpchealth"
	"connectrpc.com/grpcreflect"
)

type config struct {
	GrpcServerEndpoint string
	MetricsEndpoint    string
	Log                *slog.Logger
	Storage            goipam.Storage
	TLSCertFile        string
	TLSKeyFile         string
}
type server struct {
	c       config
	ipamer  goipam.Ipamer
	storage goipam.Storage
	log     *slog.Logger
}

func newServer(c config) *server {
	return &server{
		c:       c,
		ipamer:  goipam.NewWithStorage(c.Storage),
		storage: c.Storage,
		log:     c.Log,
	}
}
func (s *server) Run() error {
	s.log.Info("starting go-ipam", "version", v.V, "backend", s.storage.Name())

	// The exporter embeds a default OpenTelemetry Reader and
	// implements prometheus.Collector, allowing it to be used as
	// both a Reader and Collector.
	exporter, err := prometheus.New()
	if err != nil {
		return err
	}
	provider := metric.NewMeterProvider(metric.WithReader(exporter))

	// Start the prometheus HTTP server and pass the exporter Collector to it
	go func() {
		s.log.Info("serving metrics", "at", fmt.Sprintf("%s/metrics", s.c.MetricsEndpoint))
		metricsServer := http.NewServeMux()
		metricsServer.Handle("GET /metrics", promhttp.Handler())
		ms := &http.Server{
			Addr:              s.c.MetricsEndpoint,
			Handler:           metricsServer,
			ReadHeaderTimeout: time.Minute,
		}
		err := ms.ListenAndServe()
		if err != nil {
			s.log.Error("unable to start metric endpoint", "error", err)
			return
		}
	}()

	interceptors := []connect.Interceptor{}

	otelInterceptor, err := otelconnect.NewInterceptor(otelconnect.WithMeterProvider(provider))
	if err == nil {
		interceptors = append(interceptors, otelInterceptor)
	} else {
		s.log.Error("unable to create otel interceptor", "error", err)
	}

	mux := http.NewServeMux()
	// The generated constructors return a path and a plain net/http
	// handler.
	mux.Handle(
		apiv1connect.NewIpamServiceHandler(
			service.New(s.log, s.ipamer),
			connect.WithInterceptors(interceptors...),
		),
	)

	mux.Handle(grpchealth.NewHandler(
		grpchealth.NewStaticChecker(apiv1connect.IpamServiceName),
		compress.WithAll(compress.LevelBalanced),
	))
	mux.Handle(grpcreflect.NewHandlerV1(
		grpcreflect.NewStaticReflector(apiv1connect.IpamServiceName),
		compress.WithAll(compress.LevelBalanced),
	))

	switch {
	case s.c.TLSCertFile == "" && s.c.TLSKeyFile == "":
		return fmt.Errorf("TLS certificates are required; set both server-tls-cert and server-tls-key")
	case s.c.TLSCertFile == "":
		return fmt.Errorf("partial TLS config: missing server-tls-cert")
	case s.c.TLSKeyFile == "":
		return fmt.Errorf("partial TLS config: missing server-tls-key")
	}

	srv := http.Server{
		Addr:              s.c.GrpcServerEndpoint,
		Handler:           mux,
		ReadHeaderTimeout: 1 * time.Minute,
	}
	s.log.Info("serving gRPC with TLS", "addr", s.c.GrpcServerEndpoint)
	return srv.ListenAndServeTLS(s.c.TLSCertFile, s.c.TLSKeyFile)
}
