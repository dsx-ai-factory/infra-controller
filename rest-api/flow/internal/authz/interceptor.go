// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authz

import (
	"context"

	"google.golang.org/grpc"
)

// UnaryServerInterceptor authorizes unary RPCs using the mTLS service identity.
func UnaryServerInterceptor(authorizer *Authorizer) grpc.UnaryServerInterceptor {
	return func(
		ctx context.Context,
		req any,
		info *grpc.UnaryServerInfo,
		handler grpc.UnaryHandler,
	) (any, error) {
		authorization, err := authorizer.authorize(ctx, info.FullMethod)
		if err != nil {
			return nil, err
		}

		return handler(withAuthorization(ctx, authorization), req)
	}
}

// StreamServerInterceptor authorizes streaming RPCs using the mTLS service
// identity.
func StreamServerInterceptor(authorizer *Authorizer) grpc.StreamServerInterceptor {
	return func(
		srv any,
		stream grpc.ServerStream,
		info *grpc.StreamServerInfo,
		handler grpc.StreamHandler,
	) error {
		authorization, err := authorizer.authorize(stream.Context(), info.FullMethod)
		if err != nil {
			return err
		}

		return handler(
			srv,
			&serviceIdentityServerStream{
				ServerStream: stream,
				ctx:          withAuthorization(stream.Context(), authorization),
			},
		)
	}
}

type serviceIdentityServerStream struct {
	grpc.ServerStream
	ctx context.Context
}

func (s *serviceIdentityServerStream) Context() context.Context {
	return s.ctx
}
