// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"net"
	"testing"

	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/test/bufconn"

	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// flowRPCClient exercises the generated method registration on an
// in-memory connection; no test listener or credentials are shared with a site.
func flowRPCClient(t *testing.T, implementation *FlowServerImpl) pb.FlowClient {
	t.Helper()
	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	pb.RegisterFlowServer(server, implementation)
	finished := make(chan struct{})
	go func() {
		defer close(finished)
		_ = server.Serve(listener)
	}()
	connection, err := grpc.NewClient("passthrough:///flow",
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) { return listener.Dial() }),
	)
	require.NoError(t, err)
	t.Cleanup(func() {
		require.NoError(t, connection.Close())
		server.Stop()
		require.NoError(t, listener.Close())
		<-finished
	})
	return pb.NewFlowClient(connection)
}
