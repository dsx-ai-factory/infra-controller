// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"os"
	"reflect"
	"testing"

	cconfig "github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/internal/config"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	temporalClient "go.temporal.io/sdk/client"
)

func TestNewClientPool(t *testing.T) {
	type args struct {
		tcfg *cconfig.TemporalConfig
	}

	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)

	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)

	tests := []struct {
		name string
		args args
		want *ClientPool
	}{
		{
			name: "test Site client pool initializer",
			args: args{
				tcfg: tcfg,
			},
			want: &ClientPool{
				tcfg:        tcfg,
				IDClientMap: map[string]temporalClient.Client{},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewClientPool(tt.args.tcfg); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewSitePool() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestClientPool_GetClientByID(t *testing.T) {
	type fields struct {
		tcfg *cconfig.TemporalConfig
	}
	type args struct {
		siteID uuid.UUID
	}

	keyPath, certPath := config.SetupTestCerts(t)
	defer os.Remove(keyPath)
	defer os.Remove(certPath)

	cfg := config.NewConfig()
	cfg.SetTemporalCertPath(certPath)
	cfg.SetTemporalKeyPath(keyPath)
	cfg.SetTemporalCaPath(certPath)

	tcfg, err := cfg.GetTemporalConfig()
	assert.NoError(t, err)

	tests := []struct {
		name    string
		fields  fields
		args    args
		want    temporalClient.Client
		wantErr bool
	}{
		{
			name: "test retrieving client for given site ID",
			fields: fields{
				tcfg: tcfg,
			},
			args: args{
				siteID: uuid.New(),
			},
			want:    nil,
			wantErr: false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cp := NewClientPool(tt.fields.tcfg)
			_, err := cp.GetClientByID(tt.args.siteID)
			if (err != nil) != tt.wantErr {
				t.Errorf("ClientPool.GetClientByID() error = %v, wantErr %v", err, tt.wantErr)
				return
			}
		})
	}
}
