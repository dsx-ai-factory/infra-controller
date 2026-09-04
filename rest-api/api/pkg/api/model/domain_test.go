// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestAPIDomainCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIDomainCreateRequest
		wantErr bool
	}{
		{
			name:    "valid",
			request: APIDomainCreateRequest{Name: "tenant.example.com", SiteID: uuid.NewString()},
		},
		{
			name:    "missing name",
			request: APIDomainCreateRequest{SiteID: uuid.NewString()},
			wantErr: true,
		},
		{
			name:    "missing site ID",
			request: APIDomainCreateRequest{Name: "tenant.example.com"},
			wantErr: true,
		},
		{
			name:    "invalid site ID",
			request: APIDomainCreateRequest{Name: "tenant.example.com", SiteID: "not-a-uuid"},
			wantErr: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIDomainCreateRequest_ToProto(t *testing.T) {
	request := APIDomainCreateRequest{Name: "tenant.example.com", SiteID: uuid.NewString()}

	assert.Equal(t, request.Name, request.ToProto().GetName())
}

func TestAPIDomainGetAllRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIDomainGetAllRequest
		wantErr bool
	}{
		{name: "empty filters"},
		{name: "valid filters", request: APIDomainGetAllRequest{TenantID: uuid.NewString(), SiteID: uuid.NewString()}},
		{name: "invalid tenant ID", request: APIDomainGetAllRequest{TenantID: "invalid"}, wantErr: true},
		{name: "invalid site ID", request: APIDomainGetAllRequest{SiteID: "invalid"}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIDomainUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APIDomainUpdateRequest
		wantErr bool
	}{
		{name: "valid", request: APIDomainUpdateRequest{Name: "renamed.example.com"}},
		{name: "missing name", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			assert.Equal(t, tt.wantErr, err != nil)
		})
	}
}

func TestAPIDomainUpdateRequest_ToProto(t *testing.T) {
	controllerDomainID := uuid.New()
	request := APIDomainUpdateRequest{Name: "renamed.example.com", ControllerDomainID: controllerDomainID}

	got := request.ToProto().GetDomain()
	require.NotNil(t, got)
	assert.Equal(t, controllerDomainID.String(), got.GetId().GetValue())
	assert.Equal(t, request.Name, got.GetName())
}

func TestNewAPIDomain(t *testing.T) {
	localID := uuid.New()
	controllerID := uuid.New()
	tenantID := uuid.New()
	siteID := uuid.New()
	created := time.Now().UTC().Round(time.Microsecond)
	tests := []struct {
		name   string
		domain *cdbm.Domain
		check  func(*testing.T, *APIDomain)
	}{
		{
			name: "nil input",
			check: func(t *testing.T, got *APIDomain) {
				assert.Nil(t, got)
			},
		},
		{
			name: "owned projection uses REST local identity",
			domain: &cdbm.Domain{
				ID:                 localID,
				Hostname:           "tenant.example.com",
				TenantID:           &tenantID,
				SiteID:             &siteID,
				ControllerDomainID: &controllerID,
				Created:            created,
				Updated:            created,
			},
			check: func(t *testing.T, got *APIDomain) {
				require.NotNil(t, got)
				assert.Equal(t, localID.String(), got.ID)
				assert.NotEqual(t, controllerID.String(), got.ID)
				assert.Equal(t, tenantID.String(), got.TenantID)
				assert.Equal(t, siteID.String(), got.SiteID)
				assert.Equal(t, "tenant.example.com", got.Name)
				assert.Equal(t, created, got.Created)
				assert.Equal(t, created, got.Updated)
			},
		},
		{
			name:   "missing ownership remains explicit",
			domain: &cdbm.Domain{ID: localID},
			check: func(t *testing.T, got *APIDomain) {
				require.NotNil(t, got)
				assert.Empty(t, got.TenantID)
				assert.Empty(t, got.SiteID)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tt.check(t, NewAPIDomain(tt.domain))
		})
	}
}

func TestAPIDomain_MarshalJSON(t *testing.T) {
	encoded, err := json.Marshal(APIDomain{})
	require.NoError(t, err)

	var got map[string]any
	require.NoError(t, json.Unmarshal(encoded, &got))
	assert.Len(t, got, 6)
	for _, key := range []string{"id", "name", "tenantId", "siteId", "created", "updated"} {
		assert.Contains(t, got, key)
	}
}
