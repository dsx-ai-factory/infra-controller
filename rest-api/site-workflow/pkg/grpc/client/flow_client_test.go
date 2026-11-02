// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"crypto/md5"
	"os"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"
	"google.golang.org/grpc"

	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

func TestFlowAtomicClient_GetInitialCertMD5(t *testing.T) {
	// Generate files for MD5 hash testing
	clientCertPath := "/tmp/flow_tls.crt"
	serverCAPath := "/tmp/flow_ca.crt"

	// Write the files to disk
	err := os.WriteFile(clientCertPath, []byte("new test cert file"), 0644)
	assert.NoError(t, err)

	err = os.WriteFile(serverCAPath, []byte("new test ca file"), 0644)
	assert.NoError(t, err)

	// Get the MD5 hashes of the files
	clientCertBytes, err := os.ReadFile(clientCertPath)
	assert.NoError(t, err)
	clientCertMD5Hash := md5.Sum(clientCertBytes)
	clientCertMD5 := clientCertMD5Hash[:]

	serverCABytes, err := os.ReadFile(serverCAPath)
	assert.NoError(t, err)
	serverCAMD5Hash := md5.Sum(serverCABytes)
	serverCAMD5 := serverCAMD5Hash[:]

	type fields struct {
		Config *FlowGrpcClientConfig
	}
	tests := []struct {
		name              string
		fields            fields
		wantClientCertMD5 []byte
		wantServerCAMD5   []byte
		wantErr           bool
	}{
		{
			name: "test that we can get the initial cert md5s",
			fields: fields{
				Config: &FlowGrpcClientConfig{
					ClientCertPath: clientCertPath,
					ServerCAPath:   serverCAPath,
				},
			},
			wantClientCertMD5: clientCertMD5,
			wantServerCAMD5:   serverCAMD5,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rac := &FlowGrpcAtomicClient{
				Config: tt.fields.Config,
			}
			gotClientCertMD5, gotServerCAMD5, err := rac.GetInitialCertMD5()
			if tt.wantErr {
				assert.Error(t, err)
				return
			} else {
				assert.NoError(t, err)
			}
			assert.Equal(t, tt.wantClientCertMD5, gotClientCertMD5)
			assert.Equal(t, tt.wantServerCAMD5, gotServerCAMD5)
		})
	}
}

func TestFlowGrpcAtomicClient_GrpcServiceClient_ReturnsErrWhenUninitialized(t *testing.T) {
	rac := &FlowGrpcAtomicClient{
		value: &atomic.Value{},
	}
	// GrpcServiceClient should return ErrFlowClientNotConnected when no client has been stored,
	// rather than panicking on a nil-pointer deref.
	grpcServiceClient, err := rac.GrpcServiceClient()
	assert.Nil(t, grpcServiceClient)
	assert.ErrorIs(t, err, ErrFlowGrpcClientNotConnected)
}

func TestFlowGrpcAtomicClient_GrpcServiceClient_ReturnsFlowAfterSwap(t *testing.T) {
	rac := &FlowGrpcAtomicClient{
		value: &atomic.Value{},
	}
	// Once a FlowClient with a populated grpcServiceClient field is stored, GrpcServiceClient
	// should return that exact inner client. We construct a stub via
	// flowv1.NewFlowClient(nil); it isn't usable for real RPCs but is a non-nil
	// flowv1.FlowClient interface value, which is all we need to exercise the
	// success path.
	expected := flowv1.NewFlowClient((*grpc.ClientConn)(nil))
	rac.value.Store(&FlowGrpcClient{grpcServiceClient: expected})
	grpcServiceClient, err := rac.GrpcServiceClient()
	assert.NoError(t, err)
	assert.NotNil(t, grpcServiceClient)
	assert.Equal(t, expected, grpcServiceClient)
}

