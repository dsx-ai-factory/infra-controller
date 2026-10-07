// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authz

import (
	"context"
	"fmt"
	"strings"

	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

// Config configures the service identities allowed to invoke Flow.
type Config struct {
	AllowedServiceIdentities []string
	Mode                     Mode
}

// Mode controls whether authorization failures are enforced or only recorded.
type Mode string

const (
	ModeUndefined Mode = ""
	ModeEnforce   Mode = "ENFORCE"
	ModeAudit     Mode = "AUDIT"
	DefaultMode        = ModeAudit

	AuditUnidentifiedIdentity = "audit-unidentified"
)

// NewMode constructs and validates a Mode from its string representation.
func NewMode(value string) (Mode, error) {
	value = strings.TrimSpace(value)
	if value == "" {
		return DefaultMode, nil
	}

	mode := Mode(value)
	if err := mode.Validate(); err != nil {
		return ModeUndefined, err
	}

	return mode, nil
}

// Validate checks that the mode is supported.
func (m Mode) Validate() error {
	switch m {
	case ModeEnforce, ModeAudit:
		return nil
	default:
		return fmt.Errorf(
			"authorization mode must be %q or %q, got %q",
			ModeEnforce,
			ModeAudit,
			m,
		)
	}
}

func (m Mode) enforce() bool {
	return m == ModeEnforce
}

// Validate checks that the mode is supported, enforce mode has at least one
// allowed identity, and every allowed identity is a unique canonical SPIFFE
// workload URI.
func (c Config) Validate() error {
	_, err := c.buildAllowedServiceIdentities()
	return err
}

func (c Config) buildAllowedServiceIdentities() (map[string]struct{}, error) {
	if err := c.Mode.Validate(); err != nil {
		return nil, err
	}

	if c.Mode == ModeEnforce && len(c.AllowedServiceIdentities) == 0 {
		return nil, fmt.Errorf("at least one allowed service identity is required")
	}

	allowed := make(map[string]struct{}, len(c.AllowedServiceIdentities))
	for _, identity := range c.AllowedServiceIdentities {
		if err := validateSPIFFEIdentity(identity); err != nil {
			return nil, fmt.Errorf("invalid allowed service identity: %w", err)
		}

		if _, ok := allowed[identity]; ok {
			return nil, fmt.Errorf("duplicate allowed service identity %q", identity)
		}

		allowed[identity] = struct{}{}
	}

	return allowed, nil
}

// Authorizer checks the mTLS-authenticated service identity against an exact
// allowlist.
type Authorizer struct {
	allowed map[string]struct{}
	mode    Mode
}

// New constructs a production authorizer.
func New(config Config) (*Authorizer, error) {
	allowed, err := config.buildAllowedServiceIdentities()
	if err != nil {
		return nil, err
	}

	if config.Mode == ModeAudit && len(allowed) == 0 {
		log.Warn().
			Str("authorization_mode", string(config.Mode)).
			Msg("gRPC authorization allowlist is empty; audit mode will allow all callers")
	}

	return &Authorizer{allowed: allowed, mode: config.Mode}, nil
}

type rejectionKind int

const (
	rejectionNone rejectionKind = iota
	rejectionUnauthenticated
	rejectionPermissionDenied
)

func (r rejectionKind) grpcError() error {
	switch r {
	case rejectionNone:
		return nil
	case rejectionUnauthenticated:
		return status.Error(codes.Unauthenticated, "authenticated service identity is required")
	case rejectionPermissionDenied:
		return status.Error(codes.PermissionDenied, "caller is not authorized to invoke Flow")
	default:
		return status.Error(codes.Internal, "authorization decision is invalid")
	}
}

func (a *Authorizer) authorize(ctx context.Context, method string) (Authorization, error) {
	// Authenticate the caller by extracting its SPIFFE identity from the
	// verified TLS connection.
	identity, identityErr := identityFromContext(ctx)

	rejection := rejectionNone
	message := ""
	if identityErr != nil {
		rejection = rejectionUnauthenticated
		message = identityErr.Error()
	} else {
		// An authenticated caller is authorized only when its identity appears
		// in the configured allow list.
		if _, ok := a.allowed[identity]; !ok {
			rejection = rejectionPermissionDenied
			message = "service identity is not allowed"
		}
	}

	err := rejection.grpcError()
	if err == nil {
		return Authorization{ServiceIdentity: identity, Allowed: true}, nil
	}

	if a.mode == ModeAudit && len(a.allowed) == 0 {
		if identity == "" {
			identity = AuditUnidentifiedIdentity
		}
		return Authorization{ServiceIdentity: identity, Allowed: false}, nil
	}

	// Failed authorization against a configured allowlist is logged. Enforce
	// mode rejects the request, while audit mode allows it to continue.
	enforce := a.mode.enforce()

	level := zerolog.WarnLevel
	if enforce {
		level = zerolog.ErrorLevel
	}

	log.WithLevel(level).
		Str("grpc.method", method).
		Str("grpc.code", status.Code(err).String()).
		Str("authorization_mode", string(a.mode)).
		Str("service_identity", identity).
		Str("rejection_reason", message).
		Msg("gRPC authorization rejected caller")

	if enforce {
		return Authorization{}, err
	}

	if identity == "" {
		identity = AuditUnidentifiedIdentity
	}

	return Authorization{ServiceIdentity: identity, Allowed: false}, nil
}
