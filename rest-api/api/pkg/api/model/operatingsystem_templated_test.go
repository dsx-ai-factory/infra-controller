// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestOperatingSystemCreateRequest_GetOperatingSystemType(t *testing.T) {
	assert.Equal(t, cdbm.OperatingSystemTypeIPXE, (&APIOperatingSystemCreateRequest{IpxeScript: cutil.GetPtr("x")}).GetOperatingSystemType())
	assert.Equal(t, cdbm.OperatingSystemTypeTemplatedIPXE, (&APIOperatingSystemCreateRequest{IpxeTemplateId: cutil.GetPtr("t")}).GetOperatingSystemType())
	assert.Equal(t, cdbm.OperatingSystemTypeImage, (&APIOperatingSystemCreateRequest{ImageURL: cutil.GetPtr("http://x")}).GetOperatingSystemType())
}

func TestOperatingSystemCreateRequest_Validate_Templated(t *testing.T) {
	tmplID := cutil.GetPtr(uuid.NewString())
	siteIDs := []string{uuid.NewString()}
	tests := []struct {
		desc      string
		obj       APIOperatingSystemCreateRequest
		expectErr bool
	}{
		{
			desc:      "templated requires exactly one siteId",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID},
			expectErr: true,
		},
		{
			desc:      "templated with one siteId is ok",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID, SiteIDs: siteIDs},
			expectErr: false,
		},
		{
			desc:      "templated with multiple siteIds is rejected",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID, SiteIDs: []string{uuid.NewString(), uuid.NewString()}},
			expectErr: true,
		},
		{
			desc:      "templated with non-UUID ipxeTemplateId is rejected",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: cutil.GetPtr("not-a-uuid"), SiteIDs: siteIDs},
			expectErr: true,
		},
		{
			desc:      "templated with non-UUID siteId is rejected",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID, SiteIDs: []string{"not-a-uuid"}},
			expectErr: true,
		},
		{
			desc:      "templated artifact with valid cache strategy is ok",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID, SiteIDs: siteIDs, IpxeTemplateArtifacts: APIOperatingSystemIpxeArtifacts{{Name: "kernel", URL: "http://x/k", CacheStrategy: cdbm.OperatingSystemIpxeArtifactCacheStrategyCacheAsNeeded}}},
			expectErr: false,
		},
		{
			desc:      "templated artifact with invalid cache strategy is rejected",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID, SiteIDs: siteIDs, IpxeTemplateArtifacts: APIOperatingSystemIpxeArtifacts{{Name: "kernel", URL: "http://x/k", CacheStrategy: "BOGUS"}}},
			expectErr: true,
		},
		{
			desc:      "templated artifact missing url is rejected",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeTemplateId: tmplID, SiteIDs: siteIDs, IpxeTemplateArtifacts: APIOperatingSystemIpxeArtifacts{{Name: "kernel", CacheStrategy: cdbm.OperatingSystemIpxeArtifactCacheStrategyCacheAsNeeded}}},
			expectErr: true,
		},
		{
			desc:      "raw ipxe rejects template parameters",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeScript: cutil.GetPtr("ipxe"), IpxeTemplateParameters: APIOperatingSystemIpxeParameters{{Name: "p", Value: "v"}}},
			expectErr: true,
		},
		{
			desc:      "raw ipxe rejects siteIds",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeScript: cutil.GetPtr("ipxe"), SiteIDs: siteIDs},
			expectErr: true,
		},
		{
			desc:      "raw ipxe without siteIds is ok",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeScript: cutil.GetPtr("ipxe")},
			expectErr: false,
		},
		{
			desc:      "ipxeScript and ipxeTemplateId mutually exclusive",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", IpxeScript: cutil.GetPtr("ipxe"), IpxeTemplateId: tmplID},
			expectErr: true,
		},
		{
			desc:      "image with siteId is ok",
			obj:       APIOperatingSystemCreateRequest{Name: "abc", ImageURL: cutil.GetPtr("http://iso.net/iso"), ImageSHA: cutil.GetPtr("a1efca12ea51069abb123bf9c77889fcc2a31cc5483fc14d115e44fdf07c7980"), RootFsID: cutil.GetPtr("666c2eee-193d-42db-a490-4c444342bd4e"), SiteIDs: siteIDs},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
		})
	}
}

