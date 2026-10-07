// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwareauth

import (
	"encoding/base64"
	"errors"
	"testing"

	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestEncryptAndDecryptFor(t *testing.T) {
	cipher := newTestCipher(t, 1)

	tests := []struct {
		name               string
		authenticationData *pb.FirmwareAuthenticationData
		componentType      devicetypes.ComponentType
		want               string
	}{
		{
			name:               "shared value",
			authenticationData: sharedAuthenticationData("shared-token"),
			componentType:      devicetypes.ComponentTypeNVSwitch,
			want:               "shared-token",
		},
		{
			name: "shared JSON value remains opaque",
			authenticationData: sharedAuthenticationData(
				`{"header":"opaque-value"}`,
			),
			componentType: devicetypes.ComponentTypeCompute,
			want:          `{"header":"opaque-value"}`,
		},
		{
			name: "mapped value",
			authenticationData: perComponentAuthenticationData(
				proto.String("compute-token"),
				proto.String("switch-token"),
				nil,
			),
			componentType: devicetypes.ComponentTypeCompute,
			want:          "compute-token",
		},
		{
			name: "missing mapped value means no authentication",
			authenticationData: perComponentAuthenticationData(
				proto.String("compute-token"),
				nil,
				nil,
			),
			componentType: devicetypes.ComponentTypePowerShelf,
			want:          "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			encrypted, err := Encrypt(cipher, tt.authenticationData, nil)
			require.NoError(t, err)
			require.NotNil(t, encrypted)
			require.Equal(t, secret.EncryptedDataVersion, encrypted.Version)
			require.NotEmpty(t, encrypted.KeyID)
			if tt.want != "" {
				require.NotContains(t, string(encrypted.Ciphertext), tt.want)
			}

			got, err := DecryptFor(cipher, encrypted, tt.componentType)
			require.NoError(t, err)
			require.Equal(t, tt.want, got)
		})
	}
}

func TestEncryptRejectsInvalidTypedData(t *testing.T) {
	cipher := newTestCipher(t, 1)

	tests := []struct {
		name               string
		cipher             *secret.Cipher
		authenticationData *pb.FirmwareAuthenticationData
		subTargets         []string
		wantInvalid        bool
		wantUnavailable    bool
	}{
		{
			name:               "missing value",
			cipher:             cipher,
			authenticationData: &pb.FirmwareAuthenticationData{},
			wantInvalid:        true,
		},
		{
			name:   "missing per-component data",
			cipher: cipher,
			authenticationData: &pb.FirmwareAuthenticationData{
				Value: &pb.FirmwareAuthenticationData_PerComponent{},
			},
			wantInvalid: true,
		},
		{
			name:               "authentication with dpu-only subtargets",
			cipher:             cipher,
			authenticationData: sharedAuthenticationData("token"),
			subTargets:         []string{"DPU"},
			wantInvalid:        true,
		},
		{
			name:               "authentication without data cipher",
			authenticationData: sharedAuthenticationData("token"),
			wantUnavailable:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := Encrypt(tt.cipher, tt.authenticationData, tt.subTargets)
			require.Error(t, err)
			require.Equal(t, tt.wantInvalid, IsInvalidData(err))
			require.Equal(
				t,
				tt.wantUnavailable,
				errors.Is(err, ErrDataCipherNotConfigured),
			)
		})
	}
}

