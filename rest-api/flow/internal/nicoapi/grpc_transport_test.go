// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"net"
	"strings"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/grpc/test/bufconn"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"
)

const (
	testGRPCBufferSize          = 1 << 20
	testExpectedMachineCount    = 65
	testMachineDescriptionBytes = 64 * 1024
)

type expectedMachineForgeServer struct {
	corev1.UnimplementedForgeServer
	response *corev1.ExpectedMachineList
}

func (s *expectedMachineForgeServer) GetAllExpectedMachines(
	context.Context,
	*emptypb.Empty,
) (*corev1.ExpectedMachineList, error) {
	return s.response, nil
}

func TestCoreGRPCDialOptionsReceiveExpectedInventory(t *testing.T) {
	response := &corev1.ExpectedMachineList{
		ExpectedMachines: make([]*corev1.ExpectedMachine, 0, testExpectedMachineCount),
	}
	for range testExpectedMachineCount {
		response.ExpectedMachines = append(response.ExpectedMachines, &corev1.ExpectedMachine{
			Metadata: &corev1.Metadata{
				Description: strings.Repeat("a", testMachineDescriptionBytes),
			},
		})
	}

	responseSize := proto.Size(response)
	require.Greater(t, responseSize, 4*1024*1024)
	require.Less(t, responseSize, coreGRPCMaxRecvMsgSize)

	listener := bufconn.Listen(testGRPCBufferSize)
	server := grpc.NewServer()
	corev1.RegisterForgeServer(server, &expectedMachineForgeServer{response: response})
	go func() {
		_ = server.Serve(listener)
	}()
	t.Cleanup(func() {
		server.Stop()
		require.NoError(t, listener.Close())
	})

	dialer := grpc.WithContextDialer(func(context.Context, string) (net.Conn, error) {
		return listener.Dial()
	})
	tests := map[string]struct {
		dialOptions []grpc.DialOption
		wantCode    codes.Code
		wantCount   int
	}{
		"gRPC default rejects response over 4 MiB": {
			dialOptions: []grpc.DialOption{
				grpc.WithTransportCredentials(insecure.NewCredentials()),
				dialer,
			},
			wantCode: codes.ResourceExhausted,
		},
		"Flow accepts response under 32 MiB": {
			dialOptions: append(coreGRPCDialOptions(insecure.NewCredentials()), dialer),
			wantCode:    codes.OK,
			wantCount:   testExpectedMachineCount,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			connection, err := grpc.NewClient("passthrough:///core", test.dialOptions...)
			require.NoError(t, err)
			t.Cleanup(func() {
				require.NoError(t, connection.Close())
			})

			result, err := corev1.NewForgeClient(connection).GetAllExpectedMachines(t.Context(), &emptypb.Empty{})
			assert.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode != codes.OK {
				assert.Nil(t, result)
				return
			}
			require.NoError(t, err)
			assert.Len(t, result.GetExpectedMachines(), test.wantCount)
		})
	}
}
