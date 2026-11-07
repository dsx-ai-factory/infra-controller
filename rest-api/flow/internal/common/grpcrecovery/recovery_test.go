// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package grpcrecovery

import (
	"bytes"
	"context"
	"errors"
	"testing"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"
	"google.golang.org/grpc/status"
)

func TestUnaryServerInterceptor(t *testing.T) {
	tests := map[string]struct {
		panicValue any
	}{
		"string panic": {
			panicValue: "boom",
		},
		"error panic": {
			panicValue: errors.New("boom"),
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			interceptor := UnaryServerInterceptor(nil)
			info := &grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Test"}

			response, err := interceptor(
				context.Background(),
				struct{}{},
				info,
				func(context.Context, any) (any, error) {
					panic(test.panicValue)
				},
			)
			require.Error(t, err)
			assert.Nil(t, response)
			assert.Equal(t, codes.Internal, status.Code(err))
			assert.Equal(t, internalErrorMessage, status.Convert(err).Message())

			response, err = interceptor(
				context.Background(),
				struct{}{},
				info,
				func(context.Context, any) (any, error) {
					return "ok", nil
				},
			)
			require.NoError(t, err)
			assert.Equal(t, "ok", response)
		})
	}
}

func TestUnaryServerInterceptorLogsServiceIdentity(t *testing.T) {
	const identity = "spiffe://example.test/workload"
	type contextKey struct{}

	var output bytes.Buffer
	originalLogger := log.Logger
	log.Logger = zerolog.New(&output)
	t.Cleanup(func() {
		log.Logger = originalLogger
	})

	ctx := context.WithValue(context.Background(), contextKey{}, identity)
	identityLookup := func(ctx context.Context) (string, bool) {
		value, ok := ctx.Value(contextKey{}).(string)
		return value, ok
	}
	interceptor := UnaryServerInterceptor(identityLookup)

	_, err := interceptor(
		ctx,
		struct{}{},
		&grpc.UnaryServerInfo{FullMethod: "/flow.v1.Flow/Test"},
		func(context.Context, any) (any, error) {
			panic("boom")
		},
	)

	require.Error(t, err)
	assert.Equal(t, codes.Internal, status.Code(err))
	assert.Contains(t, output.String(), `"service_identity":"`+identity+`"`)
}

func TestStreamServerInterceptor(t *testing.T) {
	const identity = "spiffe://example.test/workload"

	tests := map[string]struct {
		identityLookup ServiceIdentityLookup
		panicValue     any
		wantCode       codes.Code
		wantIdentity   bool
	}{
		"panic without identity": {
			panicValue: "boom",
			wantCode:   codes.Internal,
		},
		"panic with identity": {
			identityLookup: func(context.Context) (string, bool) {
				return identity, true
			},
			panicValue:   "boom",
			wantCode:     codes.Internal,
			wantIdentity: true,
		},
		"success": {
			wantCode: codes.OK,
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			var output bytes.Buffer
			originalLogger := log.Logger
			log.Logger = zerolog.New(&output)
			t.Cleanup(func() {
				log.Logger = originalLogger
			})

			interceptor := StreamServerInterceptor(test.identityLookup)
			info := &grpc.StreamServerInfo{FullMethod: "/flow.v1.Flow/TestStream"}
			stream := &testServerStream{ctx: context.Background()}
			err := interceptor(
				struct{}{},
				stream,
				info,
				func(any, grpc.ServerStream) error {
					if test.panicValue != nil {
						panic(test.panicValue)
					}
					return nil
				},
			)

			assert.Equal(t, test.wantCode, status.Code(err))
			if test.wantCode == codes.Internal {
				assert.Equal(t, internalErrorMessage, status.Convert(err).Message())
			}
			assert.Equal(
				t,
				test.wantIdentity,
				bytes.Contains(output.Bytes(), []byte(`"service_identity":"`+identity+`"`)),
			)
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
