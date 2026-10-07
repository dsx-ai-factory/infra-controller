// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"encoding/base64"
	"errors"
	"testing"

	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/firmwareauth"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestActivitiesReturnErrorWhenComponentManagerRegistryIsMissing(t *testing.T) {
	acts := New(nil, nil, nil, nil)

	for name, call := range activityCallsForMissingManagerTest(t, acts) {
		t.Run(name, func(t *testing.T) {
			err := call()
			require.Error(t, err)
			require.True(t, errors.Is(err, componentmanager.ErrRegistryNotConfigured))
		})
	}
}

func TestActivitiesReturnErrorWhenComponentManagerIsMissing(t *testing.T) {
	registry, err := componentmanager.NewRegistry(
		nil,
		cmconfig.Config{},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	acts := New(nil, nil, registry, nil)

	for name, call := range activityCallsForMissingManagerTest(t, acts) {
		t.Run(name, func(t *testing.T) {
			err := call()
			require.Error(t, err)
			require.True(t, errors.Is(err, componentmanager.ErrManagerNotConfigured))
		})
	}
}

func TestActivitiesReturnUnsupportedCapabilityBeforeCallingManager(t *testing.T) {
	for name, call := range capabilityCheckedActivityCalls(t) {
		t.Run(name, func(t *testing.T) {
			acts, manager := newCapabilityTestActivities(t)

			err := call(acts)

			require.Error(t, err)
			require.True(t, errors.Is(err, componentmanager.ErrUnsupportedCapability))
			require.False(t, manager.called)
		})
	}
}

func TestActivityCallsManagerWhenCapabilityIsSupported(t *testing.T) {
	acts, manager := newCapabilityTestActivities(
		t,
		capability.CapabilityPowerControl,
	)

	err := acts.PowerControl(
		context.Background(),
		newActivityTestTarget(),
		operations.PowerControlTaskInfo{
			Operation: operations.PowerOperationPowerOn,
		},
	)

	require.NoError(t, err)
	require.True(t, manager.called)
}

func TestFirmwareControlDataCipherAvailability(t *testing.T) {
	cipher := newActivityTestCipher(t)
	encrypted, err := firmwareauth.Encrypt(
		cipher,
		&pb.FirmwareAuthenticationData{
			Value: &pb.FirmwareAuthenticationData_PerComponent{
				PerComponent: &pb.PerComponentFirmwareAuthenticationData{
					Compute:  proto.String("compute-token"),
					Nvswitch: proto.String("switch-token"),
				},
			},
		},
		nil,
	)
	require.NoError(t, err)

	tests := []struct {
		name               string
		dataCipher         *secret.Cipher
		authenticationData *secret.EncryptedData
		wantAccessToken    string
		wantUnavailable    bool
		wantManagerCall    bool
	}{
		{
			name:               "configured cipher decrypts authentication data",
			dataCipher:         cipher,
			authenticationData: encrypted,
			wantAccessToken:    "compute-token",
			wantManagerCall:    true,
		},
		{
			name:            "missing cipher allows operation without authentication data",
			wantManagerCall: true,
		},
		{
			name:               "missing cipher rejects encrypted authentication data",
			authenticationData: encrypted,
			wantUnavailable:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			acts, manager := newCapabilityTestActivities(
				t,
				capability.CapabilityFirmwareControl,
			)
			acts.dataCipher = tt.dataCipher

			err := acts.FirmwareControl(
				context.Background(),
				newActivityTestTarget(),
				operations.FirmwareControlTaskInfo{
					Operation:          operations.FirmwareOperationUpgrade,
					AuthenticationData: tt.authenticationData,
				},
			)

			if tt.wantUnavailable {
				require.ErrorIs(t, err, firmwareauth.ErrDataCipherNotConfigured)
			} else {
				require.NoError(t, err)
			}
			require.Equal(t, tt.wantManagerCall, manager.called)
			require.Equal(t, tt.wantAccessToken, manager.firmwareInfo.AccessToken)
		})
	}
}

func TestActivityRequiresOperationInterfaceAfterCapabilityValidation(t *testing.T) {
	acts := newDescriptorOnlyActivities(
		t,
		capability.CapabilityPowerStatus,
	)

	_, err := acts.GetPowerStatus(
		context.Background(),
		newActivityTestTarget(),
	)

	require.Error(t, err)
	require.False(t, errors.Is(err, componentmanager.ErrUnsupportedCapability))
	require.True(t, errors.Is(err, componentmanager.ErrCapabilityInterfaceNotImplemented))
	require.ErrorContains(
		t,
		err,
		`declares capability "PowerStatus" but does not implement its operation interface`,
	)
}

func activityCallsForMissingManagerTest(
	t *testing.T,
	acts *Activities,
) map[string]func() error {
	t.Helper()

	ctx := context.Background()
	target := newActivityTestTarget()

	return map[string]func() error{
		"InjectExpectation": func() error {
			return acts.InjectExpectation(
				ctx,
				target,
				operations.InjectExpectationTaskInfo{},
			)
		},
		"PowerControl": func() error {
			return acts.PowerControl(
				ctx,
				target,
				operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationPowerOn,
				},
			)
		},
		"GetPowerStatus": func() error {
			_, err := acts.GetPowerStatus(ctx, target)
			return err
		},
		"VerifyFirmwareConsistency": func() error {
			return acts.VerifyFirmwareConsistency(ctx, target)
		},
		"BringUpControl": func() error {
			return acts.BringUpControl(ctx, target, operations.BringUpTaskInfo{})
		},
		"GetBringUpStatus": func() error {
			_, err := acts.GetBringUpStatus(ctx, target)
			return err
		},
		"FirmwareControl": func() error {
			return acts.FirmwareControl(
				ctx,
				target,
				operations.FirmwareControlTaskInfo{
					Operation: operations.FirmwareOperationUpgrade,
				},
			)
		},
		"GetFirmwareStatus": func() error {
			_, err := acts.GetFirmwareStatus(ctx, target)
			return err
		},
	}
}

