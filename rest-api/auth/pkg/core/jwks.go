// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"syscall"
	"time"

	"github.com/go-jose/go-jose/v4"
	"github.com/pkg/errors"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
)

const (
	// DefaultJWKSTimeout is the default timeout for JWKS fetch operations
	DefaultJWKSTimeout = 5 * time.Second

	// MaxJWKSResponseBytes caps the size of a JWKS response body.
	MaxJWKSResponseBytes = 1 << 20
)

var (
	// awsMetadataV6 is the AWS instance metadata endpoint over IPv6, which is not
	// link-local.
	awsMetadataV6 = net.ParseIP("fd00:ec2::254")

	// jwksTransport refuses link-local addresses, such as cloud metadata endpoints,
	// which no identity provider uses. It checks after name resolution, so a
	// hostname or redirect that resolves to one is refused too.
	jwksTransport = func() *http.Transport {
		t := http.DefaultTransport.(*http.Transport).Clone()
		t.DialContext = (&net.Dialer{
			Timeout: 30 * time.Second,
			Control: func(_, address string, _ syscall.RawConn) error {
				host, _, err := net.SplitHostPort(address)
				if err != nil {
					return err
				}
				if ip := net.ParseIP(host); ip == nil || ip.IsLinkLocalUnicast() || ip.Equal(awsMetadataV6) {
					return fmt.Errorf("refusing link-local address %s", host)
				}
				return nil
			},
		}).DialContext
		return t
	}()
)

// JWKS represents a set of JSON Web keys using go-jose
type JWKS struct {
	Set *jose.JSONWebKeySet
}

// NewJWKSFromURL creates a new set of JSON Web Keys given a URL using go-jose
// If timeout is zero or negative, uses the default timeout of 5 seconds
func NewJWKSFromURL(url string, timeout time.Duration) (*JWKS, error) {
	return NewJWKSFromURLContext(context.Background(), url, timeout)
}

// NewJWKSFromURLContext is NewJWKSFromURL with cancellation.
func NewJWKSFromURLContext(ctx context.Context, url string, timeout time.Duration) (*JWKS, error) {
	if timeout <= 0 {
		timeout = DefaultJWKSTimeout
	}

	client := &http.Client{Transport: otelhttp.NewTransport(jwksTransport)}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		log.Error().Err(err).Msgf("failed to create request for JWKS URL: %s", url)
		return nil, errors.Wrap(ErrJWKSFetch, err.Error())
	}

	resp, err := client.Do(req)
	if err != nil {
		log.Error().Err(err).Msgf("failed to fetch JWKS from URL: %s", url)
		return nil, fmt.Errorf("%w: %w", ErrJWKSFetch, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		log.Error().Msgf("JWKS fetch failed - status code %d for URL: %s", resp.StatusCode, url)
		return nil, errors.Wrapf(ErrJWKSFetch, "received status code %d", resp.StatusCode)
	}

	// Read one byte past the cap so an at-the-limit body is still distinguishable
	// from an oversized one.
	bodyBytes, err := io.ReadAll(io.LimitReader(resp.Body, MaxJWKSResponseBytes+1))
	if err != nil {
		log.Error().Err(err).Msg("failed to read response body")
		return nil, errors.Wrap(ErrJWKSFetch, err.Error())
	}
	if len(bodyBytes) > MaxJWKSResponseBytes {
		log.Error().Msgf("JWKS response from %s exceeds %d bytes", url, MaxJWKSResponseBytes)
		return nil, errors.Wrapf(ErrJWKSTooLarge, "response from %s exceeds %d bytes", url, MaxJWKSResponseBytes)
	}

	// Use go-jose to parse the JWKS
	jwks := &jose.JSONWebKeySet{}
	if err := json.Unmarshal(bodyBytes, jwks); err != nil {
		log.Error().Err(err).Msg("failed to unmarshal JWKS using go-jose")
		return nil, errors.Wrap(ErrInvalidJWK, err.Error())
	}

	return &JWKS{Set: jwks}, nil
}

// NewJWKSFromBytes parses a key set serialized by Marshal, applying the fetch path's
// checks.
func NewJWKSFromBytes(raw []byte) (*JWKS, error) {
	if len(raw) == 0 {
		return nil, ErrEmptyKeySet
	}

	set := &jose.JSONWebKeySet{}
	if err := json.Unmarshal(raw, set); err != nil {
		return nil, errors.Wrap(ErrInvalidJWK, err.Error())
	}

	jwks := &JWKS{Set: set}
	if err := jwks.Validate(); err != nil {
		return nil, err
	}

	return jwks, nil
}

// Validate reports whether the key set is usable for token verification: it must
// be non-empty and contain at least one key go-jose accepts.
func (jwks JWKS) Validate() error {
	if jwks.Set == nil || len(jwks.Set.Keys) == 0 {
		return ErrEmptyKeySet
	}

	for _, key := range jwks.Set.Keys {
		if key.Valid() {
			return nil
		}
	}

	return ErrNoValidKeys
}

// Marshal serializes the key set for persistence. Only public key material ever
// reaches a JWKS endpoint, so the result is safe to store.
func (jwks JWKS) Marshal() ([]byte, error) {
	if jwks.Set == nil {
		return nil, ErrEmptyKeySet
	}

	raw, err := json.Marshal(jwks.Set)
	if err != nil {
		return nil, errors.Wrap(ErrInvalidJWK, err.Error())
	}

	return raw, nil
}

// GetKeyByID returns a specific key by its ID, leveraging go-jose's key management
func (jwks JWKS) GetKeyByID(keyID string) (*jose.JSONWebKey, error) {
	keys := jwks.Set.Key(keyID)
	if len(keys) == 0 {
		return nil, errors.Wrapf(ErrKeyNotFound, "key ID %s", keyID)
	}
	return &keys[0], nil
}

// GetKeysForAlgorithm returns all keys that explicitly declare support for a specific algorithm
func (jwks JWKS) GetKeysForAlgorithm(algorithm string) []jose.JSONWebKey {
	var matchingKeys []jose.JSONWebKey

	for _, key := range jwks.Set.Keys {
		if key.Algorithm == algorithm {
			matchingKeys = append(matchingKeys, key)
		}
	}

	return matchingKeys
}
