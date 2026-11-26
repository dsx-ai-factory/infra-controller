// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

const oauthStateTTL = 15 * time.Minute

var (
	// ErrInvalidOAuthState is returned when an OAuth state parameter fails validation.
	ErrInvalidOAuthState = errors.New("invalid oauth state")
)

type oauthStatePayload struct {
	ExpiresAt   int64  `json:"exp"`
	Nonce       string `json:"nonce"`
	RedirectURI string `json:"redirect_uri"`
	Email       string `json:"email"`
}

// generateOAuthState creates a signed OAuth state value bound to the login request.
func (k *KeycloakAuthService) generateOAuthState(email, redirectURI string) (string, error) {
	if k.config.ClientSecret == "" {
		return "", fmt.Errorf("client credentials not configured")
	}

	nonceBytes := make([]byte, 16)
	if _, err := rand.Read(nonceBytes); err != nil {
		return "", fmt.Errorf("failed to generate oauth state nonce: %w", err)
	}

	payload := oauthStatePayload{
		ExpiresAt:   time.Now().Add(oauthStateTTL).Unix(),
		Nonce:       base64.RawURLEncoding.EncodeToString(nonceBytes),
		RedirectURI: redirectURI,
		Email:       email,
	}

	payloadBytes, err := json.Marshal(payload)
	if err != nil {
		return "", fmt.Errorf("failed to marshal oauth state payload: %w", err)
	}

	signature := signOAuthStatePayload(k.config.ClientSecret, payloadBytes)
	encodedPayload := base64.RawURLEncoding.EncodeToString(payloadBytes)
	encodedSignature := base64.RawURLEncoding.EncodeToString(signature)

	return encodedPayload + "." + encodedSignature, nil
}

// ValidateOAuthState verifies that the state was issued by this service and matches the callback.
func (k *KeycloakAuthService) ValidateOAuthState(state, redirectURI string) error {
	if k.config.ClientSecret == "" {
		return fmt.Errorf("%w: client credentials not configured", ErrInvalidOAuthState)
	}

	payload, err := parseAndVerifyOAuthState(k.config.ClientSecret, state)
	if err != nil {
		return err
	}

	if time.Now().Unix() > payload.ExpiresAt {
		return fmt.Errorf("%w: state expired", ErrInvalidOAuthState)
	}

	if payload.RedirectURI != redirectURI {
		return fmt.Errorf("%w: redirect URI mismatch", ErrInvalidOAuthState)
	}

	return nil
}

func parseAndVerifyOAuthState(secret, state string) (*oauthStatePayload, error) {
	parts := strings.Split(state, ".")
	if len(parts) != 2 {
		return nil, fmt.Errorf("%w: malformed state", ErrInvalidOAuthState)
	}

	payloadBytes, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return nil, fmt.Errorf("%w: malformed state payload", ErrInvalidOAuthState)
	}

	signature, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil, fmt.Errorf("%w: malformed state signature", ErrInvalidOAuthState)
	}

	expectedSignature := signOAuthStatePayload(secret, payloadBytes)
	if !hmac.Equal(signature, expectedSignature) {
		return nil, fmt.Errorf("%w: state signature mismatch", ErrInvalidOAuthState)
	}

	var payload oauthStatePayload
	if err := json.Unmarshal(payloadBytes, &payload); err != nil {
		return nil, fmt.Errorf("%w: malformed state payload", ErrInvalidOAuthState)
	}

	if payload.Nonce == "" {
		return nil, fmt.Errorf("%w: missing state nonce", ErrInvalidOAuthState)
	}

	return &payload, nil
}

func signOAuthStatePayload(secret string, payload []byte) []byte {
	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write(payload)
	return mac.Sum(nil)
}
