// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func testDhcpOptionSetSetupSchema(t *testing.T, dbSession *db.Session) {
	// Create tables
	err := dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	require.NoError(t, err)

	err = dbSession.DB.ResetModel(context.Background(), (*DhcpOptionSet)(nil))
	require.NoError(t, err)
}

func testDhcpOptions() *DhcpOptions {
	return &DhcpOptions{DhcpOptions: &corev1.DhcpOptions{
		V4: &corev1.DhcpOptionsV4{BootUrl: cutil.GetPtr("http://10.0.0.5/boot.ipxe"), DnsServers: []string{"10.0.0.53"}},
		V6: &corev1.DhcpOptionsV6{DnsServers: []string{"fd00::53"}},
	}}
}

func TestDhcpOptionSet_ToProto(t *testing.T) {
	id := uuid.New()

	tests := []struct {
		name string
		dos  DhcpOptionSet
		want *corev1.DhcpOptionSet
	}{
		{
			name: "all fields are mapped",
			dos: DhcpOptionSet{
				ID:          id,
				Name:        "pxe-boot",
				Description: cutil.GetPtr("provider boot"),
				TenantOrg:   "test-tenant-org",
				Labels:      Labels{"env": "prod"},
				Options:     testDhcpOptions(),
				Version:     "V1-T1",
			},
			want: &corev1.DhcpOptionSet{
				Id:                   &corev1.DhcpOptionSetId{Value: id.String()},
				TenantOrganizationId: "test-tenant-org",
				Metadata: &corev1.Metadata{
					Name:        "pxe-boot",
					Description: "provider boot",
					Labels:      []*corev1.Label{{Key: "env", Value: cutil.GetPtr("prod")}},
				},
				Options: testDhcpOptions().DhcpOptions,
				Version: "V1-T1",
			},
		},
		{
			name: "nil description and options are omitted",
			dos:  DhcpOptionSet{ID: id, Name: "pxe-boot", TenantOrg: "test-tenant-org"},
			want: &corev1.DhcpOptionSet{
				Id:                   &corev1.DhcpOptionSetId{Value: id.String()},
				TenantOrganizationId: "test-tenant-org",
				Metadata:             &corev1.Metadata{Name: "pxe-boot"},
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.True(t, proto.Equal(tt.want, tt.dos.ToProto()))
		})
	}
}

func TestDhcpOptionSet_FromProto(t *testing.T) {
	id := uuid.New()
	stored := func() DhcpOptionSet {
		return DhcpOptionSet{
			ID:          id,
			Name:        "pxe-boot",
			Description: cutil.GetPtr("provider boot"),
			TenantOrg:   "test-tenant-org",
			Labels:      Labels{"env": "prod"},
			Options:     testDhcpOptions(),
			Version:     "V1-T1",
		}
	}

	tests := []struct {
		name  string
		proto *corev1.DhcpOptionSet
		want  DhcpOptionSet
	}{
		{
			name:  "round trip restores all fields",
			proto: func() *corev1.DhcpOptionSet { dos := stored(); return dos.ToProto() }(),
			want:  stored(),
		},
		{
			name: "values the Site no longer reports are cleared and an invalid ID keeps the stored one",
			proto: &corev1.DhcpOptionSet{
				Id:                   &corev1.DhcpOptionSetId{Value: "not-a-uuid"},
				TenantOrganizationId: "test-tenant-org",
				Metadata:             &corev1.Metadata{Name: "renamed"},
			},
			want: DhcpOptionSet{ID: id, Name: "renamed", TenantOrg: "test-tenant-org", Version: "V1-T1"},
		},
		{
			name: "nil proto is a no-op",
			want: stored(),
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := stored()
			got.FromProto(tt.proto)
			assert.Equal(t, tt.want.ID, got.ID)
			assert.Equal(t, tt.want.Name, got.Name)
			assert.Equal(t, tt.want.Description, got.Description)
			assert.Equal(t, tt.want.TenantOrg, got.TenantOrg)
			assert.Equal(t, tt.want.Labels, got.Labels)
			assert.Equal(t, tt.want.Version, got.Version)
			if tt.want.Options == nil {
				assert.Nil(t, got.Options)
			} else {
				require.NotNil(t, got.Options)
				assert.True(t, proto.Equal(tt.want.Options.DhcpOptions, got.Options.DhcpOptions))
			}
		})
	}
}

