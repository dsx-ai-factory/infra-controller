// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"reflect"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
)

func TestNewAPIInfrastructureProvider(t *testing.T) {
	type args struct {
		dbip *cdbm.InfrastructureProvider
	}

	dbip := &cdbm.InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test-infrastructure-provider",
		DisplayName:    nil,
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Org Display name"),
		Created:        time.Now(),
		Updated:        time.Now(),
	}

	ipAPIInfrastructureProvider := APIInfrastructureProvider{
		ID:             dbip.ID.String(),
		Org:            dbip.Org,
		OrgDisplayName: dbip.OrgDisplayName,
		Created:        dbip.Created,
		Updated:        dbip.Updated,
	}

	tests := []struct {
		name string
		args args
		want *APIInfrastructureProvider
	}{
		{
			name: "test initializing API model for Infrastructure Provider",
			args: args{
				dbip: dbip,
			},
			want: &ipAPIInfrastructureProvider,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPIInfrastructureProvider(tt.args.dbip); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIInfrastructureProvider() = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestNewAPIInfrastructureProviderSummary(t *testing.T) {
	dbip := &cdbm.InfrastructureProvider{
		ID:             uuid.New(),
		Name:           "test-infrastructure-provider",
		DisplayName:    nil,
		Org:            "test-org",
		OrgDisplayName: cutil.GetPtr("Org Display name"),
		Created:        time.Now(),
		Updated:        time.Now(),
	}

	type args struct {
		dbip *cdbm.InfrastructureProvider
	}
	tests := []struct {
		name string
		args args
		want *APIInfrastructureProviderSummary
	}{
		{
			name: "test init API summary model for Infrastructure Provider",
			args: args{
				dbip: dbip,
			},
			want: &APIInfrastructureProviderSummary{
				Org:            dbip.Org,
				OrgDisplayName: dbip.OrgDisplayName,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := NewAPIInfrastructureProviderSummary(tt.args.dbip); !reflect.DeepEqual(got, tt.want) {
				t.Errorf("NewAPIInfrastructureProviderSummary() = %v, want %v", got, tt.want)
			}
		})
	}
}
