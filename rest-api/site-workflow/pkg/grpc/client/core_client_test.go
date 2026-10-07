// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package client

import (
	"crypto/md5"
	"os"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestCoreGrpcAtomicClient_GetInitialCertMD5(t *testing.T) {
	// Generate files for MD5 hash testing
	// clientCertBytes := []byte("new test cert file")
	clientCertPath := "/tmp/tls.crt"
	// serverCABytes := []byte("new test ca file")
	serverCAPath := "/tmp/ca.crt"

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
		Config *CoreGrpcClientConfig
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
				Config: &CoreGrpcClientConfig{
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
			cac := &CoreGrpcAtomicClient{
				Config: tt.fields.Config,
			}
			gotClientCertMD5, gotServerCAMD5, err := cac.GetInitialCertMD5()
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

func TestCoreGrpcAtomicClient_GetClient_ReturnsNilWhenUninitialized(t *testing.T) {
	cac := &CoreGrpcAtomicClient{
		value: &atomic.Value{},
	}
	// GetClient should return nil without panicking when no client has been stored
	assert.Nil(t, cac.GetClient())
}

func TestCoreGrpcAtomicClient_GetClient_ReturnsClientAfterSwap(t *testing.T) {
	cac := &CoreGrpcAtomicClient{
		value: &atomic.Value{},
	}
	// Simulate storing a client via SwapClient
	testClient := &CoreGrpcClient{}
	cac.value.Store(testClient)
	assert.Equal(t, testClient, cac.GetClient())
}

func TestCoreGrpcAtomicClient_SwapClient(t *testing.T) {
	first := &CoreGrpcClient{}
	second := &CoreGrpcClient{}

	tests := []struct {
		name string
		// seed holds the clients swapped in before the call under test, so each
		// case reaches its starting state without depending on another case.
		seed        []*CoreGrpcClient
		newClient   *CoreGrpcClient
		wantOld     *CoreGrpcClient
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
			seed:        []*CoreGrpcClient{first},
			newClient:   second,
			wantOld:     first,
			wantVersion: 2,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cac := &CoreGrpcAtomicClient{
				value: &atomic.Value{},
			}
			for _, seeded := range tt.seed {
				cac.SwapClient(seeded)
			}
			assert.Equal(t, tt.wantOld, cac.SwapClient(tt.newClient))
			assert.Equal(t, tt.wantVersion, cac.Version())
			assert.Equal(t, tt.newClient, cac.GetClient())
		})
	}
}

func TestCoreGrpcAtomicClient_CheckCertificates(t *testing.T) {
	// Generate files for MD5 hash testing
	// clientCertBytes := []byte("new test cert file")
	clientCertPath := "/tmp/tls.crt"
	// serverCABytes := []byte("new test ca file")
	serverCAPath := "/tmp/ca.crt"

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
		Config *CoreGrpcClientConfig
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
				Config: &CoreGrpcClientConfig{
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
				Config: &CoreGrpcClientConfig{
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
			cac := &CoreGrpcAtomicClient{
				Config: tt.fields.Config,
			}
			got, _, _, err := cac.CheckCertificates(tt.args.lastClientCertMD5, tt.args.lastServerCAMD5)
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

func TestUuidSliceToChunks(t *testing.T) {
	tests := []struct {
		name     string
		input    []*corev1.UUID
		expected [][]*corev1.UUID
	}{
		{
			name:     "empty",
			input:    []*corev1.UUID{},
			expected: [][]*corev1.UUID{},
		},
		{
			name: "no remainder",
			input: []*corev1.UUID{
				{Value: "uuid1"},
				{Value: "uuid2"},
				{Value: "uuid3"},
				{Value: "uuid4"},
			},
			expected: [][]*corev1.UUID{
				{
					&corev1.UUID{Value: "uuid1"},
					&corev1.UUID{Value: "uuid2"},
				},
				{
					&corev1.UUID{Value: "uuid3"},
					&corev1.UUID{Value: "uuid4"},
				},
			},
		},
		{
			name: "with remainder",
			input: []*corev1.UUID{
				{Value: "uuid1"},
				{Value: "uuid2"},
				{Value: "uuid3"},
				{Value: "uuid4"},
				{Value: "uuid5"},
			},
			expected: [][]*corev1.UUID{
				{
					&corev1.UUID{Value: "uuid1"},
					&corev1.UUID{Value: "uuid2"},
				},
				{
					&corev1.UUID{Value: "uuid3"},
					&corev1.UUID{Value: "uuid4"},
				},
				{
					&corev1.UUID{Value: "uuid5"},
				},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			output := SliceToChunks(tt.input, 2)

			assert.Equal(t, tt.expected, output)
		})
	}
}
