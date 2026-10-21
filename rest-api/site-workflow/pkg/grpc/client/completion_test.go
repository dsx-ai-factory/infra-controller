// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"io"
	"net"
	"testing"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	healthpb "google.golang.org/grpc/health/grpc_health_v1"
	"google.golang.org/grpc/status"
)

func TestNewCoreGrpcClient(t *testing.T) {
	testRPCCompletion(t, func(address string, callback func(error)) (*grpc.ClientConn, error) {
		c, err := NewCoreGrpcClient(&CoreGrpcClientConfig{Address: address, OnRPCFinish: callback})
		if err != nil {
			return nil, err
		}
		return c.conn, nil
	})
}

func TestNewFlowGrpcClient(t *testing.T) {
	testRPCCompletion(t, func(address string, callback func(error)) (*grpc.ClientConn, error) {
		c, err := NewFlowGrpcClient(&FlowGrpcClientConfig{Address: address, OnRPCFinish: callback})
		if err != nil {
			return nil, err
		}
		return c.conn, nil
	})
}

func testRPCCompletion(t *testing.T, connect func(string, func(error)) (*grpc.ClientConn, error)) {
	t.Helper()
	for _, tc := range []struct {
		name   string
		code   codes.Code
		cancel bool
	}{
		{"normal EOF", codes.OK, false},
		{"terminal failure", codes.Unavailable, false},
		{"cancellation", codes.Canceled, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			listener, err := net.Listen("tcp", "127.0.0.1:0")
			require.NoError(t, err)
			end := make(chan error, 1)
			server := grpc.NewServer()
			corev1.RegisterForgeServer(server, &completionCoreServer{})
			flowv1.RegisterFlowServer(server, &completionFlowServer{})
			healthpb.RegisterHealthServer(server, &completionHealthServer{end: end})
			go func() { _ = server.Serve(listener) }()
			t.Cleanup(server.Stop)
			completions := make(chan error, 8)
			conn, err := connect(listener.Addr().String(), func(err error) { completions <- err })
			require.NoError(t, err)
			t.Cleanup(func() { assert.NoError(t, conn.Close()) })
			// The constructor's real Version probe is itself one completed RPC.
			select {
			case err := <-completions:
				require.NoError(t, err)
			case <-time.After(5 * time.Second):
				t.Fatal("Version completion callback missing")
			}
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			stream, err := healthpb.NewHealthClient(conn).Watch(ctx, &healthpb.HealthCheckRequest{})
			require.NoError(t, err)
			_, err = stream.Recv()
			require.NoError(t, err)
			require.Empty(t, completions, "opening a stream must not count as a completed RPC")
			if tc.cancel {
				cancel()
			} else {
				end <- status.Error(tc.code, "terminal error")
			}
			_, err = stream.Recv()
			if tc.code == codes.OK {
				require.ErrorIs(t, err, io.EOF)
			} else {
				require.Equal(t, tc.code, status.Code(err))
			}
			select {
			case finalErr := <-completions:
				assert.Equal(t, tc.code, status.Code(finalErr))
			case <-time.After(5 * time.Second):
				t.Fatal("RPC completion callback missing")
			}
			_, _ = stream.Recv()
			require.NoError(t, stream.CloseSend())
			assert.Empty(t, completions, "terminal reads must not count the RPC twice")
		})
	}
}

type completionCoreServer struct {
	corev1.UnimplementedForgeServer
}

func (*completionCoreServer) Version(context.Context, *corev1.VersionRequest) (*corev1.BuildInfo, error) {
	return &corev1.BuildInfo{}, nil
}

type completionFlowServer struct{ flowv1.UnimplementedFlowServer }

func (*completionFlowServer) Version(context.Context, *flowv1.VersionRequest) (*flowv1.BuildInfo, error) {
	return &flowv1.BuildInfo{}, nil
}

type completionHealthServer struct {
	healthpb.UnimplementedHealthServer
	end <-chan error
}

func (s *completionHealthServer) Watch(_ *healthpb.HealthCheckRequest, stream healthpb.Health_WatchServer) error {
	err := stream.Send(&healthpb.HealthCheckResponse{Status: healthpb.HealthCheckResponse_SERVING})
	if err != nil {
		return err
	}
	select {
	case err := <-s.end:
		return err
	case <-stream.Context().Done():
		return stream.Context().Err()
	}
}
