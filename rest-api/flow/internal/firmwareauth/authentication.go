// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package firmwareauth protects and selects authentication data used for
// firmware downloads.
package firmwareauth

import (
	"errors"
	"fmt"

	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/firmwarecomponents"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

// authenticationAAD provides purpose-level domain separation. It does not
// bind an entire encrypted envelope to a database row or Temporal execution;
// persistence-layer authorization and integrity must prevent envelope replay
// or relocation between otherwise valid firmware operations.
const authenticationAAD = "flow:firmware-authentication"

// ErrDataCipherNotConfigured identifies an operation that requires firmware
// authentication encryption or decryption while Flow has no data cipher.
var ErrDataCipherNotConfigured = errors.New("data encryption cipher is not configured")

// InvalidDataError identifies authentication data rejected at the API
// boundary. Other errors returned by this package are internal failures.
type InvalidDataError struct {
	err error
}

func (e *InvalidDataError) Error() string {
	return e.err.Error()
}

func (e *InvalidDataError) Unwrap() error {
	return e.err
}

// IsInvalidData reports whether err represents invalid caller input.
func IsInvalidData(err error) bool {
	var target *InvalidDataError
	return errors.As(err, &target)
}

// Encrypt validates authenticationData against the firmware subtargets and
// encrypts it. An absent or explicitly empty value is represented by nil so
// callers can omit it from persisted payloads.
func Encrypt(
	cipher *secret.Cipher,
	authenticationData *pb.FirmwareAuthenticationData,
	subTargets []string,
) (*secret.EncryptedData, error) {
	empty, err := validate(authenticationData)
	if err != nil {
		return nil, &InvalidDataError{err: err}
	}
	if empty {
		return nil, nil
	}
	if firmwarecomponents.IsNICoDPUOnlySubTargets(subTargets) {
		return nil, &InvalidDataError{err: errors.New(
			"dpu-only firmware updates do not support authentication data",
		)}
	}
	if cipher == nil {
		return nil, ErrDataCipherNotConfigured
	}

	plaintext, err := proto.MarshalOptions{Deterministic: true}.Marshal(
		authenticationData,
	)
	if err != nil {
		return nil, fmt.Errorf("marshal firmware authentication data: %w", err)
	}

	encrypted, err := cipher.EncryptData(plaintext, []byte(authenticationAAD))
	if err != nil {
		return nil, fmt.Errorf("encrypt firmware authentication data: %w", err)
	}

	return encrypted, nil
}

// DecryptFor decrypts authentication data and selects the value for the target
// component type. A shared value applies to every supported component type. A
// missing per-component field means that type receives no authentication data.
func DecryptFor(
	cipher *secret.Cipher,
	encrypted *secret.EncryptedData,
	componentType devicetypes.ComponentType,
) (string, error) {
	if encrypted == nil {
		return "", nil
	}
	if cipher == nil {
		return "", ErrDataCipherNotConfigured
	}

	plaintext, err := cipher.DecryptData(encrypted, []byte(authenticationAAD))
	if err != nil {
		return "", fmt.Errorf("decrypt firmware authentication data: %w", err)
	}

	var authenticationData pb.FirmwareAuthenticationData
	if err := proto.Unmarshal(plaintext, &authenticationData); err != nil {
		return "", fmt.Errorf("unmarshal firmware authentication data: %w", err)
	}
	if _, err := validate(&authenticationData); err != nil {
		return "", fmt.Errorf("validate decrypted firmware authentication data: %w", err)
	}
	return selectForComponent(&authenticationData, componentType)
}

func validate(authenticationData *pb.FirmwareAuthenticationData) (bool, error) {
	if authenticationData == nil {
		return true, nil
	}

	switch value := authenticationData.GetValue().(type) {
	case *pb.FirmwareAuthenticationData_Shared:
		return value.Shared == "", nil
	case *pb.FirmwareAuthenticationData_PerComponent:
		if value.PerComponent == nil {
			return false, fmt.Errorf("per-component authentication data is required")
		}
		return value.PerComponent.GetCompute() == "" &&
			value.PerComponent.GetNvswitch() == "" &&
			value.PerComponent.GetPowershelf() == "", nil
	default:
		return false, fmt.Errorf("authentication data value is required")
	}
}

func selectForComponent(
	authenticationData *pb.FirmwareAuthenticationData,
	componentType devicetypes.ComponentType,
) (string, error) {
	var perComponentValue string
	switch componentType {
	case devicetypes.ComponentTypeCompute:
		perComponentValue = authenticationData.GetPerComponent().GetCompute()
	case devicetypes.ComponentTypeNVSwitch:
		perComponentValue = authenticationData.GetPerComponent().GetNvswitch()
	case devicetypes.ComponentTypePowerShelf:
		perComponentValue = authenticationData.GetPerComponent().GetPowershelf()
	default:
		return "", fmt.Errorf(
			"firmware authentication data does not support component type %q",
			devicetypes.ComponentTypeToString(componentType),
		)
	}

	switch value := authenticationData.GetValue().(type) {
	case *pb.FirmwareAuthenticationData_Shared:
		return value.Shared, nil
	case *pb.FirmwareAuthenticationData_PerComponent:
		return perComponentValue, nil
	default:
		return "", fmt.Errorf("firmware authentication data value is not configured")
	}
}
