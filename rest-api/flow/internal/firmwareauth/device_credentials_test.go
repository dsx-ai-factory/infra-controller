// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwareauth

import (
	"encoding/base64"
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestEncryptDeviceCredentials(t *testing.T) {
	cipher, err := secret.NewCipher(base64.StdEncoding.EncodeToString(make([]byte, 32)))
	require.NoError(t, err)
	cases := []struct {
		name        string
		cipher      *secret.Cipher
		credentials *pb.DeviceCredentialOverrides
		valid       bool
	}{
		{"absent", nil, nil, true},
		{"both pairs", cipher, &pb.DeviceCredentialOverrides{Bmc: &pb.DeviceLogin{Username: "bmc-user", Password: "bmc-secret-value"}, Nvos: &pb.DeviceLogin{Username: "nvos-user", Password: "nvos-secret-value"}}, true},
		{"missing key", nil, &pb.DeviceCredentialOverrides{Bmc: &pb.DeviceLogin{Username: "user", Password: "value"}}, false},
		{"empty", cipher, &pb.DeviceCredentialOverrides{}, false},
		{"incomplete", cipher, &pb.DeviceCredentialOverrides{Bmc: &pb.DeviceLogin{Username: "user"}}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			encrypted, err := EncryptDeviceCredentials(tc.cipher, tc.credentials)
			if !tc.valid {
				require.Error(t, err)
				return
			}
			require.NoError(t, err)
			decoded, err := DecryptDeviceCredentials(tc.cipher, encrypted)
			require.NoError(t, err)
			if tc.credentials == nil {
				require.Nil(t, decoded)
				return
			}
			require.Equal(t, tc.credentials.Bmc.Username, decoded.Bmc.Username)
			require.Equal(t, tc.credentials.Bmc.Password, decoded.Bmc.Password)
			require.Equal(t, tc.credentials.Nvos.Password, decoded.Nvos.Password)
			info := operations.FirmwareControlTaskInfo{DeviceCredentials: encrypted, CredentialOverrides: decoded}
			encoded, err := json.Marshal(info)
			require.NoError(t, err)
			require.NotContains(t, string(encoded), "bmc-secret-value")
			require.NotContains(t, string(encoded), "nvos-secret-value")
			var restored operations.FirmwareControlTaskInfo
			require.NoError(t, json.Unmarshal(encoded, &restored))
			decoded, err = DecryptDeviceCredentials(cipher, restored.DeviceCredentials)
			require.NoError(t, err)
			require.Equal(t, "nvos-secret-value", decoded.Nvos.Password)
		})
	}
}

func TestDecryptDeviceCredentials(t *testing.T) {
	cipher, err := secret.NewCipher(base64.StdEncoding.EncodeToString(make([]byte, 32)))
	require.NoError(t, err)
	otherKey := make([]byte, 32)
	otherKey[0] = 1
	other, err := secret.NewCipher(base64.StdEncoding.EncodeToString(otherKey))
	require.NoError(t, err)
	encrypted, err := EncryptDeviceCredentials(cipher, &pb.DeviceCredentialOverrides{Bmc: &pb.DeviceLogin{Username: "user", Password: "value"}})
	require.NoError(t, err)
	wrongPurpose, err := cipher.EncryptData([]byte("value"), []byte(authenticationAAD))
	require.NoError(t, err)
	malformed, err := cipher.EncryptData([]byte("malformed-credential-value"), []byte(deviceCredentialsAAD))
	require.NoError(t, err)
	tampered := *encrypted
	tampered.Ciphertext = append([]byte(nil), encrypted.Ciphertext...)
	tampered.Ciphertext[0] ^= 1
	for _, tc := range []struct {
		name   string
		cipher *secret.Cipher
		data   *secret.EncryptedData
	}{
		{"wrong key", other, encrypted}, {"wrong purpose", cipher, wrongPurpose}, {"tampered", cipher, &tampered}, {"malformed", cipher, malformed},
	} {
		t.Run(tc.name, func(t *testing.T) {
			_, err := DecryptDeviceCredentials(tc.cipher, tc.data)
			require.Error(t, err)
			require.NotContains(t, err.Error(), "malformed-credential-value")
		})
	}
}
