// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwareauth

import (
	"fmt"

	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// Purpose separation prevents substitution with download-authentication ciphertext.
// Like download authentication, this envelope relies on persistence authorization
// and integrity for protection against relocation between firmware tasks.
const deviceCredentialsAAD = "flow:firmware-device-credentials"

// EncryptDeviceCredentials validates complete login pairs and protects them before
// task persistence or Temporal dispatch. Missing overrides remain absent.
func EncryptDeviceCredentials(cipher *secret.Cipher, credentials *pb.DeviceCredentialOverrides) (*secret.EncryptedData, error) {
	if credentials == nil {
		return nil, nil
	}
	err := validateDeviceCredentials(credentials)
	if err != nil {
		return nil, &InvalidDataError{err: err}
	}
	if cipher == nil {
		return nil, ErrDataCipherNotConfigured
	}
	plaintext, err := proto.MarshalOptions{Deterministic: true}.Marshal(credentials)
	if err != nil {
		return nil, fmt.Errorf("encode device credential overrides")
	}
	return cipher.EncryptData(plaintext, []byte(deviceCredentialsAAD))
}

func validateDeviceCredentials(credentials *pb.DeviceCredentialOverrides) error {
	if credentials.Bmc == nil && credentials.Nvos == nil {
		return fmt.Errorf("device credential overrides require bmc or nvos")
	}
	for _, pair := range []*pb.DeviceLogin{credentials.Bmc, credentials.Nvos} {
		if pair != nil && (pair.Username == "" || pair.Password == "") {
			return fmt.Errorf("device credentials require a nonempty username and password")
		}
	}
	return nil
}

// DecryptDeviceCredentials opens the envelope only inside an activity and builds
// Core's request. Decode errors never contain plaintext credential values.
func DecryptDeviceCredentials(cipher *secret.Cipher, encrypted *secret.EncryptedData) (*corev1.ComponentCredentialOverrides, error) {
	if encrypted == nil {
		return nil, nil
	}
	if cipher == nil {
		return nil, ErrDataCipherNotConfigured
	}
	plaintext, err := cipher.DecryptData(encrypted, []byte(deviceCredentialsAAD))
	if err != nil {
		return nil, fmt.Errorf("cannot decrypt device credential overrides")
	}
	var credentials pb.DeviceCredentialOverrides
	err = proto.Unmarshal(plaintext, &credentials)
	if err != nil {
		return nil, fmt.Errorf("cannot decode device credential overrides")
	}
	err = validateDeviceCredentials(&credentials)
	if err != nil {
		return nil, err
	}
	result := &corev1.ComponentCredentialOverrides{}
	if credentials.Bmc != nil {
		result.Bmc = &corev1.UsernamePassword{Username: credentials.Bmc.Username, Password: credentials.Bmc.Password}
	}
	if credentials.Nvos != nil {
		result.Nvos = &corev1.UsernamePassword{Username: credentials.Nvos.Username, Password: credentials.Nvos.Password}
	}
	return result, nil
}
