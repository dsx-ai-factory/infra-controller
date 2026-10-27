// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"context"
	"errors"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/reflect/protoreflect"
)

// fakeProxyConn implements grpc.ClientConnInterface for transcoder tests. It
// records the invoked method and populates the first scalar string field of the
// reply so the response-encoding path runs without a real server.
type fakeProxyConn struct {
	lastMethod  string
	lastRequest proto.Message
	setValue    string
	replyBytes  []byte
}

func (f *fakeProxyConn) Invoke(_ context.Context, method string, request, reply any, _ ...grpc.CallOption) error {
	f.lastMethod = method
	message, ok := request.(proto.Message)
	if ok {
		f.lastRequest = proto.Clone(message)
	}
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
	// REST decodes the dynamic proxy's JSON using its own generated Flow bindings.
	// Exercise that boundary so missing descriptors cannot silently discard external
	// identities, their availability, or the imported Core observation.
	for _, tc := range []struct {
		name            string
		externalRackID  *string
		externalTargets *flowv1.PowerTaskExternalTargets
	}{
		{
			// Identical ID text across component types must retain both typed identities.
			name:           "populated typed targets",
			externalRackID: proto.String("rack-123"),
			externalTargets: &flowv1.PowerTaskExternalTargets{
				RackId: "rack-123",
				Targets: []*flowv1.ExternalRef{
					{Type: flowv1.ComponentType_COMPONENT_TYPE_COMPUTE, Id: "component-123"},
					{Type: flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH, Id: "component-123"},
				},
			},
		},
		{
			// Omission means the service or proxy did not supply external identities.
			name: "absent external target support",
		},
		{
			// Successful empty selection must remain distinguishable from omission.
			name:            "present empty selection",
			externalRackID:  proto.String("rack-123"),
			externalTargets: &flowv1.PowerTaskExternalTargets{RackId: "rack-123"},
		},
		{
			// A failed lookup must not become a successful empty selection in JSON.
			name:           "explicit external target failure",
			externalRackID: proto.String("rack-123"),
			externalTargets: &flowv1.PowerTaskExternalTargets{
				Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_NOT_LINKED.Enum(),
			},
		},
	} {
		t.Run("flow power details "+tc.name, func(t *testing.T) {
			reply := &flowv1.GetTrayPowerDetailsResponse{
				Details: &flowv1.TrayPowerDetails{
					ComponentId:    proto.String("component-123"),
					RackExternalId: tc.externalRackID,
					Actual: &flowv1.PowerActualDetails{Result: &flowv1.PowerActualDetails_Value{
						Value: &corev1.ComponentPowerObservation{Outcome: &corev1.ComponentPowerObservation_State{
							State: corev1.ComponentPowerState_COMPONENT_POWER_STATE_ON,
						}},
					}},
					Operations: &flowv1.PowerOperations{
						Actions: &flowv1.PowerActionDetails{Result: &flowv1.PowerActionDetails_Error{
							Error: flowv1.PowerDetailsReadError_POWER_DETAILS_READ_ERROR_UNSUPPORTED,
						}},
						Tasks: &flowv1.PowerTaskDetailsList{Entries: []*flowv1.PowerTaskDetails{
							{ExternalTargets: tc.externalTargets},
						}},
					},
				},
			}
			binaryReply, err := proto.Marshal(reply)
			require.NoError(t, err)
			conn := &fakeProxyConn{replyBytes: binaryReply}

			response, err := invokeProxyJSONConn(t.Context(), conn, grpcproxy.Flow, "GetTrayPowerDetails", []byte(`{"id":"aa:bb:cc:dd:ee:ff"}`))

			require.NoError(t, err)
			assert.Equal(t, "/v1.Flow/GetTrayPowerDetails", conn.lastMethod)
			require.NotNil(t, conn.lastRequest)
			request := conn.lastRequest.ProtoReflect()
			assert.Equal(t, "aa:bb:cc:dd:ee:ff", request.Get(request.Descriptor().Fields().ByName("id")).String())

			// A renamed JSON key can round-trip unnoticed when both bindings change.
			assert.NotContains(t, string(response), `"internalComponentId"`)
			assert.NotContains(t, string(response), `"flowComponentId"`)
			assert.Contains(t, string(response), `"componentId"`)
			if tc.externalRackID != nil {
				assert.Contains(t, string(response), `"rackExternalId"`)
			}

			// Match the generated type and decode options used by REST's Flow proxy.
			var decoded flowv1.GetTrayPowerDetailsResponse
			err = (protojson.UnmarshalOptions{DiscardUnknown: true}).Unmarshal(response, &decoded)
			require.NoError(t, err)
			assert.True(t, proto.Equal(reply, &decoded), "proxy response = %s; want %s", decoded.String(), reply.String())
		})
	}

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
