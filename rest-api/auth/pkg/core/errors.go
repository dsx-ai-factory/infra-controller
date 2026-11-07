// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import "errors"

var (
	// --- JWKS Errors ---

	// ErrJWKSFetch is returned when JWKS cannot be fetched
	ErrJWKSFetch = errors.New("failed to fetch JWKS")
	// ErrInvalidJWK is returned when JWK is invalid
	ErrInvalidJWK = errors.New("invalid JWK")
	// ErrKeyNotFound is returned when a requested key is not found
	ErrKeyNotFound = errors.New("key not found")
	// ErrJWKSURLEmpty is returned when JWKS URL is empty
	ErrJWKSURLEmpty = errors.New("JWKS URL is empty")
	// ErrJWKSNotInitialized is returned when JWKS has not been initialized
	ErrJWKSNotInitialized = errors.New("JWKS not initialized - call UpdateAllJWKS first")
	// ErrEmptyKeySet is returned when JWKS key set is empty
	ErrEmptyKeySet = errors.New("JWKS key set is empty")
	// ErrNoValidKeys is returned when JWKS contains no valid keys
	ErrNoValidKeys = errors.New("JWKS contains no valid keys")
	// ErrJWKSUpdateInProgress is returned when a JWKS update is already in progress
	ErrJWKSUpdateInProgress = errors.New("JWKS update already in progress")

	// --- Token Validation Errors ---

	// ErrInvalidAudience is returned when token audience does not match (401)
	ErrInvalidAudience = errors.New("token audience does not match issuer configuration")
	// ErrInvalidScope is returned when token scopes do not match (403)
	ErrInvalidScope = errors.New("token scopes do not match required scopes for issuer")

	// --- Claim/Role Errors ---

	// ErrInvalidConfiguration is returned when no claim mapping is configured (401)
	ErrInvalidConfiguration = errors.New("no claim mapping configured for requested organization")
	// ErrNoClaimRoles is returned when no roles found in token claims (401)
	ErrNoClaimRoles = errors.New("no roles found in token claims for organization")
	// ErrReservedOrgName is returned when token claims a reserved organization name (403)
	ErrReservedOrgName = errors.New("token claims a reserved organization name")
	// ErrInvalidRole is returned when role is not in allowed roles set
	ErrInvalidRole = errors.New("role is not in allowed roles set")
)
