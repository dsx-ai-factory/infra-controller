// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"os"
	"time"

	cotel "github.com/NVIDIA/infra-controller/rest-api/common/pkg/otel"

	"github.com/grpc-ecosystem/go-grpc-middleware/v2/interceptors/logging"
	log "github.com/sirupsen/logrus"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/certs"
	pb "github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/internal/proto/v1"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/powershelfmanager"

	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/reflection"
	"google.golang.org/grpc/status"
)

// Service owns the gRPC server lifecycle and a PowershelfManager orchestrator.
type Service struct {
	conf       Config
	grpcServer *grpc.Server
	psm        *powershelfmanager.PowershelfManager
}

// New initializes a PowershelfManager and constructs a Service from the Config.
func New(ctx context.Context, c Config) (*Service, error) {
	psmConfig, err := c.ToPsmConf()
	if err != nil {
		return nil, err
	}

	psm, err := powershelfmanager.New(ctx, *psmConfig)
	if err != nil {
		return nil, err
	}

	return &Service{
		conf: c,
		psm:  psm,
	}, nil
}

// Start begins the PowershelfManager, binds the gRPC server on the configured port, and serves until the listener is closed.
func (s *Service) Start(ctx context.Context) error {
	certOpt := s.certOption()

	err := s.psm.Start(ctx)
	if err != nil {
		return err
	}

	lis, err := net.Listen("tcp", fmt.Sprintf(":%v", s.conf.Port))
	if err != nil {
		return err
	}

	serverImpl, err := newServerImplementation(s.psm)
	if err != nil {
		return err
	}

	grpcServerOptions := []grpc.ServerOption{
		certOpt,
		grpc.ChainUnaryInterceptor(
			loggingUnaryInterceptor(),
		),
	}
	if cotel.TransportEnabled() {
		// Create server spans and continue traces from instrumented clients
		grpcServerOptions = append(grpcServerOptions, grpc.StatsHandler(otelgrpc.NewServerHandler()))
	}
	s.grpcServer = grpc.NewServer(grpcServerOptions...)

	log.Info("gRPC server starting with request/response logging enabled")

	// Block the main runtime loop for accepting and processing gRPC requests.
	pb.RegisterPowershelfManagerServer(s.grpcServer, serverImpl)
	reflection.Register(s.grpcServer)

	if err := s.grpcServer.Serve(lis); err != nil {
		return err
	}

	return nil
}

// Stop gracefully shuts down the gRPC server and stops the PowershelfManager.
func (s *Service) Stop(ctx context.Context) {
	log.Printf("Starting graceful shutdown now...")

	s.grpcServer.GracefulStop()
	s.psm.Stop(ctx)
}

// certOption returns the gRPC server option for TLS/mTLS if certificates are
// present. The service refuses to start without certificates unless
// ALLOW_INSECURE_GRPC=true is set.
func (s *Service) certOption() grpc.ServerOption {
	tlsConfig, certDir, err := certs.TLSConfig()
	if err != nil {
		if err == certs.ErrNotPresent {
			if os.Getenv("ALLOW_INSECURE_GRPC") == "true" {
				log.Warnf("TLS certs not present, running without mTLS (ALLOW_INSECURE_GRPC=true)")
				return grpc.EmptyServerOption{}
			}
			log.Fatalf("TLS certificates required but not found; set ALLOW_INSECURE_GRPC=true for local development")
		}
		log.Fatalf("Failed to load TLS certificates: %v", err)
	}
	log.Printf("Using certificates from %s (mTLS enabled)", certDir)
	return grpc.Creds(credentials.NewTLS(tlsConfig))
}

// logrusInterceptorLogger adapts logrus to the go-grpc-middleware logging.Logger interface.
func logrusInterceptorLogger() logging.Logger {
	return logging.LoggerFunc(func(ctx context.Context, lvl logging.Level, msg string, fields ...any) {
		entry := log.WithContext(ctx)

		// Convert fields slice to logrus.Fields
		if len(fields) > 0 {
			logrusFields := make(log.Fields)
			for i := 0; i+1 < len(fields); i += 2 {
				if key, ok := fields[i].(string); ok {
					logrusFields[key] = fields[i+1]
				}
			}
			entry = entry.WithFields(logrusFields)
		}

		switch lvl {
		case logging.LevelDebug:
			entry.Debug(msg)
		case logging.LevelInfo:
			entry.Info(msg)
		case logging.LevelWarn:
			entry.Warn(msg)
		case logging.LevelError:
			entry.Error(msg)
		default:
			entry.Info(msg)
		}
	})
}

// loggingUnaryInterceptor returns a gRPC unary interceptor that logs request and response payloads.
func loggingUnaryInterceptor() grpc.UnaryServerInterceptor {
	jsonOpts := protojson.MarshalOptions{
		EmitUnpopulated: false,
		Indent:          "",
	}

	return func(ctx context.Context, req any, info *grpc.UnaryServerInfo, handler grpc.UnaryHandler) (any, error) {
		start := time.Now()

		// Log request
		reqLog := log.WithField("grpc.method", info.FullMethod)

		if msg, ok := req.(proto.Message); ok {
			if jsonBytes, err := jsonOpts.Marshal(msg); err == nil {
				reqLog = reqLog.WithField("grpc.request", json.RawMessage(jsonBytes))
			}
		}
		reqLog.Info("gRPC request")

		// Call handler
		resp, err := handler(ctx, req)

		// Log response
		duration := time.Since(start)
		respLog := log.WithFields(log.Fields{
			"grpc.method":   info.FullMethod,
			"grpc.duration": duration.String(),
		})

		if err != nil {
			st, _ := status.FromError(err)
			respLog = respLog.WithFields(log.Fields{
				"grpc.code":  st.Code().String(),
				"grpc.error": st.Message(),
			})
			respLog.Error("gRPC response")
		} else {
			respLog = respLog.WithField("grpc.code", "OK")
			if msg, ok := resp.(proto.Message); ok {
				if jsonBytes, err := jsonOpts.Marshal(msg); err == nil {
					respLog = respLog.WithField("grpc.response", json.RawMessage(jsonBytes))
				}
			}
			respLog.Info("gRPC response")
		}

		return resp, err
	}
}
