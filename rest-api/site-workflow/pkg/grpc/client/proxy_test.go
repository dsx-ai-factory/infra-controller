// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
)

// fakeProxyConn implements grpc.ClientConnInterface for transcoder tests. It
// records the invoked method and populates the first scalar string field of the
// reply so the response-encoding path runs without a real server.
type fakeProxyConn struct {
	lastMethod string
	setValue   string
	replyBytes []byte
}

func (f *fakeProxyConn) Invoke(_ context.Context, method string, _, reply any, _ ...grpc.CallOption) error {
	f.lastMethod = method
	msg, ok := reply.(proto.Message)
	if !ok {
		return errors.New("reply is not a proto.Message")
	}
	if f.replyBytes != nil {
		return proto.Unmarshal(f.replyBytes, msg)
	}
	pm := msg.ProtoReflect()
	fields := pm.Descriptor().Fields()
	for i := range fields.Len() {
		fd := fields.Get(i)
		if fd.Kind() == protoreflect.StringKind && fd.Cardinality() != protoreflect.Repeated {
			pm.Set(fd, protoreflect.ValueOfString(f.setValue))
			break
		}
	}
	return nil
}

func (f *fakeProxyConn) NewStream(context.Context, *grpc.StreamDesc, string, ...grpc.CallOption) (grpc.ClientStream, error) {
	return nil, errors.New("streaming not supported")
}

// proxyBackendCases covers both proxied services with one method that exists on
// both and one that is specific to the backend, so a backend wired to the wrong
// descriptor set fails here rather than at runtime on site.
var proxyBackendCases = []struct {
	name          string
	backend       grpcproxy.Backend
	qualified     string
	qualifiedName string
}{
	{
		name:          "core",
		backend:       grpcproxy.Core,
		qualified:     "/forge.Forge/CreateCredential",
		qualifiedName: "CreateCredential",
	},
	{
		name:          "flow",
		backend:       grpcproxy.Flow,
		qualified:     "/v1.Flow/CreateOperationRun",
		qualifiedName: "CreateOperationRun",
	},
}

func TestResolveProxyMethod(t *testing.T) {
	for _, tc := range proxyBackendCases {
		t.Run(tc.name+" resolves a bare method", func(t *testing.T) {
			md, err := resolveProxyMethod(tc.backend, "Version")
			require.NoError(t, err)
			assert.Equal(t, "Version", string(md.Name()))
		})

		t.Run(tc.name+" resolves a fully qualified method", func(t *testing.T) {
			md, err := resolveProxyMethod(tc.backend, tc.qualified)
			require.NoError(t, err)
			assert.Equal(t, tc.qualifiedName, string(md.Name()))
		})

		t.Run(tc.name+" rejects an unknown method", func(t *testing.T) {
			_, err := resolveProxyMethod(tc.backend, "DefinitelyNotARealMethod")
			require.Error(t, err)
			assert.ErrorIs(t, err, ErrUnknownProxyMethod)
		})
	}
}

func TestInvokeProxyJSONConn(t *testing.T) {
	for _, tc := range proxyBackendCases {
		t.Run(tc.name+" round trips request and response", func(t *testing.T) {
			conn := &fakeProxyConn{setValue: "proxy-test-value"}
			respJSON, err := invokeProxyJSONConn(context.Background(), conn, tc.backend, "Version", nil)
			require.NoError(t, err)
			assert.Equal(t, "/"+tc.backend.ServiceName+"/Version", conn.lastMethod)
			assert.Contains(t, string(respJSON), "proxy-test-value")
		})

		t.Run(tc.name+" rejects unknown method before dialing", func(t *testing.T) {
			conn := &fakeProxyConn{}
			_, err := invokeProxyJSONConn(context.Background(), conn, tc.backend, "NopeNotReal", nil)
			require.Error(t, err)
			assert.ErrorIs(t, err, ErrUnknownProxyMethod)
			assert.Empty(t, conn.lastMethod, "transport must not be invoked for an unknown method")
		})
	}

	t.Run("core preserves advertised capabilities", func(t *testing.T) {
		binaryReply, err := proto.Marshal(&corev1.BuildInfo{
			Capabilities: []corev1.BuildCapability{
				corev1.BuildCapability_BUILD_CAPABILITY_VPC_SLAAC,
			},
		})
		require.NoError(t, err)

		conn := &fakeProxyConn{replyBytes: binaryReply}
		respJSON, err := invokeProxyJSONConn(context.Background(), conn, grpcproxy.Core, "Version", nil)
		require.NoError(t, err)
		assert.Equal(t, "/forge.Forge/Version", conn.lastMethod)
		assert.JSONEq(t, `{"capabilities":["BUILD_CAPABILITY_VPC_SLAAC"]}`, string(respJSON))
	})
}

func TestProxyMethodName(t *testing.T) {
	for _, tc := range proxyBackendCases {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.qualifiedName, proxyMethodName(tc.qualified))
			assert.Equal(t, tc.qualifiedName, proxyMethodName(tc.qualifiedName))
			assert.Equal(t, tc.qualified, proxyFullMethod(tc.backend, tc.qualifiedName))
			assert.Equal(t, "/"+tc.backend.ServiceName+"/Version", proxyFullMethod(tc.backend, "Version"))
		})
	}
}