func TestDhcpOptionSetSQLDAO_Create(t *testing.T) {
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDhcpOptionSetSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	dossd := NewDhcpOptionSetDAO(dbSession)
	ctx := context.Background()

	tests := []struct {
		name    string
		input   DhcpOptionSetCreateInput
		wantErr bool
	}{
		{
			name: "all fields are persisted",
			input: DhcpOptionSetCreateInput{
				DhcpOptionSetID: cutil.GetPtr(uuid.New()),
				Name:            "pxe-boot",
				Description:     cutil.GetPtr("provider boot"),
				TenantOrg:       tn.Org,
				SiteID:          st.ID,
				TenantID:        tn.ID,
				Options:         testDhcpOptions(),
				Labels:          map[string]string{"env": "prod"},
				Status:          DhcpOptionSetStatusPending,
				Version:         cutil.GetPtr("V1-T1"),
				CreatedBy:       tnu.ID,
			},
		},
		{
			name: "unknown tenant fails the foreign key",
			input: DhcpOptionSetCreateInput{
				Name:      "pxe-boot",
				TenantOrg: tn.Org,
				SiteID:    st.ID,
				TenantID:  uuid.New(),
				Status:    DhcpOptionSetStatusPending,
				CreatedBy: tnu.ID,
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, err := dossd.Create(ctx, nil, tt.input)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)

			stored, err := dossd.GetByID(ctx, nil, got.ID, []string{SiteRelationName, TenantRelationName})
			require.NoError(t, err)
			assert.Equal(t, *tt.input.DhcpOptionSetID, stored.ID)
			assert.Equal(t, tt.input.Name, stored.Name)
			assert.Equal(t, tt.input.Description, stored.Description)
			assert.Equal(t, Labels(tt.input.Labels), stored.Labels)
			assert.True(t, proto.Equal(tt.input.Options.DhcpOptions, stored.Options.DhcpOptions))
			assert.Equal(t, tt.input.Status, stored.Status)
			assert.Equal(t, *tt.input.Version, stored.Version)
			assert.False(t, stored.IsMissingOnSite)
			assert.NotNil(t, stored.Site)
			assert.NotNil(t, stored.Tenant)
		})
	}
}

func TestDhcpOptionSetSQLDAO_GetAll(t *testing.T) {
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDhcpOptionSetSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st1 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-1", "Test Site 1", ip.Org, ipu.ID)
	st2 := testBuildSite(t, dbSession, nil, ip.ID, "test-site-2", "Test Site 2", ip.Org, ipu.ID)

	dossd := NewDhcpOptionSetDAO(dbSession)
	ctx := context.Background()

	create := func(name string, siteID uuid.UUID, status DhcpOptionSetStatus) *DhcpOptionSet {
		dos, err := dossd.Create(ctx, nil, DhcpOptionSetCreateInput{
			Name: name, TenantOrg: tn.Org, SiteID: siteID, TenantID: tn.ID, Status: status, CreatedBy: tnu.ID,
		})
		require.NoError(t, err)
		return dos
	}
	a := create("a", st1.ID, DhcpOptionSetStatusReady)
	create("b", st1.ID, DhcpOptionSetStatusPending)
	create("c", st2.ID, DhcpOptionSetStatusReady)
	deleted := create("d", st1.ID, DhcpOptionSetStatusReady)
	require.NoError(t, dossd.Delete(ctx, nil, deleted.ID))

	tests := []struct {
		name      string
		filter    DhcpOptionSetFilterInput
		wantNames []string
	}{
		{name: "no filter returns all but deleted, ordered by created", wantNames: []string{"a", "b", "c"}},
		{name: "by site", filter: DhcpOptionSetFilterInput{SiteIDs: []uuid.UUID{st1.ID}}, wantNames: []string{"a", "b"}},
		{name: "by tenant", filter: DhcpOptionSetFilterInput{TenantIDs: []uuid.UUID{tn.ID}}, wantNames: []string{"a", "b", "c"}},
		{name: "by name", filter: DhcpOptionSetFilterInput{Names: []string{"c"}}, wantNames: []string{"c"}},
		{name: "by ID", filter: DhcpOptionSetFilterInput{DhcpOptionSetIDs: []uuid.UUID{a.ID}}, wantNames: []string{"a"}},
		{name: "by status", filter: DhcpOptionSetFilterInput{Statuses: []string{string(DhcpOptionSetStatusPending)}}, wantNames: []string{"b"}},
		{name: "by search query", filter: DhcpOptionSetFilterInput{SearchQuery: cutil.GetPtr("pending")}, wantNames: []string{"b"}},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, total, err := dossd.GetAll(ctx, nil, tt.filter, paginator.PageInput{}, nil)
			require.NoError(t, err)
			assert.Equal(t, len(tt.wantNames), total)
			names := []string{}
			for _, dos := range got {
				names = append(names, dos.Name)
			}
			assert.Equal(t, tt.wantNames, names)
		})
	}
}

