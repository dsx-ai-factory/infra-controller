// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package authz authorizes direct callers of the Flow gRPC service using the
// workload identity authenticated by mTLS.
package authz

import (
	"context"
	"errors"
	"fmt"

	"github.com/spiffe/go-spiffe/v2/spiffeid"
	"github.com/spiffe/go-spiffe/v2/spiffetls"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/peer"
)

func identityFromContext(ctx context.Context) (string, error) {
	p, ok := peer.FromContext(ctx)
	if !ok {
		return "", errors.New("gRPC peer information is missing")
	}

	tlsInfo, ok := p.AuthInfo.(credentials.TLSInfo)
	if !ok {
		return "", errors.New("gRPC peer is not authenticated with TLS")
	}

	if len(tlsInfo.State.VerifiedChains) == 0 {
		return "", errors.New("TLS peer has no verified certificate chain")
	}

	id, err := spiffetls.PeerIDFromConnectionState(tlsInfo.State)
	if err != nil {
		return "", fmt.Errorf("extract SPIFFE peer identity: %w", err)
	}

	return id.String(), nil
}

func validateSPIFFEIdentity(identity string) error {
	id, err := spiffeid.FromString(identity)
	if err != nil {
		return fmt.Errorf("identity %q is not a valid SPIFFE ID: %w", identity, err)
	}

	if id.Path() == "" {
		return fmt.Errorf("identity %q must include a workload path", identity)
	}

	if id.String() != identity {
		return fmt.Errorf("identity %q is not canonical", identity)
	}

	return nil
}

// Authorization describes the caller identity and whether the configured
// policy allowed it. In audit mode, a disallowed caller may still proceed with
// Allowed set to false.
type Authorization struct {
	ServiceIdentity string
	Allowed         bool
}

type authorizationContextKey struct{}

func withAuthorization(ctx context.Context, authorization Authorization) context.Context {
	return context.WithValue(ctx, authorizationContextKey{}, authorization)
}

// AuthorizationFromContext returns the caller authorization attached by the
// Flow authorization interceptor.
func AuthorizationFromContext(ctx context.Context) (Authorization, bool) {
	authorization, ok := ctx.Value(authorizationContextKey{}).(Authorization)
	return authorization, ok
}

// ServiceIdentityFromContext returns only the caller identity. Consumers that
// make authorization-sensitive decisions must use AuthorizationFromContext so
// audit-mode callers are not mistaken for allowlisted callers.
func ServiceIdentityFromContext(ctx context.Context) (string, bool) {
	authorization, ok := AuthorizationFromContext(ctx)
	return authorization.ServiceIdentity, ok
}
