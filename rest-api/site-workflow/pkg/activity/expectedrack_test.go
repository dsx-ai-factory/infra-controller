// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/stretchr/testify/assert"
)

func TestManageExpectedRack_CreateExpectedRackOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedRack
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test create expected rack success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRack{
					RackId:        &corev1.RackId{Id: "test-rack-001"},
					RackProfileId: &corev1.RackProfileId{Id: "test-rack-profile-001"},
				},
			},
			wantErr: false,
		},
		{
			name: "test create expected rack fail on missing rack_id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRack{
					RackId:        nil,
					RackProfileId: &corev1.RackProfileId{Id: "test-rack-profile-001"},
				},
			},
			wantErr: true,
		},
		{
			name: "create without caller profile",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRack{
					RackId:        &corev1.RackId{Id: "test-rack-002"},
					RackProfileId: nil,
				},
			},
			wantErr: false,
		},
		{
			name: "test create expected rack fail on missing request",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: nil,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mer := NewManageExpectedRack(tt.fields.coreGrpcAtomicClient)
			err := mer.CreateExpectedRackOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedRack_UpdateExpectedRackOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedRack
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test update expected rack success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRack{
					RackId:        &corev1.RackId{Id: "test-update-rack-001"},
					RackProfileId: &corev1.RackProfileId{Id: "test-update-rack-profile-001"},
				},
			},
			wantErr: false,
		},
		{
			name: "test update expected rack fail on missing rack_id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRack{
					RackId:        nil,
					RackProfileId: &corev1.RackProfileId{Id: "test-update-rack-profile-001"},
				},
			},
			wantErr: true,
		},
		{
			name: "update without caller profile",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRack{
					RackId:        &corev1.RackId{Id: "test-update-rack-002"},
					RackProfileId: nil,
				},
			},
			wantErr: false,
		},
		{
			name: "test update expected rack fail on missing request",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: nil,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mer := NewManageExpectedRack(tt.fields.coreGrpcAtomicClient)
			err := mer.UpdateExpectedRackOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedRack_DeleteExpectedRackOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedRackRequest
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test delete expected rack success",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRackRequest{
					RackId: "test-delete-rack-001",
				},
			},
			wantErr: false,
		},
		{
			name: "test delete expected rack fail on empty rack_id",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRackRequest{
					RackId: "",
				},
			},
			wantErr: true,
		},
		{
			name: "test delete expected rack fail on missing request",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: nil,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mer := NewManageExpectedRack(tt.fields.coreGrpcAtomicClient)
			err := mer.DeleteExpectedRackOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedRack_ReplaceAllExpectedRacksOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	type fields struct {
		coreGrpcAtomicClient *cClient.CoreGrpcAtomicClient
	}
	type args struct {
		ctx     context.Context
		request *corev1.ExpectedRackList
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		wantErr bool
	}{
		{
			name: "test replace all expected racks success with empty list",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: &corev1.ExpectedRackList{},
			},
			wantErr: false,
		},
		{
			name: "test replace all expected racks success with valid list",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx: context.Background(),
				request: &corev1.ExpectedRackList{
					ExpectedRacks: []*corev1.ExpectedRack{
						{
							RackId:        &corev1.RackId{Id: "test-replace-rack-001"},
							RackProfileId: &corev1.RackProfileId{Id: "test-replace-rack-profile-001"},
						},
						{
							RackId:        &corev1.RackId{Id: "test-replace-rack-002"},
							RackProfileId: &corev1.RackProfileId{Id: "test-replace-rack-profile-002"},
						},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test replace all expected racks fail on missing request",
			fields: fields{
				coreGrpcAtomicClient: coreGrpcAtomicClient,
			},
			args: args{
				ctx:     context.Background(),
				request: nil,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mer := NewManageExpectedRack(tt.fields.coreGrpcAtomicClient)
			err := mer.ReplaceAllExpectedRacksOnSite(tt.args.ctx, tt.args.request)
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestManageExpectedRack_DeleteAllExpectedRacksOnSite(t *testing.T) {
	mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()

	coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
	coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

	mer := NewManageExpectedRack(coreGrpcAtomicClient)
	err := mer.DeleteAllExpectedRacksOnSite(context.Background())
	assert.NoError(t, err)
}

func TestManageExpectedRack_CreateExpectedRackOnFlow(t *testing.T) {
	manager := NewManageExpectedRack(nil)

	assert.NoError(t, manager.CreateExpectedRackOnFlow(context.Background(), nil))
	assert.NoError(t, manager.CreateExpectedRackOnFlow(context.Background(), &corev1.ExpectedRack{}))
}