func TestDhcpOptionSetSQLDAO_Update(t *testing.T) {
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDhcpOptionSetSetupSchema(t, dbSession)

	ipu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("johnd@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	ip := testBuildInfrastructureProvider(t, dbSession, nil, "test-ip", "Test Provider", ipu.ID)
	tnu := testBuildUser(t, dbSession, nil, testGenerateStarfleetID(), cutil.GetPtr("jdoe@test.com"), cutil.GetPtr("John"), cutil.GetPtr("Doe"))
	tn := testBuildTenant(t, dbSession, nil, "test-tenant", "test-tenant-org", tnu.ID)
	st := testBuildSite(t, dbSession, nil, ip.ID, "test-site", "Test Site", ip.Org, ipu.ID)

	dossd := NewDhcpOptionSetDAO(dbSession)
	ctx := context.Background()

	newOptions := &DhcpOptions{DhcpOptions: &corev1.DhcpOptions{
		V4: &corev1.DhcpOptionsV4{DnsServers: []string{"10.0.0.54"}},
	}}

	tests := []struct {
		name  string
		input func(id uuid.UUID) DhcpOptionSetUpdateInput
		want  func(created *DhcpOptionSet) *DhcpOptionSet
	}{
		{
			name: "set fields change and others are kept",
			input: func(id uuid.UUID) DhcpOptionSetUpdateInput {
				return DhcpOptionSetUpdateInput{
					DhcpOptionSetID: id,
					Name:            cutil.GetPtr("pxe-boot-2"),
					Options:         newOptions,
					Status:          cutil.GetPtr(DhcpOptionSetStatusReady),
					Version:         cutil.GetPtr("V2-T2"),
					IsMissingOnSite: cutil.GetPtr(true),
				}
			},
			want: func(created *DhcpOptionSet) *DhcpOptionSet {
				want := *created
				want.Name = "pxe-boot-2"
				want.Options = newOptions
				want.Status = DhcpOptionSetStatusReady
				want.Version = "V2-T2"
				want.IsMissingOnSite = true
				return &want
			},
		},
		{
			name:  "no fields leaves the row unchanged",
			input: func(id uuid.UUID) DhcpOptionSetUpdateInput { return DhcpOptionSetUpdateInput{DhcpOptionSetID: id} },
			want:  func(created *DhcpOptionSet) *DhcpOptionSet { return created },
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			created, err := dossd.Create(ctx, nil, DhcpOptionSetCreateInput{
				Name:        "pxe-boot",
				Description: cutil.GetPtr("provider boot"),
				TenantOrg:   tn.Org,
				SiteID:      st.ID,
				TenantID:    tn.ID,
				Options:     testDhcpOptions(),
				Labels:      map[string]string{"env": "prod"},
				Status:      DhcpOptionSetStatusPending,
				CreatedBy:   tnu.ID,
			})
			require.NoError(t, err)

			got, err := dossd.Update(ctx, nil, tt.input(created.ID))
			require.NoError(t, err)

			want := tt.want(created)
			assert.Equal(t, want.Name, got.Name)
			assert.Equal(t, want.Description, got.Description)
			assert.Equal(t, want.Labels, got.Labels)
			assert.True(t, proto.Equal(want.Options.DhcpOptions, got.Options.DhcpOptions))
			assert.Equal(t, want.Status, got.Status)
			assert.Equal(t, want.Version, got.Version)
			assert.Equal(t, want.IsMissingOnSite, got.IsMissingOnSite)
		})
	}
}
