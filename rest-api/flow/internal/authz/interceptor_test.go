// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authz

import (
	"context"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/peer"
	"google.golang.org/grpc/status"
)

func TestUnaryServerInterceptor(t *testing.T) {
	authorizer, err := New(Config{
		AllowedServiceIdentities: []string{allowedIdentity},
		Mode:                     ModeEnforce,
	})
	require.NoError(t, err)

	tests := map[string]struct {
		ctx         context.Context
		wantCode    codes.Code
		handlerRuns bool
	}{
		"allowed": {
			ctx:         tlsPeerContext(certificateWithURIs(t, allowedIdentity)),
			wantCode:    codes.OK,
			handlerRuns: true,
		},
		"denied": {
			ctx:         tlsPeerContext(certificateWithURIs(t, deniedIdentity)),
			wantCode:    codes.PermissionDenied,
			handlerRuns: false,
		},
		"no TLS peer": {
			ctx:         peer.NewContext(context.Background(), &peer.Peer{}),
			wantCode:    codes.Unauthenticated,
			handlerRuns: false,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			handlerRan := false
			_, err := UnaryServerInterceptor(authorizer)(
				test.ctx,
				struct{}{},
				&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Test"},
				func(ctx context.Context, _ any) (any, error) {
					handlerRan = true
					authorization, ok := AuthorizationFromContext(ctx)
					require.True(t, ok)
					assert.Equal(t, allowedIdentity, authorization.ServiceIdentity)
					assert.True(t, authorization.Allowed)
					return struct{}{}, nil
				},
			)

			assert.Equal(t, test.wantCode, status.Code(err))
			assert.Equal(t, test.handlerRuns, handlerRan)
		})
	}
}

func TestStreamServerInterceptor(t *testing.T) {
	tests := map[string]struct {
		mode        Mode
		identity    string
		wantCode    codes.Code
		handlerRuns bool
		wantAllowed bool
	}{
		"enforce allowed": {
			mode:        ModeEnforce,
			identity:    allowedIdentity,
			wantCode:    codes.OK,
			handlerRuns: true,
			wantAllowed: true,
		},
		"enforce denied": {
			mode:        ModeEnforce,
			identity:    deniedIdentity,
			wantCode:    codes.PermissionDenied,
			handlerRuns: false,
			wantAllowed: false,
		},
		"audit denied": {
			mode:        ModeAudit,
			identity:    deniedIdentity,
			wantCode:    codes.OK,
			handlerRuns: true,
			wantAllowed: false,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			authorizer, err := New(Config{
				AllowedServiceIdentities: []string{allowedIdentity},
				Mode:                     test.mode,
			})
			require.NoError(t, err)

			stream := &testServerStream{
				ctx: tlsPeerContext(certificateWithURIs(t, test.identity)),
			}
			handlerRan := false
			err = StreamServerInterceptor(authorizer)(
				struct{}{},
				stream,
				&grpc.StreamServerInfo{FullMethod: "/flow.v1.Flow/TestStream"},
				func(_ any, stream grpc.ServerStream) error {
					handlerRan = true
					authorization, ok := AuthorizationFromContext(stream.Context())
					require.True(t, ok)
					assert.Equal(t, test.identity, authorization.ServiceIdentity)
					assert.Equal(t, test.wantAllowed, authorization.Allowed)
					return nil
				},
			)

			assert.Equal(t, test.wantCode, status.Code(err))
			assert.Equal(t, test.handlerRuns, handlerRan)
		})
	}
}

func TestUnaryServerInterceptorAuditMode(t *testing.T) {
	authorizer, err := New(Config{Mode: ModeAudit})
	require.NoError(t, err)

	tests := map[string]struct {
		ctx      context.Context
		identity string
	}{
		"denied identity is propagated": {
			ctx:      tlsPeerContext(certificateWithURIs(t, deniedIdentity)),
			identity: deniedIdentity,
		},
		"unidentified caller gets audit identity": {
			ctx:      context.Background(),
			identity: AuditUnidentifiedIdentity,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			_, err := UnaryServerInterceptor(authorizer)(
				test.ctx,
				struct{}{},
				&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Test"},
				func(ctx context.Context, _ any) (any, error) {
					authorization, ok := AuthorizationFromContext(ctx)
					require.True(t, ok)
					assert.Equal(t, test.identity, authorization.ServiceIdentity)
					assert.False(t, authorization.Allowed)

					identity, ok := ServiceIdentityFromContext(ctx)
					require.True(t, ok)
					assert.Equal(t, test.identity, identity)
					return struct{}{}, nil
				},
			)
			require.NoError(t, err)
		})
	}
}

func TestRejectionGRPCError(t *testing.T) {
	tests := map[string]struct {
		rejection rejectionKind
		wantCode  codes.Code
	}{
		"none": {
			rejection: rejectionNone,
			wantCode:  codes.OK,
		},
		"unauthenticated": {
			rejection: rejectionUnauthenticated,
			wantCode:  codes.Unauthenticated,
		},
		"permission denied": {
			rejection: rejectionPermissionDenied,
			wantCode:  codes.PermissionDenied,
		},
		"unknown": {
			rejection: rejectionKind(100),
			wantCode:  codes.Internal,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			assert.Equal(t, test.wantCode, status.Code(test.rejection.grpcError()))
		})
	}
}

type testServerStream struct {
	ctx context.Context
}

func (*testServerStream) SetHeader(metadata.MD) error  { return nil }
func (*testServerStream) SendHeader(metadata.MD) error { return nil }
func (*testServerStream) SetTrailer(metadata.MD)       {}
func (s *testServerStream) Context() context.Context   { return s.ctx }
func (*testServerStream) SendMsg(any) error            { return nil }
func (*testServerStream) RecvMsg(any) error            { return nil }