func capabilityCheckedActivityCalls(
	t *testing.T,
) map[string]func(*Activities) error {
	t.Helper()

	ctx := context.Background()
	target := newActivityTestTarget()

	return map[string]func(*Activities) error{
		"InjectExpectation": func(acts *Activities) error {
			return acts.InjectExpectation(
				ctx,
				target,
				operations.InjectExpectationTaskInfo{},
			)
		},
		"PowerControl": func(acts *Activities) error {
			return acts.PowerControl(
				ctx,
				target,
				operations.PowerControlTaskInfo{
					Operation: operations.PowerOperationPowerOn,
				},
			)
		},
		"GetPowerStatus": func(acts *Activities) error {
			_, err := acts.GetPowerStatus(ctx, target)
			return err
		},
		"VerifyFirmwareConsistency": func(acts *Activities) error {
			return acts.VerifyFirmwareConsistency(ctx, target)
		},
		"BringUpControl": func(acts *Activities) error {
			return acts.BringUpControl(ctx, target, operations.BringUpTaskInfo{})
		},
		"GetBringUpStatus": func(acts *Activities) error {
			_, err := acts.GetBringUpStatus(ctx, target)
			return err
		},
		"FirmwareControl": func(acts *Activities) error {
			return acts.FirmwareControl(
				ctx,
				target,
				operations.FirmwareControlTaskInfo{
					Operation: operations.FirmwareOperationUpgrade,
				},
			)
		},
		"GetFirmwareStatus": func(acts *Activities) error {
			_, err := acts.GetFirmwareStatus(ctx, target)
			return err
		},
	}
}

type capabilityTestManager struct {
	descriptor   cmcatalog.Descriptor
	called       bool
	firmwareInfo operations.FirmwareControlTaskInfo
}

func (m *capabilityTestManager) Descriptor() cmcatalog.Descriptor {
	return m.descriptor
}

func (m *capabilityTestManager) InjectExpectation(
	context.Context,
	common.Target,
	operations.InjectExpectationTaskInfo,
) error {
	m.called = true
	return nil
}

func (m *capabilityTestManager) PowerControl(
	context.Context,
	common.Target,
	operations.PowerControlTaskInfo,
) error {
	m.called = true
	return nil
}

func (m *capabilityTestManager) GetPowerStatus(
	context.Context,
	common.Target,
) (map[string]operations.PowerStatus, error) {
	m.called = true
	return nil, nil
}

func (m *capabilityTestManager) FirmwareControl(
	_ context.Context,
	_ common.Target,
	info operations.FirmwareControlTaskInfo,
) error {
	m.called = true
	m.firmwareInfo = info
	return nil
}

func (m *capabilityTestManager) GetFirmwareStatus(
	context.Context,
	common.Target,
) (map[string]operations.FirmwareUpdateStatus, error) {
	m.called = true
	return nil, nil
}

func newActivityTestCipher(t *testing.T) *secret.Cipher {
	t.Helper()

	key := make([]byte, 32)
	encodedKey := base64.StdEncoding.EncodeToString(key)
	cipher, err := secret.NewCipher(encodedKey)
	require.NoError(t, err)
	return cipher
}

func newCapabilityTestActivities(
	t *testing.T,
	capabilities ...capability.Capability,
) (*Activities, *capabilityTestManager) {
	t.Helper()

	descriptor := cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "limited",
		},
		Capabilities: capability.CapabilitySet(capabilities),
	}
	manager := &capabilityTestManager{descriptor: descriptor}
	registry, err := componentmanager.NewRegistry(
		[]componentmanager.FactorySpec{
			{
				Descriptor: descriptor,
				Factory: func(
					*providerapi.ProviderRegistry,
				) (componentmanager.ComponentManager, error) {
					return manager, nil
				},
			},
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "limited",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	return New(nil, nil, registry, nil), manager
}

type descriptorOnlyManager struct {
	descriptor cmcatalog.Descriptor
}

func (m descriptorOnlyManager) Descriptor() cmcatalog.Descriptor {
	return m.descriptor
}

func newDescriptorOnlyActivities(
	t *testing.T,
	capabilities ...capability.Capability,
) *Activities {
	t.Helper()

	descriptor := cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "descriptor-only",
		},
		Capabilities: capability.CapabilitySet(capabilities),
	}
	registry, err := componentmanager.NewRegistry(
		[]componentmanager.FactorySpec{
			{
				Descriptor: descriptor,
				Factory: func(
					*providerapi.ProviderRegistry,
				) (componentmanager.ComponentManager, error) {
					return descriptorOnlyManager{descriptor: descriptor}, nil
				},
			},
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "descriptor-only",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	return New(nil, nil, registry, nil)
}

func newActivityTestTarget() common.Target {
	return common.Target{
		Type:        devicetypes.ComponentTypeCompute,
		Identifiers: []string{"machine-1"},
	}
}
