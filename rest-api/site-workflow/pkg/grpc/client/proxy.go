// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"errors"
	"fmt"
	"path"

	"google.golang.org/grpc"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/reflect/protoreflect"
	"google.golang.org/protobuf/reflect/protoregistry"
	"google.golang.org/protobuf/types/dynamicpb"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
)

// ErrUnknownProxyMethod is returned when a method name does not resolve to a
// unary RPC on the proxied service.
var ErrUnknownProxyMethod = errors.New("unknown proxied gRPC method")

// proxyMethodName returns the bare method name for a bare or fully qualified
// gRPC method ("/forge.Forge/CreateCredential" -> "CreateCredential").
func proxyMethodName(method string) string {
	return path.Base(method)
}

// proxyFullMethod returns the canonical "/<service>/<Method>" gRPC path.
func proxyFullMethod(backend grpcproxy.Backend, method string) string {
	return "/" + backend.ServiceName + "/" + proxyMethodName(method)
}

// resolveProxyMethod looks the method up in the global proto registry, which
// holds the backend's descriptors because this package imports its generated
// bindings (see core_client.go and flow_client.go).
func resolveProxyMethod(backend grpcproxy.Backend, method string) (protoreflect.MethodDescriptor, error) {
	desc, err := protoregistry.GlobalFiles.FindDescriptorByName(protoreflect.FullName(backend.ServiceName))
	if err != nil {
		return nil, fmt.Errorf("resolve service %q: %w", backend.ServiceName, err)
	}
	svc, ok := desc.(protoreflect.ServiceDescriptor)
	if !ok {
		return nil, fmt.Errorf("%q is not a gRPC service", backend.ServiceName)
	}
	md := svc.Methods().ByName(protoreflect.Name(proxyMethodName(method)))
	if md == nil {
		return nil, fmt.Errorf("%w: %q on %q", ErrUnknownProxyMethod, proxyMethodName(method), backend.ServiceName)
	}
	if md.IsStreamingClient() || md.IsStreamingServer() {
		return nil, fmt.Errorf("method %q is streaming and not supported by the proxy", md.Name())
	}
	return md, nil
}

// InvokeJSON proxies a unary forge.Forge call: it transcodes reqJSON (protojson)
// into the request message for method, invokes it on the Core connection, and
// returns the protojson-encoded response. An empty reqJSON is treated as the
// zero-valued request message.
func (cc *CoreGrpcClient) InvokeJSON(ctx context.Context, method string, reqJSON []byte) ([]byte, error) {
	return invokeProxyJSONConn(ctx, cc.conn, grpcproxy.Core, method, reqJSON)
}

// InvokeJSON proxies a unary v1.Flow call: it transcodes reqJSON (protojson)
// into the request message for method, invokes it on the Flow connection, and
// returns the protojson-encoded response. An empty reqJSON is treated as the
// zero-valued request message.
func (fg *FlowGrpcClient) InvokeJSON(ctx context.Context, method string, reqJSON []byte) ([]byte, error) {
	return invokeProxyJSONConn(ctx, fg.conn, grpcproxy.Flow, method, reqJSON)
}

// invokeProxyJSONConn is the transport-agnostic transcoder seam used by both
// InvokeJSON methods and exercised directly in tests with a fake connection.
func invokeProxyJSONConn(ctx context.Context, conn grpc.ClientConnInterface, backend grpcproxy.Backend, method string, reqJSON []byte) ([]byte, error) {
	md, err := resolveProxyMethod(backend, method)
	if err != nil {
		return nil, err
	}

	in := dynamicpb.NewMessage(md.Input())
	if len(reqJSON) > 0 {
		decodeErr := (protojson.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(reqJSON, in)
		if decodeErr != nil {
			return nil, fmt.Errorf("decode request for %q: %w", md.Name(), decodeErr)
		}
	}

	out := dynamicpb.NewMessage(md.Output())
	invokeErr := conn.Invoke(ctx, proxyFullMethod(backend, method), in, out)
	if invokeErr != nil {
		return nil, invokeErr
	}

	respJSON, err := protojson.Marshal(out)
	if err != nil {
		return nil, fmt.Errorf("encode response for %q: %w", md.Name(), err)
	}
	return respJSON, nil
}