func TestFlowGrpcAtomicClient_SwapClient(t *testing.T) {
	first := &FlowGrpcClient{}
	second := &FlowGrpcClient{}

	tests := []struct {
		name string
		// seed holds the clients swapped in before the call under test, so each
		// case reaches its starting state without depending on another case.
		seed        []*FlowGrpcClient
		newClient   *FlowGrpcClient
		wantOld     *FlowGrpcClient
		wantVersion int64
	}{
		{
			name: "test that the initial swap reports no previous client and still advances the version",
			// Initial creation is the normal path rather than a failure, even though
			// there is nothing to hand back.
			newClient:   first,
			wantOld:     nil,
			wantVersion: 1,
		},
		{
			name:        "test that a later swap returns the client it replaced",
			seed:        []*FlowGrpcClient{first},
			newClient:   second,
			wantOld:     first,
			wantVersion: 2,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fgac := &FlowGrpcAtomicClient{
				value: &atomic.Value{},
			}
			for _, seeded := range tt.seed {
				fgac.SwapClient(seeded)
			}
			assert.Equal(t, tt.wantOld, fgac.SwapClient(tt.newClient))
			assert.Equal(t, tt.wantVersion, fgac.Version())
			assert.Equal(t, tt.newClient, fgac.GetClient())
		})
	}
}

func TestFlowAtomicClient_CheckCertificates(t *testing.T) {
	// Generate files for MD5 hash testing
	clientCertPath := "/tmp/flow_tls.crt"
	serverCAPath := "/tmp/flow_ca.crt"

	// Write the files to disk
	err := os.WriteFile(clientCertPath, []byte("new test cert file"), 0644)
	assert.NoError(t, err)

	err = os.WriteFile(serverCAPath, []byte("new test ca file"), 0644)
	assert.NoError(t, err)

	// Get the MD5 hashes of the files
	clientCertBytes, err := os.ReadFile(clientCertPath)
	assert.NoError(t, err)
	clientCertMD5Hash := md5.Sum(clientCertBytes)
	newClientCertMD5 := clientCertMD5Hash[:]

	serverCABytes, err := os.ReadFile(serverCAPath)
	assert.NoError(t, err)
	serverCAMD5Hash := md5.Sum(serverCABytes)
	newServerCAMD5 := serverCAMD5Hash[:]

	val := md5.Sum([]byte("old test cert file"))
	lastClientCertMD5 := val[:]

	val = md5.Sum([]byte("old test ca file"))
	lastServerCAMD5 := val[:]

	type fields struct {
		Config *FlowGrpcClientConfig
	}
	type args struct {
		lastClientCertMD5 []byte
		lastServerCAMD5   []byte
	}
	tests := []struct {
		name    string
		fields  fields
		args    args
		want    bool
		wantErr bool
	}{
		{
			name: "test that check certificates returns true when the certificates have changed",
			fields: fields{
				Config: &FlowGrpcClientConfig{
					ClientCertPath: clientCertPath,
					ServerCAPath:   serverCAPath,
				},
			},
			args: args{
				lastClientCertMD5: lastClientCertMD5,
				lastServerCAMD5:   lastServerCAMD5,
			},
			want: true,
		},
		{
			name: "test that check certificates returns false when the certificates have not changed",
			fields: fields{
				Config: &FlowGrpcClientConfig{
					ClientCertPath: clientCertPath,
					ServerCAPath:   serverCAPath,
				},
			},
			args: args{
				lastClientCertMD5: newClientCertMD5,
				lastServerCAMD5:   newServerCAMD5,
			},
			want: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rac := &FlowGrpcAtomicClient{
				Config: tt.fields.Config,
			}
			got, _, _, err := rac.CheckCertificates(tt.args.lastClientCertMD5, tt.args.lastServerCAMD5)
			if tt.wantErr {
				assert.Error(t, err)
				return
			} else {
				assert.NoError(t, err)
			}
			assert.Equal(t, tt.want, got)
		})
	}
}
