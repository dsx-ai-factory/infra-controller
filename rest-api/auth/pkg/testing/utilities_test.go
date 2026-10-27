// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package testing

import (
	"crypto/ecdsa"
	"crypto/elliptic"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestMockServerHelper_createJWKMap(t *testing.T) {
	const keyUse = "sig"
	tests := []struct {
		name      string
		curve     elliptic.Curve
		scalar    byte
		algorithm string
		wantX     string
		wantY     string
	}{
		{
			name:      "P-256",
			curve:     elliptic.P256(),
			scalar:    43,
			algorithm: "ES256",
			wantX:     "mGriUG8f8QTQQjCGHY9LSY9LxMbQCbMPdUTcEpuC0o0",
			wantY:     "PMzApkYOCuMopNl9PHth2G_GKJwYnyUlEQxEG7B-lw",
		},
		{
			name:      "P-384",
			curve:     elliptic.P384(),
			scalar:    1,
			algorithm: "ES384",
			wantX:     "qofKIr6LBTeOscce8yCtdG4dO2KLp5uYWfdB4IJUKjhVAvJdv1UpbDpUXjhydgq3",
			wantY:     "NhfeSpYmLG9dnpi_kpLcKfj0Hb0omhR86doxE7XwuMAKYLHOHX6BnXpDHXyQ6g5f",
		},
		{
			name:      "P-521",
			curve:     elliptic.P521(),
			scalar:    1,
			algorithm: "ES512",
			wantX:     "xoWOBrcEBOnNnj7LZiOVtEKcZIE5BT-1Ifgor2BrTT26oUted-_nWSj-HcEnov-o3jNIs8GFakKb-X5-McLlvWY",
			wantY:     "ARg5KWp4mjvABFyKX7QsfRvZmPVESVebRGgXr70XJz5mLJfucple9CZAxVC5AT-tB2E1PHCGonLCQIi-lHaf0WZQ",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			scalar := make([]byte, (tt.curve.Params().BitSize+7)/8)
			scalar[len(scalar)-1] = tt.scalar
			key, err := ecdsa.ParseRawPrivateKey(tt.curve, scalar)
			require.NoError(t, err)
			result := NewMockServerHelper(t).createJWKMap(&key.PublicKey, TestKeyID, tt.algorithm, keyUse)
			assert.Equal(t, map[string]interface{}{
				"kid": TestKeyID, "kty": "EC", "crv": tt.name, "alg": tt.algorithm,
				"use": keyUse, "x": tt.wantX, "y": tt.wantY,
			}, result)
		})
	}
}
