// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"context"
	"encoding/base64"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/firmwareauth"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
)

func TestUpgradeFirmwareEncryptsAuthenticationDataBeforeSubmittingTask(t *testing.T) {
	cipher := newServiceTestCipher(t)
	manager := &firmwareTaskManager{}
	server := &FlowServerImpl{taskManager: manager, dataCipher: cipher}
	authenticationData := perComponentServiceAuthenticationData(
		proto.String("compute-token"), nil, nil,
	)
	targetVersion := "1.2.3"

	_, err := server.UpgradeFirmware(
		context.Background(),
		&pb.UpgradeFirmwareRequest{
			TargetSpec: &pb.OperationTargetSpec{
				Targets: &pb.OperationTargetSpec_Components{
					Components: &pb.ComponentTargets{
						Targets: []*pb.ComponentTarget{
							{
								Identifier: &pb.ComponentTarget_Id{
									Id: &pb.UUID{Id: uuid.NewString()},
								},
							},
						},
					},
				},
			},
			TargetVersion:        &targetVersion,
			AuthenticationData:   authenticationData,
			OverrideVersionCheck: true,
		},
	)

	require.NoError(t, err)
	require.NotNil(t, manager.request)
	require.NotContains(t, string(manager.request.Operation.Info), "compute-token")

	var info operations.FirmwareControlTaskInfo
	require.NoError(t, info.Unmarshal(manager.request.Operation.Info))
	require.True(t, info.OverrideVersionCheck)
	got, err := firmwareauth.DecryptFor(
		cipher,
		info.AuthenticationData,
		devicetypes.ComponentTypeCompute,
	)
	require.NoError(t, err)
	require.Equal(t, "compute-token", got)
}

func TestUpgradeFirmwareAuthenticationDataStatusCodes(t *testing.T) {
	tests := []struct {
		name               string
		cipher             *secret.Cipher
		authenticationData *pb.FirmwareAuthenticationData
		subTargets         []string
		wantCode           codes.Code
		wantSubmitted      bool
	}{
		{
			name:          "missing cipher without authentication",
			wantCode:      codes.OK,
			wantSubmitted: true,
		},
		{
			name:               "invalid input",
			cipher:             newServiceTestCipher(t),
			authenticationData: &pb.FirmwareAuthenticationData{},
			wantCode:           codes.InvalidArgument,
		},
		{
			name:               "missing cipher",
			authenticationData: sharedServiceAuthenticationData("token"),
			wantCode:           codes.FailedPrecondition,
		},
		{
			name:               "authentication with dpu-only subtargets",
			cipher:             newServiceTestCipher(t),
			authenticationData: sharedServiceAuthenticationData("token"),
			subTargets:         []string{"dpu"},
			wantCode:           codes.InvalidArgument,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			manager := &firmwareTaskManager{}
			server := &FlowServerImpl{
				taskManager: manager,
				dataCipher:  tt.cipher,
			}
			_, err := server.UpgradeFirmware(
				context.Background(),
				&pb.UpgradeFirmwareRequest{
					SubTargets: tt.subTargets,
					TargetSpec: &pb.OperationTargetSpec{
						Targets: &pb.OperationTargetSpec_Components{
							Components: &pb.ComponentTargets{
								Targets: []*pb.ComponentTarget{
									{
										Identifier: &pb.ComponentTarget_Id{
											Id: &pb.UUID{Id: uuid.NewString()},
										},
									},
								},
							},
						},
					},
					AuthenticationData: tt.authenticationData,
				},
			)
			require.Equal(t, tt.wantCode, status.Code(err))
			require.Equal(t, tt.wantSubmitted, manager.request != nil)
		})
	}
}

type firmwareTaskManager struct {
	request *operation.Request
}

func (*firmwareTaskManager) Start(context.Context) error { return nil }
func (*firmwareTaskManager) Stop(context.Context)        {}
func (m *firmwareTaskManager) SubmitTask(
	_ context.Context,
	req *operation.Request,
) ([]uuid.UUID, error) {
	m.request = req
	return []uuid.UUID{uuid.New()}, nil
}
func (*firmwareTaskManager) CancelTask(context.Context, uuid.UUID) error { return nil }

func newServiceTestCipher(t *testing.T) *secret.Cipher {
	t.Helper()

	key := make([]byte, 32)
	cipher, err := secret.NewCipher(base64.StdEncoding.EncodeToString(key))
	require.NoError(t, err)
	return cipher
}

func sharedServiceAuthenticationData(
	value string,
) *pb.FirmwareAuthenticationData {
	return &pb.FirmwareAuthenticationData{
		Value: &pb.FirmwareAuthenticationData_Shared{Shared: value},
	}
}

func perComponentServiceAuthenticationData(
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