func TestOperatingSystemUpdateRequest_Validate_Template(t *testing.T) {
	templatedOS := &cdbm.OperatingSystem{ID: uuid.New(), Name: "ab", Type: cdbm.OperatingSystemTypeTemplatedIPXE, Status: cdbm.OperatingSystemStatusReady}
	rawIpxeOS := &cdbm.OperatingSystem{ID: uuid.New(), Name: "ab", Type: cdbm.OperatingSystemTypeIPXE, IpxeScript: cutil.GetPtr("x"), Status: cdbm.OperatingSystemStatusReady}

	t.Run("templated accepts template params", func(t *testing.T) {
		err := (&APIOperatingSystemUpdateRequest{IpxeTemplateParameters: &APIOperatingSystemIpxeParameters{{Name: "p", Value: "v"}}}).Validate(templatedOS)
		assert.NoError(t, err)
	})
	t.Run("raw ipxe rejects template params", func(t *testing.T) {
		err := (&APIOperatingSystemUpdateRequest{IpxeTemplateParameters: &APIOperatingSystemIpxeParameters{{Name: "p", Value: "v"}}}).Validate(rawIpxeOS)
		assert.Error(t, err)
	})
	t.Run("raw ipxe rejects template id", func(t *testing.T) {
		err := (&APIOperatingSystemUpdateRequest{IpxeTemplateId: cutil.GetPtr("t")}).Validate(rawIpxeOS)
		assert.Error(t, err)
	})
	t.Run("templated accepts valid UUID template id", func(t *testing.T) {
		err := (&APIOperatingSystemUpdateRequest{IpxeTemplateId: cutil.GetPtr(uuid.NewString())}).Validate(templatedOS)
		assert.NoError(t, err)
	})
	t.Run("templated rejects non-UUID template id", func(t *testing.T) {
		err := (&APIOperatingSystemUpdateRequest{IpxeTemplateId: cutil.GetPtr("not-a-uuid")}).Validate(templatedOS)
		assert.Error(t, err)
	})
}

