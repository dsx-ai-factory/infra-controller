// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package core

import (
	"context"
	"encoding/json"
	"io"
	"net/http"
	"time"

	"github.com/go-jose/go-jose/v4"
	"github.com/pkg/errors"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
)

// DefaultJWKSTimeout is the default timeout for JWKS fetch operations
const DefaultJWKSTimeout = 5 * time.Second

// JWKS represents a set of JSON Web keys using go-jose
type JWKS struct {
	Set *jose.JSONWebKeySet
}

// NewJWKSFromURL creates a new set of JSON Web Keys given a URL using go-jose
// If timeout is zero or negative, uses the default timeout of 5 seconds
func NewJWKSFromURL(url string, timeout time.Duration) (*JWKS, error) {
	if timeout <= 0 {
		timeout = DefaultJWKSTimeout
	}

	client := &http.Client{Transport: otelhttp.NewTransport(http.DefaultTransport)}
	// Deliberately detached from any request context: this is a throttled,
	// shared-cache refresh whose outcome serves all requests, so it must not
	// be cancelled by (or traced under) whichever request triggered it.
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, "GET", url, nil)
	if err != nil {
		log.Error().Err(err).Msgf("failed to create request for JWKS URL: %s", url)
		return nil, errors.Wrap(ErrJWKSFetch, err.Error())
	}

	resp, err := client.Do(req)
	if err != nil {
		log.Error().Err(err).Msgf("failed to fetch JWKS from URL: %s", url)
		return nil, errors.Wrap(ErrJWKSFetch, err.Error())
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		log.Error().Msgf("JWKS fetch failed - status code %d for URL: %s", resp.StatusCode, url)
		return nil, errors.Wrapf(ErrJWKSFetch, "received status code %d", resp.StatusCode)
	}

	// Read the response body
	bodyBytes, err := io.ReadAll(resp.Body)
	if err != nil {
		log.Error().Err(err).Msg("failed to read response body")
		return nil, errors.Wrap(ErrJWKSFetch, err.Error())
	}

	// Use go-jose to parse the JWKS
	jwks := &jose.JSONWebKeySet{}
	if err := json.Unmarshal(bodyBytes, jwks); err != nil {
		log.Error().Err(err).Msg("failed to unmarshal JWKS using go-jose")
		return nil, errors.Wrap(ErrInvalidJWK, err.Error())
	}

	return &JWKS{Set: jwks}, nil
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