func TestDecryptRejectsEnvelopeFailures(t *testing.T) {
	cipher := newTestCipher(t, 1)
	encrypted, err := Encrypt(cipher, sharedAuthenticationData("token"), nil)
	require.NoError(t, err)

	tests := []struct {
		name            string
		cipher          *secret.Cipher
		mutate          func(*secret.EncryptedData)
		wantErr         string
		wantUnavailable bool
	}{
		{
			name:            "missing data cipher",
			wantErr:         ErrDataCipherNotConfigured.Error(),
			wantUnavailable: true,
		},
		{
			name:    "wrong key",
			cipher:  newTestCipher(t, 2),
			wantErr: "does not match configured key ID",
		},
		{
			name:   "wrong key ID",
			cipher: cipher,
			mutate: func(data *secret.EncryptedData) {
				data.KeyID = "different-key"
			},
			wantErr: "does not match configured key ID",
		},
		{
			name:   "unsupported envelope version",
			cipher: cipher,
			mutate: func(data *secret.EncryptedData) {
				data.Version++
			},
			wantErr: "unsupported encrypted data version",
		},
		{
			name:   "malformed ciphertext",
			cipher: cipher,
			mutate: func(data *secret.EncryptedData) {
				data.Ciphertext = []byte("short")
			},
			wantErr: "encrypted data is too short",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			candidate := *encrypted
			candidate.Ciphertext = append([]byte(nil), encrypted.Ciphertext...)
			if tt.mutate != nil {
				tt.mutate(&candidate)
			}

			_, err := DecryptFor(
				tt.cipher,
				&candidate,
				devicetypes.ComponentTypeCompute,
			)
			require.ErrorContains(t, err, tt.wantErr)
			require.Equal(
				t,
				tt.wantUnavailable,
				errors.Is(err, ErrDataCipherNotConfigured),
			)
		})
	}
}

func TestDecryptRejectsUnsupportedComponentType(t *testing.T) {
	cipher := newTestCipher(t, 1)
	tests := []struct {
		name               string
		authenticationData *pb.FirmwareAuthenticationData
	}{
		{
			name:               "shared",
			authenticationData: sharedAuthenticationData("token"),
		},
		{
			name: "per-component",
			authenticationData: perComponentAuthenticationData(
				proto.String("token"), nil, nil,
			),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			encrypted, err := Encrypt(cipher, tt.authenticationData, nil)
			require.NoError(t, err)

			_, err = DecryptFor(
				cipher,
				encrypted,
				devicetypes.ComponentTypeToRSwitch,
			)
			require.ErrorContains(t, err, "does not support component type")
		})
	}
}

func TestDecryptRejectsMalformedPlaintext(t *testing.T) {
	cipher := newTestCipher(t, 1)
	encrypted, err := cipher.EncryptData(
		[]byte{0xff},
		[]byte(authenticationAAD),
	)
	require.NoError(t, err)

	_, err = DecryptFor(
		cipher,
		encrypted,
		devicetypes.ComponentTypeCompute,
	)
	require.ErrorContains(t, err, "unmarshal firmware authentication data")
}

func TestEmptyAuthenticationData(t *testing.T) {
	tests := []struct {
		name               string
		authenticationData *pb.FirmwareAuthenticationData
	}{
		{name: "absent"},
		{name: "empty shared", authenticationData: sharedAuthenticationData("")},
		{
			name: "empty per-component",
			authenticationData: perComponentAuthenticationData(
				proto.String(""), nil, nil,
			),
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			encrypted, err := Encrypt(nil, tt.authenticationData, nil)
			require.NoError(t, err)
			require.Nil(t, encrypted)
		})
	}

	got, err := DecryptFor(nil, nil, devicetypes.ComponentTypeCompute)
	require.NoError(t, err)
	require.Empty(t, got)
}

func sharedAuthenticationData(value string) *pb.FirmwareAuthenticationData {
	return &pb.FirmwareAuthenticationData{
		Value: &pb.FirmwareAuthenticationData_Shared{Shared: value},
	}
}

func perComponentAuthenticationData(
	compute, nvswitch, powershelf *string,
) *pb.FirmwareAuthenticationData {
	return &pb.FirmwareAuthenticationData{
		Value: &pb.FirmwareAuthenticationData_PerComponent{
			PerComponent: &pb.PerComponentFirmwareAuthenticationData{
				Compute:    compute,
				Nvswitch:   nvswitch,
				Powershelf: powershelf,
			},
		},
	}
}

func newTestCipher(t *testing.T, fill byte) *secret.Cipher {
	t.Helper()

	key := make([]byte, 32)
	for idx := range key {
		key[idx] = fill
	}
	cipher, err := secret.NewCipher(base64.StdEncoding.EncodeToString(key))
	require.NoError(t, err)
	return cipher
}