func TestOperatingSystemRequest_ToProto(t *testing.T) {
	id := uuid.New()
	tenantID := uuid.New()
	authToken := "secret-token"
	createRequest := &APIOperatingSystemCreateRequest{}
	updateRequest := &APIOperatingSystemUpdateRequest{}
	os := &cdbm.OperatingSystem{
		ID:               id,
		Name:             "templated-os",
		Description:      cutil.GetPtr("desc"),
		Org:              "org-1",
		TenantID:         &tenantID,
		Type:             cdbm.OperatingSystemTypeTemplatedIPXE,
		IsActive:         true,
		AllowOverride:    true,
		PhoneHomeEnabled: true,
		UserData:         cutil.GetPtr("ud"),
		IpxeTemplateId:   cutil.GetPtr("tmpl-1"),
		IpxeTemplateParameters: []cdbm.OperatingSystemIpxeParameter{
			{Name: "version", Value: "22.04"},
		},
		IpxeTemplateArtifacts: []cdbm.OperatingSystemIpxeArtifact{
			{Name: "kernel", URL: "http://x/k", AuthType: cutil.GetPtr(cdbm.OperatingSystemAuthTypeBearer), AuthToken: &authToken, CacheStrategy: cdbm.OperatingSystemIpxeArtifactCacheStrategyCacheAsNeeded},
		},
		IpxeTemplateDefinitionHash: cutil.GetPtr("hash-1"),
	}

	t.Run("tenant-owned create request maps all fields and tenant org", func(t *testing.T) {
		req := createRequest.ToProto(os)
		require.NotNil(t, req)
		assert.Equal(t, id.String(), req.GetId().GetValue())
		assert.Equal(t, "templated-os", req.Name)
		require.NotNil(t, req.TenantOrganizationId)
		assert.Equal(t, "org-1", req.GetTenantOrganizationId())
		assert.True(t, req.IsActive)
		assert.True(t, req.AllowOverride)
		assert.True(t, req.PhoneHomeEnabled)
		assert.Equal(t, "tmpl-1", req.GetIpxeTemplateId().GetValue())
		require.Len(t, req.IpxeTemplateParameters, 1)
		assert.Equal(t, "version", req.IpxeTemplateParameters[0].Name)
		require.Len(t, req.IpxeTemplateArtifacts, 1)
		assert.Equal(t, "kernel", req.IpxeTemplateArtifacts[0].Name)
		// Cache strategy maps from the friendly name to the proto enum.
		assert.Equal(t, corev1.IpxeTemplateArtifactCacheStrategy_CACHE_AS_NEEDED, req.IpxeTemplateArtifacts[0].CacheStrategy)
		// CachedUrl is never emitted from the rest side.
		assert.Nil(t, req.IpxeTemplateArtifacts[0].CachedUrl)
	})

	t.Run("provider-owned create request omits tenant org", func(t *testing.T) {
		providerOS := *os
		providerID := uuid.New()
		providerOS.TenantID = nil
		providerOS.InfrastructureProviderID = &providerID

		req := createRequest.ToProto(&providerOS)
		require.NotNil(t, req)
		assert.Nil(t, req.TenantOrganizationId)
	})

	t.Run("update request maps all fields", func(t *testing.T) {
		req := updateRequest.ToProto(os)
		require.NotNil(t, req)
		assert.Equal(t, id.String(), req.GetId().GetValue())
		require.NotNil(t, req.Name)
		assert.Equal(t, "templated-os", *req.Name)
		assert.Equal(t, "tmpl-1", req.GetIpxeTemplateId().GetValue())
		require.NotNil(t, req.IpxeTemplateParameters)
		require.Len(t, req.IpxeTemplateParameters.Items, 1)
		require.NotNil(t, req.IpxeTemplateArtifacts)
		require.Len(t, req.IpxeTemplateArtifacts.Items, 1)
		require.NotNil(t, req.IpxeTemplateDefinitionHash)
		assert.Equal(t, "hash-1", *req.IpxeTemplateDefinitionHash)
	})

	t.Run("delete request maps id", func(t *testing.T) {
		req := os.ToDeletionRequestProto()
		require.NotNil(t, req)
		assert.Equal(t, id.String(), req.GetId().GetValue())
	})
}

func TestNewAPIOperatingSystem_RedactsArtifactAuthToken(t *testing.T) {
	authToken := "super-secret"
	dbOS := &cdbm.OperatingSystem{
		ID:             uuid.New(),
		Name:           "templated",
		Org:            "org-1",
		Type:           cdbm.OperatingSystemTypeTemplatedIPXE,
		IpxeTemplateId: cutil.GetPtr("tmpl-1"),
		IpxeTemplateArtifacts: []cdbm.OperatingSystemIpxeArtifact{
			{Name: "kernel", URL: "http://x/k", AuthType: cutil.GetPtr(cdbm.OperatingSystemAuthTypeBearer), AuthToken: &authToken, CacheStrategy: cdbm.OperatingSystemIpxeArtifactCacheStrategyCacheAsNeeded},
		},
	}

	api := NewAPIOperatingSystem(dbOS, nil, nil, nil)
	require.NotNil(t, api)
	require.Len(t, api.IpxeTemplateArtifacts, 1)
	assert.Equal(t, "kernel", api.IpxeTemplateArtifacts[0].Name)

	// The response artifact type has no AuthToken field, so the serialized
	// response can never carry the stored secret (structural redaction).
	blob, err := json.Marshal(api)
	require.NoError(t, err)
	assert.NotContains(t, string(blob), "authToken", "serialized response must not contain an authToken field")
	assert.NotContains(t, string(blob), "super-secret", "serialized response must not contain the stored secret")

	// The source DB object must not be mutated when building the response.
	require.NotNil(t, dbOS.IpxeTemplateArtifacts[0].AuthToken)
	assert.Equal(t, "super-secret", *dbOS.IpxeTemplateArtifacts[0].AuthToken)
}
