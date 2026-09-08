// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
)

func TestOperatingSystem_GetSiteID(t *testing.T) {
	id := uuid.New()
	ctrlID := uuid.New()
	t.Run("falls back to ID when ControllerOperatingSystemID is nil", func(t *testing.T) {
		os := &OperatingSystem{ID: id}
		got := os.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, id, *got)
	})
	t.Run("uses ControllerOperatingSystemID when set", func(t *testing.T) {
		os := &OperatingSystem{ID: id, ControllerOperatingSystemID: &ctrlID}
		got := os.GetSiteID()
		require.NotNil(t, got)
		assert.Equal(t, ctrlID, *got)
	})
}

func TestOperatingSystem_IsTenantUsable(t *testing.T) {
	tenantID := uuid.New()
	otherTenantID := uuid.New()
	providerID := uuid.New()

	tests := []struct {
		name     string
		os       OperatingSystem
		tenantID string
		want     bool
	}{
		{
			name:     "allows tenant-owned OS for owner",
			os:       OperatingSystem{TenantID: &tenantID},
			tenantID: tenantID.String(),
			want:     true,
		},
		{
			name:     "rejects tenant-owned OS for another tenant",
			os:       OperatingSystem{TenantID: &tenantID},
			tenantID: otherTenantID.String(),
			want:     false,
		},
		{
			name: "allows provider-owned templated iPXE OS",
			os: OperatingSystem{
				InfrastructureProviderID: &providerID,
				Type:                     OperatingSystemTypeTemplatedIPXE,
			},
			tenantID: tenantID.String(),
			want:     true,
		},
		{
			name: "rejects provider-owned raw iPXE OS",
			os: OperatingSystem{
				InfrastructureProviderID: &providerID,
				Type:                     OperatingSystemTypeIPXE,
			},
			tenantID: tenantID.String(),
			want:     false,
		},
		{
			name:     "rejects OS without an owner",
			os:       OperatingSystem{Type: OperatingSystemTypeTemplatedIPXE},
			tenantID: tenantID.String(),
			want:     false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, tc.os.IsTenantUsable(tc.tenantID))
		})
	}
}

func TestOperatingSystem_ToImageAttributesProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"
	url := "https://image"
	sha := "deadbeef"
	authType := "Basic"
	authToken := "token"
	rootFsID := "fs-1"
	rootFsLabel := "label"
	imageDisk := "/dev/disk/by-id/nvme-Dell_DC_NVMe_CD7_U.2_960GB_Z3W0A01DTXBH-extra-long"
	os := &OperatingSystem{
		ID:                 id,
		Name:               "ubuntu",
		Description:        &desc,
		ImageURL:           &url,
		ImageSHA:           &sha,
		ImageAuthType:      &authType,
		ImageAuthToken:     &authToken,
		RootFsID:           &rootFsID,
		RootFsLabel:        &rootFsLabel,
		ImageDisk:          &imageDisk,
		EnableBlockStorage: true,
	}
	got := os.ToImageAttributesProto("org-1")
	require.NotNil(t, got)
	require.NotNil(t, got.Id)
	assert.Equal(t, id.String(), got.Id.Value)
	require.NotNil(t, got.Name)
	assert.Equal(t, "ubuntu", *got.Name)
	assert.Equal(t, "org-1", got.TenantOrganizationId)
	assert.Equal(t, &desc, got.Description)
	assert.Equal(t, "https://image", got.SourceUrl)
	assert.Equal(t, "deadbeef", got.Digest)
	assert.True(t, got.CreateVolume)
	assert.Equal(t, &authType, got.AuthType)
	assert.Equal(t, &authToken, got.AuthToken)
	assert.Equal(t, &rootFsID, got.RootfsId)
	assert.Equal(t, &rootFsLabel, got.RootfsLabel)
	assert.Equal(t, &imageDisk, got.BootDisk)
}

func TestOperatingSystem_ToImageDeletionRequestProto(t *testing.T) {
	id := uuid.New()
	os := &OperatingSystem{ID: id}
	got := os.ToImageDeletionRequestProto("org-1")
	require.NotNil(t, got)
	require.NotNil(t, got.Id)
	assert.Equal(t, id.String(), got.Id.Value)
	assert.Equal(t, "org-1", got.TenantOrganizationId)
}

func testOperatingSystemInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(false),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for OperatingSystem tests
func testOperatingSystemSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create OperatingSystem table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystem)(nil))
	assert.Nil(t, err)
	// create OperatingSystemSiteAssociation table
	err = dbSession.DB.ResetModel(context.Background(), (*OperatingSystemSiteAssociation)(nil))
	assert.Nil(t, err)
}

func testOperatingSystemBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
	ip := &InfrastructureProvider{
		ID:          uuid.New(),
		Name:        name,
		DisplayName: cutil.GetPtr("TestInfraProvider"),
		Org:         "test",
	}
	_, err := dbSession.DB.NewInsert().Model(ip).Exec(context.Background())
	assert.Nil(t, err)
	return ip
}

func testOperatingSystemBuildTenant(t *testing.T, dbSession *db.Session, name string) *Tenant {
	tenant := &Tenant{
		ID:   uuid.New(),
		Name: name,
		Org:  "test",
	}
	_, err := dbSession.DB.NewInsert().Model(tenant).Exec(context.Background())
	assert.Nil(t, err)
	return tenant
}

func testOperatingSystemBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
	user := &User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(starfleetID),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}
	_, err := dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	assert.Nil(t, err)
	return user
}

func TestOperatingSystemSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)
	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ossd := NewOperatingSystemDAO(dbSession)
	dummyUUID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		its                []OperatingSystem
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			its: []OperatingSystem{
				{
					Name: "test", InfrastructureProviderID: &ip.ID, TenantID: &tenant.ID, CreatedBy: user.ID, PhoneHomeEnabled: true,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with nullable site field",
			its: []OperatingSystem{
				{
					Name: "test1", InfrastructureProviderID: &ip.ID, TenantID: &tenant.ID, CreatedBy: user.ID,
				},
				{
					Name: "test2", InfrastructureProviderID: &ip.ID, TenantID: nil, CreatedBy: user.ID,
				},
				{
					Name: "test3", InfrastructureProviderID: &ip.ID, TenantID: &tenant.ID, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "failure - foreign key violation on infrastructure_provider_id",
			its: []OperatingSystem{
				{
					Name: "test", InfrastructureProviderID: &dummyUUID, TenantID: nil, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on tenant_id",
			its: []OperatingSystem{
				{
					Name: "test", InfrastructureProviderID: &ip.ID, TenantID: &dummyUUID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.its {
				os, err := ossd.Create(
					ctx, nil, OperatingSystemCreateInput{
						Name:                        i.Name,
						Description:                 cutil.GetPtr("description"),
						Org:                         "testOrg",
						InfrastructureProviderID:    i.InfrastructureProviderID,
						TenantID:                    i.TenantID,
						ControllerOperatingSystemID: &dummyUUID,
						Version:                     cutil.GetPtr("version"),
						OsType:                      "ipxe",
						ImageURL:                    cutil.GetPtr("imageURL"),
						ImageSHA:                    cutil.GetPtr("imageSHA"),
						ImageAuthType:               cutil.GetPtr("imageAuthType"),
						ImageAuthToken:              cutil.GetPtr("imageAuthToken"),
						ImageDisk:                   cutil.GetPtr("imageDisk"),
						RootFsId:                    cutil.GetPtr("rootFsId"),
						RootFsLabel:                 cutil.GetPtr("rootFsLabel"),
						IpxeScript:                  cutil.GetPtr("ipxeScript"),
						UserData:                    cutil.GetPtr("userData"),
						AllowOverride:               true,
						EnableBlockStorage:          true,
						PhoneHomeEnabled:            i.PhoneHomeEnabled,
						Status:                      OperatingSystemStatusPending,
						CreatedBy:                   i.CreatedBy,
					},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, os)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestOperatingSystemSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)
	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ossd := NewOperatingSystemDAO(dbSession)
	dummyUUID := uuid.New()
	os1, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "test1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "ipxe",
			ImageURL:                    cutil.GetPtr("imageURL"),
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          false,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os1)
	os2, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "test2",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    nil,
			TenantID:                    nil,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "image",
			ImageURL:                    cutil.GetPtr("imageURL"),
			ImageSHA:                    cutil.GetPtr("imageSHA"),
			ImageAuthType:               cutil.GetPtr("imageAuthType"),
			ImageAuthToken:              cutil.GetPtr("imageAuthToken"),
			ImageDisk:                   cutil.GetPtr("imageDisk"),
			RootFsId:                    cutil.GetPtr("rootFsId"),
			RootFsLabel:                 cutil.GetPtr("rootFsLabel"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          false,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                           string
		id                             uuid.UUID
		os                             *OperatingSystem
		paramRelations                 []string
		expectedError                  bool
		expectedErrVal                 error
		expectedInfrastructureProvider bool
		expectedTenant                 bool
		verifyChildSpanner             bool
	}{
		{
			desc:                           "GetById success when OperatingSystem exists",
			id:                             os1.ID,
			os:                             os1,
			paramRelations:                 []string{},
			expectedError:                  false,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
			verifyChildSpanner:             true,
		},
		{
			desc:                           "GetById error when not found",
			id:                             uuid.New(),
			paramRelations:                 []string{},
			expectedError:                  true,
			expectedErrVal:                 db.ErrDoesNotExist,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
		},
		{
			desc:                           "GetById with the infrastructure_provider relation",
			id:                             os1.ID,
			os:                             os1,
			paramRelations:                 []string{InfrastructureProviderRelationName},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedTenant:                 false,
		},
		{
			desc:                           "GetById with both the infrastructure_provider and site relations",
			id:                             os1.ID,
			os:                             os1,
			paramRelations:                 []string{InfrastructureProviderRelationName, TenantRelationName},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedTenant:                 true,
		},
		{
			desc:                           "GetById when both the infrastructure_provider and tenant are nil",
			id:                             os2.ID,
			os:                             os2,
			paramRelations:                 []string{InfrastructureProviderRelationName, TenantRelationName},
			expectedError:                  false,
			expectedInfrastructureProvider: false,
			expectedTenant:                 false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := ossd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.os.ID, tmp.ID)
				assert.Equal(t, tc.expectedInfrastructureProvider, tmp.InfrastructureProvider != nil)
				if tc.expectedInfrastructureProvider {
					assert.EqualValues(t, tc.os.InfrastructureProviderID, &tmp.InfrastructureProvider.ID)
				} else {
					assert.Nil(t, tmp.InfrastructureProvider)
				}
				assert.Equal(t, tc.expectedTenant, tmp.Tenant != nil)
				if tc.expectedTenant {
					assert.EqualValues(t, *tc.os.TenantID, tmp.Tenant.ID)
				} else {
					assert.Nil(t, tmp.Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestOperatingSystemSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)

	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)

	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant1 := testOperatingSystemBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testOperatingSystemBuildTenant(t, dbSession, "testTenant2")
	tenant3 := testOperatingSystemBuildTenant(t, dbSession, "testTenant3")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	site := TestBuildSite(t, dbSession, ip, "test", user)
	ossd := NewOperatingSystemDAO(dbSession)

	dummyUUID := uuid.New()

	totalCount := 30

	ossaDAO := NewOperatingSystemSiteAssociationDAO(dbSession)
	ossTenant1 := []OperatingSystem{}
	ossas := []OperatingSystemSiteAssociation{}
	for i := 0; i < totalCount; i++ {
		if i%2 == 0 {
			os, err := ossd.Create(
				ctx, nil, OperatingSystemCreateInput{
					Name:                        fmt.Sprintf("os-%v", i),
					Description:                 cutil.GetPtr("Test Description"),
					Org:                         tenant1.Org,
					InfrastructureProviderID:    nil,
					TenantID:                    &tenant1.ID,
					ControllerOperatingSystemID: &dummyUUID,
					Version:                     cutil.GetPtr("version"),
					OsType:                      OperatingSystemTypeImage,
					ImageURL:                    cutil.GetPtr("imageURL"),
					UserData:                    cutil.GetPtr("userData"),
					AllowOverride:               true,
					EnableBlockStorage:          true,
					PhoneHomeEnabled:            true,
					Status:                      OperatingSystemStatusPending,
					CreatedBy:                   user.ID,
				})
			assert.Nil(t, err)
			ossTenant1 = append(ossTenant1, *os)

			ossa1, err := ossaDAO.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
				OperatingSystemID: os.ID,
				SiteID:            site.ID,
				Status:            OperatingSystemSiteAssociationStatusSyncing,
				CreatedBy:         user.ID,
			})
			assert.Nil(t, err)
			assert.NotNil(t, ossa1)
			ossas = append(ossas, *ossa1)
		} else {
			_, err := ossd.Create(
				ctx, nil, OperatingSystemCreateInput{
					Name:                        fmt.Sprintf("os-%v", i),
					Description:                 cutil.GetPtr("description"),
					Org:                         tenant2.Org,
					InfrastructureProviderID:    nil,
					TenantID:                    &tenant2.ID,
					ControllerOperatingSystemID: &dummyUUID,
					Version:                     cutil.GetPtr("version"),
					OsType:                      OperatingSystemTypeIPXE,
					ImageURL:                    cutil.GetPtr("iPXE"),
					IpxeScript:                  cutil.GetPtr("ipxeScript"),
					UserData:                    cutil.GetPtr("userData"),
					AllowOverride:               true,
					EnableBlockStorage:          true,
					PhoneHomeEnabled:            false,
					Status:                      OperatingSystemStatusPending,
					CreatedBy:                   user.ID,
				})
			assert.Nil(t, err)
		}
	}

	testJoinCount := 5
	tenant4 := testOperatingSystemBuildTenant(t, dbSession, "testTenant4")
	site2 := TestBuildSite(t, dbSession, ip, "test2", user)
	site3 := TestBuildSite(t, dbSession, ip, "test3", user)

	ossasSite2 := []OperatingSystemSiteAssociation{}
	ossasSite3 := []OperatingSystemSiteAssociation{}
	joinIpxeOss := []OperatingSystem{}

	// iPXE image 1
	os, _ := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "ipxe-os-1",
			Description:                 cutil.GetPtr("description"),
			Org:                         tenant4.Org,
			InfrastructureProviderID:    nil,
			TenantID:                    &tenant4.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      OperatingSystemTypeIPXE,
			ImageURL:                    cutil.GetPtr("iPXE"),
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            false,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	joinIpxeOss = append(joinIpxeOss, *os)

	// iPXE image 2
	os, _ = ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "ipxe-os-2",
			Description:                 cutil.GetPtr("description"),
			Org:                         tenant4.Org,
			InfrastructureProviderID:    nil,
			TenantID:                    &tenant4.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      OperatingSystemTypeIPXE,
			ImageURL:                    cutil.GetPtr("iPXE"),
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            false,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	joinIpxeOss = append(joinIpxeOss, *os)

	// OS Image 1 for site2
	os, _ = ossd.Create(ctx, nil, OperatingSystemCreateInput{
		Name:                        "image-os-1",
		Description:                 cutil.GetPtr("Test Description"),
		Org:                         tenant4.Org,
		InfrastructureProviderID:    nil,
		TenantID:                    &tenant4.ID,
		ControllerOperatingSystemID: &dummyUUID,
		Version:                     cutil.GetPtr("version"),
		OsType:                      OperatingSystemTypeImage,
		ImageURL:                    cutil.GetPtr("imageURL"),
		UserData:                    cutil.GetPtr("userData"),
		AllowOverride:               true,
		EnableBlockStorage:          true,
		PhoneHomeEnabled:            true,
		Status:                      OperatingSystemStatusPending,
		CreatedBy:                   user.ID,
	})
	ossa, _ := ossaDAO.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: os.ID,
		SiteID:            site2.ID,
		Status:            OperatingSystemSiteAssociationStatusSyncing,
		CreatedBy:         user.ID,
	})
	ossasSite2 = append(ossasSite2, *ossa)

	// OS Image 2 for site2
	os, _ = ossd.Create(ctx, nil, OperatingSystemCreateInput{
		Name:                        "image-os-2",
		Description:                 cutil.GetPtr("Test Description"),
		Org:                         tenant4.Org,
		InfrastructureProviderID:    nil,
		TenantID:                    &tenant4.ID,
		ControllerOperatingSystemID: &dummyUUID,
		Version:                     cutil.GetPtr("version"),
		OsType:                      OperatingSystemTypeImage,
		ImageURL:                    cutil.GetPtr("imageURL"),
		UserData:                    cutil.GetPtr("userData"),
		AllowOverride:               true,
		EnableBlockStorage:          true,
		PhoneHomeEnabled:            true,
		Status:                      OperatingSystemStatusPending,
		CreatedBy:                   user.ID,
	})
	ossa, _ = ossaDAO.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: os.ID,
		SiteID:            site2.ID,
		Status:            OperatingSystemSiteAssociationStatusSyncing,
		CreatedBy:         user.ID,
	})
	ossasSite2 = append(ossasSite2, *ossa)

	// OS Image 3 for site3
	os, _ = ossd.Create(ctx, nil, OperatingSystemCreateInput{
		Name:                        "image-os-3",
		Description:                 cutil.GetPtr("Test Description"),
		Org:                         tenant4.Org,
		InfrastructureProviderID:    nil,
		TenantID:                    &tenant4.ID,
		ControllerOperatingSystemID: &dummyUUID,
		Version:                     cutil.GetPtr("version"),
		OsType:                      OperatingSystemTypeImage,
		ImageURL:                    cutil.GetPtr("imageURL"),
		UserData:                    cutil.GetPtr("userData"),
		AllowOverride:               true,
		EnableBlockStorage:          true,
		PhoneHomeEnabled:            true,
		Status:                      OperatingSystemStatusPending,
		CreatedBy:                   user.ID,
	})
	ossa, _ = ossaDAO.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
		OperatingSystemID: os.ID,
		SiteID:            site3.ID,
		Status:            OperatingSystemSiteAssociationStatusSyncing,
		CreatedBy:         user.ID,
	})
	ossasSite3 = append(ossasSite3, *ossa)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		ipID               *uuid.UUID
		osNames            []string
		osOrgs             []string
		tenantIDs          []uuid.UUID
		siteIDs            []uuid.UUID
		paramIDs           []uuid.UUID
		osTypes            []string
		searchQuery        *string
		statuses           []string
		isActive           *bool
		offset             *int
		limit              *int
		orderBy            *paginator.OrderBy
		firstEntry         *OperatingSystem
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters returns objects",
			ipID:               nil,
			tenantIDs:          nil,
			osNames:            nil,
			expectedCount:      paginator.DefaultLimit,
			expectedTotal:      cutil.GetPtr(totalCount + testJoinCount),
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:           "GetAll with relation returns objects",
			ipID:           nil,
			tenantIDs:      nil,
			osNames:        nil,
			expectedError:  false,
			expectedCount:  paginator.DefaultLimit,
			expectedTotal:  cutil.GetPtr(totalCount + testJoinCount),
			paramRelations: []string{InfrastructureProviderRelationName, TenantRelationName},
		},
		{
			desc:          "GetAll with ip filter returns no objects",
			ipID:          &dummyUUID,
			osNames:       nil,
			tenantIDs:     nil,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with name filter returns objects",
			ipID:          nil,
			osNames:       []string{"os-1"},
			tenantIDs:     nil,
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with multiple name filter values returns objects",
			ipID:          nil,
			osNames:       []string{"os-1", "os-2"},
			tenantIDs:     nil,
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with name filter returns no objects",
			ipID:          nil,
			osNames:       []string{"notfound"},
			tenantIDs:     nil,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with tenant filter returns objects",
			ipID:          nil,
			osNames:       nil,
			osOrgs:        []string{tenant1.Org},
			tenantIDs:     []uuid.UUID{tenant1.ID},
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with false active filter returns no objects",
			ipID:          nil,
			osNames:       nil,
			tenantIDs:     nil,
			isActive:      cutil.GetPtr(false),
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with true active filter returns all objects",
			ipID:          nil,
			osNames:       nil,
			tenantIDs:     nil,
			isActive:      cutil.GetPtr(true),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + testJoinCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with multiple tenant filter values returns objects",
			ipID:          nil,
			osNames:       nil,
			osOrgs:        []string{tenant1.Org, tenant2.Org},
			tenantIDs:     []uuid.UUID{tenant1.ID, tenant2.ID},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with ids filter returns objects",
			paramIDs:      []uuid.UUID{ossTenant1[0].ID, ossTenant1[1].ID},
			expectedError: false,
			expectedTotal: cutil.GetPtr(2),
			expectedCount: 2,
		},
		{
			desc:          "GetAll with tenant filter returns no objects",
			ipID:          nil,
			osNames:       nil,
			tenantIDs:     []uuid.UUID{tenant3.ID},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with name and tenant filters returns objects",
			ipID:          nil,
			osNames:       []string{"os-2"},
			tenantIDs:     []uuid.UUID{tenant1.ID},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with name and tenant filters returns no objects",
			ipID:          nil,
			osNames:       []string{"os-1"},
			tenantIDs:     []uuid.UUID{tenant3.ID},
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with limit returns objects",
			ipID:          nil,
			osNames:       nil,
			tenantIDs:     []uuid.UUID{tenant1.ID},
			offset:        cutil.GetPtr(0),
			limit:         cutil.GetPtr(5),
			expectedCount: 5,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			ipID:          nil,
			osNames:       nil,
			tenantIDs:     []uuid.UUID{tenant1.ID},
			offset:        cutil.GetPtr(5),
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:      "GetAll with order by returns objects",
			ipID:      nil,
			osNames:   nil,
			tenantIDs: []uuid.UUID{tenant1.ID},
			orderBy: &paginator.OrderBy{
				Field: "name",
				Order: paginator.OrderDescending,
			},
			firstEntry:    &ossTenant1[4], // 5th entry is "os-8" and would appear first on descending order
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by site returns objects",
			ipID:          nil,
			osOrgs:        []string{tenant1.Org},
			tenantIDs:     []uuid.UUID{tenant1.ID},
			osNames:       nil,
			siteIDs:       []uuid.UUID{site.ID},
			searchQuery:   nil,
			expectedCount: len(ossas),
			expectedTotal: cutil.GetPtr(len(ossas)),
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by only site returns objects",
			ipID:          nil,
			osOrgs:        nil,
			tenantIDs:     nil,
			osNames:       nil,
			siteIDs:       []uuid.UUID{site.ID},
			searchQuery:   nil,
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + len(joinIpxeOss)),
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by multiple sites returns objects",
			ipID:          nil,
			osOrgs:        nil,
			tenantIDs:     nil,
			osNames:       nil,
			siteIDs:       []uuid.UUID{site2.ID, site3.ID},
			osTypes:       []string{OperatingSystemTypeImage},
			searchQuery:   nil,
			expectedCount: len(ossasSite3) + len(ossasSite2),
			expectedTotal: cutil.GetPtr(len(ossasSite3) + len(ossasSite2)),
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by site and image type returns objects",
			ipID:          nil,
			osOrgs:        nil,
			tenantIDs:     []uuid.UUID{tenant1.ID},
			osNames:       nil,
			siteIDs:       []uuid.UUID{site.ID},
			osTypes:       []string{OperatingSystemTypeImage},
			searchQuery:   nil,
			expectedCount: len(ossas),
			expectedTotal: cutil.GetPtr(len(ossas)),
			expectedError: false,
		},
		{
			desc:          "GetAll with name search query returns objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			searchQuery:   cutil.GetPtr("os-"),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + testJoinCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with description search query returns objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			searchQuery:   cutil.GetPtr("description"),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + testJoinCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with os iPXE type query returns objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			osTypes:       []string{OperatingSystemTypeIPXE},
			expectedCount: totalCount/2 + len(joinIpxeOss),
			expectedTotal: cutil.GetPtr(totalCount/2 + len(joinIpxeOss)),
			expectedError: false,
		},
		{
			desc:          "GetAll with os image type query returns objects",
			ipID:          nil,
			tenantIDs:     []uuid.UUID{tenant1.ID},
			osNames:       nil,
			osTypes:       []string{OperatingSystemTypeImage},
			expectedCount: (totalCount) / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with both iPXE and OS image type query returns objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			osTypes:       []string{OperatingSystemTypeImage, OperatingSystemTypeIPXE},
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + testJoinCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with status search query returns objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			searchQuery:   cutil.GetPtr(OperatingSystemStatusPending),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + testJoinCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with OperatingSystemStatusPending status returns objects",
			ipID:          nil,
			tenantIDs:     []uuid.UUID{tenant1.ID},
			osNames:       nil,
			statuses:      []string{OperatingSystemStatusPending},
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with multiple statuses specified returns objects",
			ipID:          nil,
			tenantIDs:     []uuid.UUID{tenant1.ID},
			osNames:       nil,
			statuses:      []string{OperatingSystemStatusPending, OperatingSystemStatusError},
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with OperatingSystemStatusError status returns no objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			statuses:      []string{OperatingSystemStatusError},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns objects",
			ipID:          nil,
			tenantIDs:     nil,
			osNames:       nil,
			searchQuery:   cutil.GetPtr(""),
			expectedCount: paginator.DefaultLimit,
			expectedTotal: cutil.GetPtr(totalCount + testJoinCount),
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by tenant and site returns objects",
			ipID:          nil,
			tenantIDs:     []uuid.UUID{tenant4.ID},
			osNames:       nil,
			siteIDs:       []uuid.UUID{site3.ID},
			osTypes:       nil,
			searchQuery:   nil,
			expectedCount: len(joinIpxeOss) + len(ossasSite3),
			expectedTotal: cutil.GetPtr(len(joinIpxeOss) + len(ossasSite3)),
			expectedError: false,
		},
		{
			desc:          "GetAll with filter by tenant, site and image type returns objects",
			ipID:          nil,
			tenantIDs:     []uuid.UUID{tenant4.ID},
			osNames:       nil,
			siteIDs:       []uuid.UUID{site3.ID},
			osTypes:       []string{OperatingSystemTypeImage},
			searchQuery:   nil,
			expectedCount: len(ossasSite3),
			expectedTotal: cutil.GetPtr(len(ossasSite3)),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			filter := OperatingSystemFilterInput{
				InfrastructureProviderID: tc.ipID,
				TenantIDs:                tc.tenantIDs,
				SiteIDs:                  tc.siteIDs,
				Names:                    tc.osNames,
				Orgs:                     tc.osOrgs,
				OsTypes:                  tc.osTypes,
				Statuses:                 tc.statuses,
				IsActive:                 tc.isActive,
				SearchQuery:              tc.searchQuery,
				OperatingSystemIds:       tc.paramIDs,
			}
			page := paginator.PageInput{
				Limit:   tc.limit,
				Offset:  tc.offset,
				OrderBy: tc.orderBy,
			}
			got, total, err := ossd.GetAll(ctx, nil, filter, page, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].Tenant)
				}
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.firstEntry != nil {
				assert.Equal(t, tc.firstEntry.Name, got[0].Name)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestOperatingSystemSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)
	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	updatedIP := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testUpdatedIP")
	tenant1 := testOperatingSystemBuildTenant(t, dbSession, "testTenant1")
	updatedTenant := testOperatingSystemBuildTenant(t, dbSession, "updatedTenant")
	tenant2 := testOperatingSystemBuildTenant(t, dbSession, "testTenant2")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ossd := NewOperatingSystemDAO(dbSession)
	dummyUUID := uuid.New()
	updatedUUID := uuid.New()
	os1tenant1, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant1.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "ipxe",
			ImageURL:                    cutil.GetPtr("imageURL"),
			ImageSHA:                    cutil.GetPtr("imageSHA"),
			ImageAuthType:               cutil.GetPtr("imageAuthType"),
			ImageAuthToken:              cutil.GetPtr("imageAuthToken"),
			ImageDisk:                   cutil.GetPtr("imageDisk"),
			RootFsId:                    cutil.GetPtr("rootFsId"),
			RootFsLabel:                 cutil.GetPtr("rootFsLabel"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os1tenant1)
	os2tenant1, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os2tenant1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant1.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "ipxe",
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          false,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os2tenant1)
	os1tenant2, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant2.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "ipxe",
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          false,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os1tenant2)

	updatedAllowOverride := true
	updatedEnableBlockStorage := false
	updatedPhoneHomeEnabled := false
	updatedDeactivationNote := "reason for deactivation"
	updatedIsActive := false

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc string
		os   *OperatingSystem

		paramName                        *string
		paramDescription                 *string
		paramOrg                         *string
		paramInfrastructureProviderID    *uuid.UUID
		paramTenantID                    *uuid.UUID
		paramControllerOperatingSystemID *uuid.UUID
		paramVersion                     *string
		paramType                        *string
		paramImageURL                    *string
		paramImageSHA                    *string
		paramImageAuthType               *string
		paramImageAuthToken              *string
		paramImageDisk                   *string
		paramRootFsID                    *string
		paramRootFsLabel                 *string
		paramIpxeScript                  *string
		paramUserData                    *string
		paramAllowOverride               *bool
		paramEnableBlockStorage          *bool
		paramPhoneHomeEnabled            *bool
		paramIsActive                    *bool
		paramDeactivationNote            *string
		paramStatus                      *string

		expectedName                        *string
		expectedDescription                 *string
		expectedOrg                         *string
		expectedInfrastructureProviderID    *uuid.UUID
		expectedTenantID                    *uuid.UUID
		expectedControllerOperatingSystemID *uuid.UUID
		expectedVersion                     *string
		expectedType                        *string
		expectedImageURL                    *string
		expectedImageSHA                    *string
		expectedImageAuthType               *string
		expectedImageAuthToken              *string
		expectedImageDisk                   *string
		expectedRootFsID                    *string
		expectedRootFsLabel                 *string
		expectedIpxeScript                  *string
		expectedUserData                    *string
		expectedAllowOverride               *bool
		expectedEnableBlockStorage          *bool
		expectPhoneHomeEnabled              *bool
		expectedIsActive                    *bool
		expectedDeactivationNote            *string
		expectedStatus                      *string
		verifyChildSpanner                  bool
	}{
		{
			desc: "can update string fields: name, description, org, version, imageurl, imageSHA, imageAuthType, imageAuthToken, imageDisk, rootFsID, rootFsLabel, ipxescript, userdata, status",
			os:   os1tenant1,

			paramName:                        cutil.GetPtr("updatedName"),
			paramDescription:                 cutil.GetPtr("updatedDescription"),
			paramOrg:                         cutil.GetPtr("updatedOrg"),
			paramInfrastructureProviderID:    nil,
			paramTenantID:                    nil,
			paramControllerOperatingSystemID: nil,
			paramVersion:                     cutil.GetPtr("updatedVersion"),
			paramType:                        cutil.GetPtr("updatedType"),
			paramImageURL:                    cutil.GetPtr("updatedImageURL"),
			paramImageSHA:                    cutil.GetPtr("updatedImageSHA"),
			paramImageAuthType:               cutil.GetPtr("updatedImageAuthType"),
			paramImageAuthToken:              cutil.GetPtr("updatedImageAuthToken"),
			paramImageDisk:                   cutil.GetPtr("updatedImageDisk"),
			paramRootFsID:                    cutil.GetPtr("updatedRootFsID"),
			paramRootFsLabel:                 cutil.GetPtr("updatedRootFsLabel"),
			paramIpxeScript:                  cutil.GetPtr("updatedIpxeScript"),
			paramUserData:                    cutil.GetPtr("updatedUserData"),
			paramAllowOverride:               nil,
			paramEnableBlockStorage:          nil,
			paramPhoneHomeEnabled:            nil,
			paramStatus:                      cutil.GetPtr(OperatingSystemStatusProvisioning),

			expectedName:                        cutil.GetPtr("updatedName"),
			expectedDescription:                 cutil.GetPtr("updatedDescription"),
			expectedOrg:                         cutil.GetPtr("updatedOrg"),
			expectedInfrastructureProviderID:    os1tenant1.InfrastructureProviderID,
			expectedTenantID:                    os1tenant1.TenantID,
			expectedControllerOperatingSystemID: os1tenant1.ControllerOperatingSystemID,
			expectedVersion:                     cutil.GetPtr("updatedVersion"),
			expectedType:                        cutil.GetPtr("updatedType"),
			expectedImageURL:                    cutil.GetPtr("updatedImageURL"),
			expectedImageSHA:                    cutil.GetPtr("updatedImageSHA"),
			expectedImageAuthType:               cutil.GetPtr("updatedImageAuthType"),
			expectedImageAuthToken:              cutil.GetPtr("updatedImageAuthToken"),
			expectedImageDisk:                   cutil.GetPtr("updatedImageDisk"),
			expectedRootFsID:                    cutil.GetPtr("updatedRootFsID"),
			expectedRootFsLabel:                 cutil.GetPtr("updatedRootFsLabel"),
			expectedIpxeScript:                  cutil.GetPtr("updatedIpxeScript"),
			expectedUserData:                    cutil.GetPtr("updatedUserData"),
			expectedAllowOverride:               &os1tenant1.AllowOverride,
			expectedEnableBlockStorage:          &os1tenant1.EnableBlockStorage,
			expectPhoneHomeEnabled:              &os1tenant1.PhoneHomeEnabled,
			expectedStatus:                      cutil.GetPtr(OperatingSystemStatusProvisioning),
			verifyChildSpanner:                  true,
		},
		{
			desc: "can update uuid fields: infrastructureproviderid, tenantid, controlleroperatingsystemid",
			os:   os1tenant1,

			paramName:                        nil,
			paramDescription:                 nil,
			paramOrg:                         nil,
			paramInfrastructureProviderID:    &updatedIP.ID,
			paramTenantID:                    &updatedTenant.ID,
			paramControllerOperatingSystemID: &updatedUUID,
			paramVersion:                     nil,
			paramType:                        nil,
			paramImageURL:                    nil,
			paramImageSHA:                    nil,
			paramImageAuthType:               nil,
			paramImageAuthToken:              nil,
			paramImageDisk:                   nil,
			paramRootFsID:                    nil,
			paramRootFsLabel:                 nil,
			paramIpxeScript:                  nil,
			paramUserData:                    nil,
			paramAllowOverride:               nil,
			paramEnableBlockStorage:          nil,
			paramPhoneHomeEnabled:            nil,
			paramStatus:                      nil,

			expectedName:                        cutil.GetPtr("updatedName"),
			expectedDescription:                 cutil.GetPtr("updatedDescription"),
			expectedOrg:                         cutil.GetPtr("updatedOrg"),
			expectedInfrastructureProviderID:    &updatedIP.ID,
			expectedTenantID:                    &updatedTenant.ID,
			expectedControllerOperatingSystemID: &updatedUUID,
			expectedVersion:                     cutil.GetPtr("updatedVersion"),
			expectedType:                        cutil.GetPtr("updatedType"),
			expectedImageURL:                    cutil.GetPtr("updatedImageURL"),
			expectedImageSHA:                    cutil.GetPtr("updatedImageSHA"),
			expectedImageAuthType:               cutil.GetPtr("updatedImageAuthType"),
			expectedImageAuthToken:              cutil.GetPtr("updatedImageAuthToken"),
			expectedImageDisk:                   cutil.GetPtr("updatedImageDisk"),
			expectedRootFsID:                    cutil.GetPtr("updatedRootFsID"),
			expectedRootFsLabel:                 cutil.GetPtr("updatedRootFsLabel"),
			expectedIpxeScript:                  cutil.GetPtr("updatedIpxeScript"),
			expectedUserData:                    cutil.GetPtr("updatedUserData"),
			expectedAllowOverride:               &os1tenant1.AllowOverride,
			expectedEnableBlockStorage:          &os1tenant1.EnableBlockStorage,
			expectPhoneHomeEnabled:              &os1tenant1.PhoneHomeEnabled,
			expectedStatus:                      cutil.GetPtr(OperatingSystemStatusProvisioning),
		},
		{
			desc: "can update bool fields: allowoverride, isblockstorage",
			os:   os1tenant1,

			paramName:                        nil,
			paramDescription:                 nil,
			paramOrg:                         nil,
			paramInfrastructureProviderID:    nil,
			paramTenantID:                    nil,
			paramControllerOperatingSystemID: nil,
			paramVersion:                     nil,
			paramType:                        nil,
			paramImageURL:                    nil,
			paramImageSHA:                    nil,
			paramImageAuthType:               nil,
			paramImageAuthToken:              nil,
			paramImageDisk:                   nil,
			paramRootFsID:                    nil,
			paramRootFsLabel:                 nil,
			paramIpxeScript:                  nil,
			paramUserData:                    nil,
			paramAllowOverride:               &updatedAllowOverride,
			paramEnableBlockStorage:          &updatedEnableBlockStorage,
			paramPhoneHomeEnabled:            &updatedPhoneHomeEnabled,
			paramStatus:                      nil,

			expectedName:                        cutil.GetPtr("updatedName"),
			expectedDescription:                 cutil.GetPtr("updatedDescription"),
			expectedOrg:                         cutil.GetPtr("updatedOrg"),
			expectedInfrastructureProviderID:    &updatedIP.ID,
			expectedTenantID:                    &updatedTenant.ID,
			expectedControllerOperatingSystemID: &updatedUUID,
			expectedVersion:                     cutil.GetPtr("updatedVersion"),
			expectedType:                        cutil.GetPtr("updatedType"),
			expectedImageURL:                    cutil.GetPtr("updatedImageURL"),
			expectedImageSHA:                    cutil.GetPtr("updatedImageSHA"),
			expectedImageAuthType:               cutil.GetPtr("updatedImageAuthType"),
			expectedImageAuthToken:              cutil.GetPtr("updatedImageAuthToken"),
			expectedImageDisk:                   cutil.GetPtr("updatedImageDisk"),
			expectedRootFsID:                    cutil.GetPtr("updatedRootFsID"),
			expectedRootFsLabel:                 cutil.GetPtr("updatedRootFsLabel"),
			expectedIpxeScript:                  cutil.GetPtr("updatedIpxeScript"),
			expectedUserData:                    cutil.GetPtr("updatedUserData"),
			expectedAllowOverride:               &updatedAllowOverride,
			expectedEnableBlockStorage:          &updatedEnableBlockStorage,
			expectPhoneHomeEnabled:              &updatedEnableBlockStorage,
			expectedStatus:                      cutil.GetPtr(OperatingSystemStatusProvisioning),
		},
		{
			desc: "ok when no fields are updated",
			os:   os1tenant1,

			expectedName:                        cutil.GetPtr("updatedName"),
			expectedDescription:                 cutil.GetPtr("updatedDescription"),
			expectedOrg:                         cutil.GetPtr("updatedOrg"),
			expectedInfrastructureProviderID:    &updatedIP.ID,
			expectedTenantID:                    &updatedTenant.ID,
			expectedControllerOperatingSystemID: &updatedUUID,
			expectedVersion:                     cutil.GetPtr("updatedVersion"),
			expectedType:                        cutil.GetPtr("updatedType"),
			expectedImageURL:                    cutil.GetPtr("updatedImageURL"),
			expectedImageSHA:                    cutil.GetPtr("updatedImageSHA"),
			expectedImageAuthType:               cutil.GetPtr("updatedImageAuthType"),
			expectedImageAuthToken:              cutil.GetPtr("updatedImageAuthToken"),
			expectedImageDisk:                   cutil.GetPtr("updatedImageDisk"),
			expectedRootFsID:                    cutil.GetPtr("updatedRootFsID"),
			expectedRootFsLabel:                 cutil.GetPtr("updatedRootFsLabel"),
			expectedIpxeScript:                  cutil.GetPtr("updatedIpxeScript"),
			expectedUserData:                    cutil.GetPtr("updatedUserData"),
			expectedAllowOverride:               &updatedAllowOverride,
			expectedEnableBlockStorage:          &updatedEnableBlockStorage,
			expectPhoneHomeEnabled:              &updatedPhoneHomeEnabled,
			expectedStatus:                      cutil.GetPtr(OperatingSystemStatusProvisioning),
		},
		{
			desc:                  "can update isActive from true to false",
			os:                    os1tenant1,
			paramIsActive:         &updatedIsActive,
			paramDeactivationNote: &updatedDeactivationNote,

			expectedName:                        cutil.GetPtr("updatedName"),
			expectedDescription:                 cutil.GetPtr("updatedDescription"),
			expectedOrg:                         cutil.GetPtr("updatedOrg"),
			expectedInfrastructureProviderID:    &updatedIP.ID,
			expectedTenantID:                    &updatedTenant.ID,
			expectedControllerOperatingSystemID: &updatedUUID,
			expectedVersion:                     cutil.GetPtr("updatedVersion"),
			expectedType:                        cutil.GetPtr("updatedType"),
			expectedImageURL:                    cutil.GetPtr("updatedImageURL"),
			expectedImageSHA:                    cutil.GetPtr("updatedImageSHA"),
			expectedImageAuthType:               cutil.GetPtr("updatedImageAuthType"),
			expectedImageAuthToken:              cutil.GetPtr("updatedImageAuthToken"),
			expectedImageDisk:                   cutil.GetPtr("updatedImageDisk"),
			expectedRootFsID:                    cutil.GetPtr("updatedRootFsID"),
			expectedRootFsLabel:                 cutil.GetPtr("updatedRootFsLabel"),
			expectedIpxeScript:                  cutil.GetPtr("updatedIpxeScript"),
			expectedUserData:                    cutil.GetPtr("updatedUserData"),
			expectedAllowOverride:               &updatedAllowOverride,
			expectedEnableBlockStorage:          &updatedEnableBlockStorage,
			expectPhoneHomeEnabled:              &updatedPhoneHomeEnabled,
			expectedIsActive:                    &updatedIsActive,
			expectedDeactivationNote:            &updatedDeactivationNote,
			expectedStatus:                      cutil.GetPtr(OperatingSystemStatusProvisioning),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			input := OperatingSystemUpdateInput{
				OperatingSystemId:           tc.os.ID,
				Name:                        tc.paramName,
				Description:                 tc.paramDescription,
				Org:                         tc.paramOrg,
				InfrastructureProviderID:    tc.paramInfrastructureProviderID,
				TenantID:                    tc.paramTenantID,
				ControllerOperatingSystemID: tc.paramControllerOperatingSystemID,
				Version:                     tc.paramVersion,
				OsType:                      tc.paramType,
				ImageURL:                    tc.paramImageURL,
				ImageSHA:                    tc.paramImageSHA,
				ImageAuthType:               tc.paramImageAuthType,
				ImageAuthToken:              tc.paramImageAuthToken,
				ImageDisk:                   tc.paramImageDisk,
				RootFsId:                    tc.paramRootFsID,
				RootFsLabel:                 tc.paramRootFsLabel,
				IpxeScript:                  tc.paramIpxeScript,
				UserData:                    tc.paramUserData,
				AllowOverride:               tc.paramAllowOverride,
				EnableBlockStorage:          tc.paramEnableBlockStorage,
				PhoneHomeEnabled:            tc.paramPhoneHomeEnabled,
				IsActive:                    tc.paramIsActive,
				DeactivationNote:            tc.paramDeactivationNote,
				Status:                      tc.paramStatus,
			}
			got, err := ossd.Update(ctx, nil, input)
			assert.Nil(t, err)
			assert.NotNil(t, got)

			assert.Equal(t, *tc.expectedName, got.Name)

			assert.Equal(t, tc.expectedDescription == nil, got.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *got.Description)
			}
			assert.Equal(t, *tc.expectedOrg, got.Org)
			assert.Equal(t, tc.expectedInfrastructureProviderID == nil, got.InfrastructureProviderID == nil)
			if tc.expectedInfrastructureProviderID != nil {
				assert.Equal(t, *tc.expectedInfrastructureProviderID, *got.InfrastructureProviderID)
			}
			assert.Equal(t, tc.expectedTenantID == nil, got.TenantID == nil)
			if tc.expectedTenantID != nil {
				assert.Equal(t, *tc.expectedTenantID, *got.TenantID)
			}
			assert.Equal(t, tc.expectedControllerOperatingSystemID == nil, got.ControllerOperatingSystemID == nil)
			if tc.expectedControllerOperatingSystemID != nil {
				assert.Equal(t, *tc.expectedControllerOperatingSystemID, *got.ControllerOperatingSystemID)
			}
			assert.Equal(t, tc.expectedVersion == nil, got.Version == nil)
			if tc.expectedVersion != nil {
				assert.Equal(t, *tc.expectedVersion, *got.Version)
			}
			assert.Equal(t, tc.expectedImageURL == nil, got.ImageURL == nil)
			if tc.expectedImageURL != nil {
				assert.Equal(t, *tc.expectedImageURL, *got.ImageURL)
			}
			assert.Equal(t, tc.expectedImageSHA == nil, got.ImageSHA == nil)
			if tc.expectedImageSHA != nil {
				assert.Equal(t, *tc.expectedImageSHA, *got.ImageSHA)
			}
			assert.Equal(t, tc.expectedImageAuthType == nil, got.ImageAuthType == nil)
			if tc.expectedImageAuthType != nil {
				assert.Equal(t, *tc.expectedImageAuthType, *got.ImageAuthType)
			}
			assert.Equal(t, tc.expectedImageAuthToken == nil, got.ImageAuthToken == nil)
			if tc.expectedImageAuthToken != nil {
				assert.Equal(t, *tc.expectedImageAuthToken, *got.ImageAuthToken)
			}
			assert.Equal(t, tc.expectedImageDisk == nil, got.ImageDisk == nil)
			if tc.expectedImageDisk != nil {
				assert.Equal(t, *tc.expectedImageDisk, *got.ImageDisk)
			}
			assert.Equal(t, tc.expectedRootFsID == nil, got.RootFsID == nil)
			if tc.expectedRootFsID != nil {
				assert.Equal(t, *tc.expectedRootFsID, *got.RootFsID)
			}
			assert.Equal(t, tc.expectedRootFsLabel == nil, got.RootFsLabel == nil)
			if tc.expectedRootFsLabel != nil {
				assert.Equal(t, *tc.expectedRootFsLabel, *got.RootFsLabel)
			}
			assert.Equal(t, tc.expectedIpxeScript == nil, got.IpxeScript == nil)
			if tc.expectedIpxeScript != nil {
				assert.Equal(t, *tc.expectedIpxeScript, *got.IpxeScript)
			}
			assert.Equal(t, tc.expectedUserData == nil, got.UserData == nil)
			if tc.expectedUserData != nil {
				assert.Equal(t, *tc.expectedUserData, *got.UserData)
			}
			assert.Equal(t, *tc.expectedAllowOverride, got.AllowOverride)
			assert.Equal(t, *tc.expectedEnableBlockStorage, got.EnableBlockStorage)
			assert.Equal(t, *tc.expectPhoneHomeEnabled, got.PhoneHomeEnabled)
			if tc.expectedIsActive == nil {
				assert.Equal(t, true, got.IsActive)
			} else {
				assert.Equal(t, *tc.expectedIsActive, got.IsActive)
			}
			if tc.expectedDeactivationNote != nil {
				assert.Equal(t, *tc.expectedDeactivationNote, *got.DeactivationNote)
			}

			if got.Updated.String() == tc.os.Updated.String() {
				t.Errorf("got.Updated = %v, want different value", got.Updated)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestOperatingSystemSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)
	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant1 := testOperatingSystemBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testOperatingSystemBuildTenant(t, dbSession, "testTenant2")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ossd := NewOperatingSystemDAO(dbSession)
	dummyUUID := uuid.New()
	os1tenant1, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant1.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "image",
			ImageURL:                    cutil.GetPtr("imageURL"),
			ImageSHA:                    cutil.GetPtr("imageSHA"),
			ImageAuthType:               cutil.GetPtr("imageAuthType"),
			ImageAuthToken:              cutil.GetPtr("imageAuthToken"),
			ImageDisk:                   cutil.GetPtr("imageDisk"),
			RootFsId:                    cutil.GetPtr("rootFsId"),
			RootFsLabel:                 cutil.GetPtr("rootFsLabel"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os1tenant1)
	os2tenant1, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os2tenant1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant1.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "ipxe",
			ImageURL:                    cutil.GetPtr("imageURL"),
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os2tenant1)
	os1tenant2, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant2.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "image",
			ImageURL:                    cutil.GetPtr("imageURL"),
			ImageSHA:                    cutil.GetPtr("imageSHA"),
			ImageAuthType:               cutil.GetPtr("imageAuthType"),
			ImageAuthToken:              cutil.GetPtr("imageAuthToken"),
			ImageDisk:                   cutil.GetPtr("imageDisk"),
			RootFsId:                    cutil.GetPtr("rootFsId"),
			RootFsLabel:                 cutil.GetPtr("rootFsLabel"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os1tenant2)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                             string
		os                               *OperatingSystem
		paramDescription                 bool
		paramInfrastructureProviderID    bool
		paramTenantID                    bool
		paramControllerOperatingSystemID bool
		paramVersion                     bool
		paramImageURL                    bool
		paramImageSHA                    bool
		paramImageAuthType               bool
		paramImageAuthToken              bool
		paramImageDisk                   bool
		paramRootFsID                    bool
		paramRootFsLabel                 bool
		paramIpxeScript                  bool
		paramUserData                    bool

		expectedDescription                 *string
		expectedInfrastructureProviderID    *uuid.UUID
		expectedTenantID                    *uuid.UUID
		expectedControllerOperatingSystemID *uuid.UUID
		expectedVersion                     *string
		expectedImageURL                    *string
		expectedImageSHA                    *string
		expectedImageAuthType               *string
		expectedImageAuthToken              *string
		expectedImageDisk                   *string
		expectedRootFsID                    *string
		expectedRootFsLabel                 *string
		expectedIpxeScript                  *string
		expectedUserData                    *string
		expectedUpdate                      bool
		verifyChildSpanner                  bool
	}{
		{
			desc: "can clear description",
			os:   os1tenant1,

			paramDescription: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    os1tenant1.InfrastructureProviderID,
			expectedTenantID:                    os1tenant1.TenantID,
			expectedControllerOperatingSystemID: os1tenant1.ControllerOperatingSystemID,
			expectedVersion:                     os1tenant1.Version,
			expectedImageURL:                    os1tenant1.ImageURL,
			expectedImageSHA:                    os1tenant1.ImageSHA,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
			verifyChildSpanner:                  true,
		},
		{
			desc: "can clear InfrastructureProviderID",
			os:   os1tenant1,

			paramInfrastructureProviderID: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    os1tenant1.TenantID,
			expectedControllerOperatingSystemID: os1tenant1.ControllerOperatingSystemID,
			expectedVersion:                     os1tenant1.Version,
			expectedImageURL:                    os1tenant1.ImageURL,
			expectedImageSHA:                    os1tenant1.ImageSHA,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear TenantID",
			os:   os1tenant1,

			paramTenantID: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: os1tenant1.ControllerOperatingSystemID,
			expectedVersion:                     os1tenant1.Version,
			expectedImageURL:                    os1tenant1.ImageURL,
			expectedImageSHA:                    os1tenant1.ImageSHA,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear ControllerOperatingSystemID",
			os:   os1tenant1,

			paramControllerOperatingSystemID: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     os1tenant1.Version,
			expectedImageURL:                    os1tenant1.ImageURL,
			expectedImageSHA:                    os1tenant1.ImageSHA,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear version",
			os:   os1tenant1,

			paramVersion: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    os1tenant1.ImageURL,
			expectedImageSHA:                    os1tenant1.ImageSHA,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear ImageURL",
			os:   os1tenant1,

			paramImageURL: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    os1tenant1.ImageSHA,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear ImageSHA",
			os:   os1tenant1,

			paramImageSHA: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               os1tenant1.ImageAuthType,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear ImageAuthType",
			os:   os1tenant1,

			paramImageAuthType: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              os1tenant1.ImageAuthToken,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear ImageAuthToken",
			os:   os1tenant1,

			paramImageAuthToken: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              nil,
			expectedImageDisk:                   os1tenant1.ImageDisk,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear ImageDisk",
			os:   os1tenant1,

			paramImageDisk: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              nil,
			expectedImageDisk:                   nil,
			expectedRootFsID:                    os1tenant1.RootFsID,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear RootFsId",
			os:   os1tenant1,

			paramRootFsID: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              nil,
			expectedImageDisk:                   nil,
			expectedRootFsID:                    nil,
			expectedRootFsLabel:                 os1tenant1.RootFsLabel,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear RootFsLabel",
			os:   os1tenant1,

			paramRootFsLabel: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              nil,
			expectedImageDisk:                   nil,
			expectedRootFsID:                    nil,
			expectedRootFsLabel:                 nil,
			expectedIpxeScript:                  os1tenant1.IpxeScript,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear IpxeScript",
			os:   os1tenant1,

			paramIpxeScript: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedIpxeScript:                  nil,
			expectedUserData:                    os1tenant1.UserData,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear UserData",
			os:   os1tenant1,

			paramUserData: true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              nil,
			expectedImageDisk:                   nil,
			expectedRootFsID:                    nil,
			expectedRootFsLabel:                 nil,
			expectedIpxeScript:                  nil,
			expectedUserData:                    nil,
			expectedUpdate:                      true,
		},
		{
			desc: "can clear multiple fields at once",
			os:   os1tenant2,

			paramDescription:                 true,
			paramInfrastructureProviderID:    true,
			paramTenantID:                    true,
			paramControllerOperatingSystemID: true,
			paramVersion:                     true,
			paramImageURL:                    true,
			paramImageSHA:                    true,
			paramImageAuthType:               true,
			paramImageAuthToken:              true,
			paramImageDisk:                   true,
			paramRootFsID:                    true,
			paramRootFsLabel:                 true,
			paramIpxeScript:                  true,
			paramUserData:                    true,

			expectedDescription:                 nil,
			expectedInfrastructureProviderID:    nil,
			expectedTenantID:                    nil,
			expectedControllerOperatingSystemID: nil,
			expectedVersion:                     nil,
			expectedImageURL:                    nil,
			expectedImageSHA:                    nil,
			expectedImageAuthType:               nil,
			expectedImageAuthToken:              nil,
			expectedImageDisk:                   nil,
			expectedRootFsID:                    nil,
			expectedRootFsLabel:                 nil,
			expectedIpxeScript:                  nil,
			expectedUserData:                    nil,
			expectedUpdate:                      true,
		},
		{
			desc: "nop when no cleared fields are specified",
			os:   os2tenant1,

			expectedDescription:                 os2tenant1.Description,
			expectedInfrastructureProviderID:    os2tenant1.InfrastructureProviderID,
			expectedTenantID:                    os2tenant1.TenantID,
			expectedControllerOperatingSystemID: os2tenant1.ControllerOperatingSystemID,
			expectedVersion:                     os2tenant1.Version,
			expectedImageURL:                    os2tenant1.ImageURL,
			expectedImageSHA:                    os2tenant1.ImageSHA,
			expectedImageAuthType:               os2tenant1.ImageAuthType,
			expectedImageAuthToken:              os2tenant1.ImageAuthToken,
			expectedImageDisk:                   os2tenant1.ImageDisk,
			expectedRootFsID:                    os2tenant1.RootFsID,
			expectedRootFsLabel:                 os2tenant1.RootFsLabel,
			expectedIpxeScript:                  os2tenant1.IpxeScript,
			expectedUserData:                    os2tenant1.UserData,
			expectedUpdate:                      false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			input := OperatingSystemClearInput{
				OperatingSystemId:           tc.os.ID,
				Description:                 tc.paramDescription,
				InfrastructureProviderID:    tc.paramInfrastructureProviderID,
				TenantID:                    tc.paramTenantID,
				ControllerOperatingSystemID: tc.paramControllerOperatingSystemID,
				Version:                     tc.paramVersion,
				ImageURL:                    tc.paramImageURL,
				ImageSHA:                    tc.paramImageSHA,
				ImageAuthType:               tc.paramImageAuthType,
				ImageAuthToken:              tc.paramImageAuthToken,
				ImageDisk:                   tc.paramImageDisk,
				RootFsId:                    tc.paramRootFsID,
				RootFsLabel:                 tc.paramRootFsLabel,
				IpxeScript:                  tc.paramIpxeScript,
				UserData:                    tc.paramUserData,
			}
			tmp, err := ossd.Clear(ctx, nil, input)
			assert.Nil(t, err)
			assert.NotNil(t, tmp)
			assert.Equal(t, tc.expectedDescription == nil, tmp.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *tmp.Description)
			}
			assert.Equal(t, tc.expectedInfrastructureProviderID == nil, tmp.InfrastructureProviderID == nil)
			if tc.expectedInfrastructureProviderID != nil {
				assert.Equal(t, *tc.expectedInfrastructureProviderID, *tmp.InfrastructureProviderID)
			}
			assert.Equal(t, tc.expectedTenantID == nil, tmp.TenantID == nil)
			if tc.expectedTenantID != nil {
				assert.Equal(t, *tc.expectedTenantID, *tmp.TenantID)
			}
			assert.Equal(t, tc.expectedControllerOperatingSystemID == nil, tmp.ControllerOperatingSystemID == nil)
			if tc.expectedControllerOperatingSystemID != nil {
				assert.Equal(t, *tc.expectedControllerOperatingSystemID, *tmp.ControllerOperatingSystemID)
			}
			assert.Equal(t, tc.expectedVersion == nil, tmp.Version == nil)
			if tc.expectedVersion != nil {
				assert.Equal(t, *tc.expectedVersion, *tmp.Version)
			}
			assert.Equal(t, tc.expectedImageURL == nil, tmp.ImageURL == nil)
			if tc.expectedImageURL != nil {
				assert.Equal(t, *tc.expectedImageURL, *tmp.ImageURL)
			}
			assert.Equal(t, tc.expectedImageSHA == nil, tmp.ImageSHA == nil)
			if tc.expectedImageSHA != nil {
				assert.Equal(t, *tc.expectedImageSHA, *tmp.ImageSHA)
			}
			assert.Equal(t, tc.expectedImageAuthType == nil, tmp.ImageAuthType == nil)
			if tc.expectedImageAuthType != nil {
				assert.Equal(t, *tc.expectedImageAuthType, *tmp.ImageAuthType)
			}
			assert.Equal(t, tc.expectedImageAuthToken == nil, tmp.ImageAuthToken == nil)
			if tc.expectedImageAuthToken != nil {
				assert.Equal(t, *tc.expectedImageAuthToken, *tmp.ImageAuthToken)
			}
			assert.Equal(t, tc.expectedImageDisk == nil, tmp.ImageDisk == nil)
			if tc.expectedImageDisk != nil {
				assert.Equal(t, *tc.expectedImageDisk, *tmp.ImageDisk)
			}
			assert.Equal(t, tc.expectedRootFsID == nil, tmp.RootFsID == nil)
			if tc.expectedRootFsID != nil {
				assert.Equal(t, *tc.expectedRootFsID, *tmp.RootFsID)
			}
			assert.Equal(t, tc.expectedRootFsLabel == nil, tmp.RootFsLabel == nil)
			if tc.expectedRootFsLabel != nil {
				assert.Equal(t, *tc.expectedRootFsLabel, *tmp.RootFsLabel)
			}
			assert.Equal(t, tc.expectedIpxeScript == nil, tmp.IpxeScript == nil)
			if tc.expectedIpxeScript != nil {
				assert.Equal(t, *tc.expectedIpxeScript, *tmp.IpxeScript)
			}
			assert.Equal(t, tc.expectedUserData == nil, tmp.UserData == nil)
			if tc.expectedUserData != nil {
				assert.Equal(t, *tc.expectedUserData, *tmp.UserData)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.os.Updated))
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestOperatingSystemSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)
	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	ossd := NewOperatingSystemDAO(dbSession)
	dummyUUID := uuid.New()
	os1, err := ossd.Create(
		ctx, nil, OperatingSystemCreateInput{
			Name:                        "os1",
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    &ip.ID,
			TenantID:                    &tenant.ID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      "ipxe",
			ImageURL:                    cutil.GetPtr("imageURL"),
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            true,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
	assert.Nil(t, err)
	assert.NotNil(t, os1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		itID               uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			itID:               os1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			itID:          uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := ossd.Delete(ctx, nil, tc.itID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := ossd.GetByID(ctx, nil, tc.itID, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

// TestOperatingSystemSQLDAO_GetAll_OwnershipFilters exercises the provider-only,
// tenant-only, and dual-role ownership filters. Cross-ownership visibility is
// composed by API handlers rather than by the DAO.
func TestOperatingSystemSQLDAO_GetAll_OwnershipFilters(t *testing.T) {
	ctx := context.Background()
	dbSession := testOperatingSystemInitDB(t)
	defer dbSession.Close()
	testOperatingSystemSetupSchema(t, dbSession)

	ip := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "testIP")
	tenant := testOperatingSystemBuildTenant(t, dbSession, "testTenant")
	user := testOperatingSystemBuildUser(t, dbSession, "testUser")
	siteX := TestBuildSite(t, dbSession, ip, "siteX", user)
	siteY := TestBuildSite(t, dbSession, ip, "siteY", user)
	otherIP := testOperatingSystemBuildInfrastructureProvider(t, dbSession, "otherIP")
	siteZ := TestBuildSite(t, dbSession, otherIP, "siteZ", user)

	ossd := NewOperatingSystemDAO(dbSession)
	ossaDAO := NewOperatingSystemSiteAssociationDAO(dbSession)
	dummyUUID := uuid.New()

	buildOS := func(name string, providerID, tenantID *uuid.UUID) *OperatingSystem {
		os, err := ossd.Create(ctx, nil, OperatingSystemCreateInput{
			Name:                        name,
			Description:                 cutil.GetPtr("description"),
			Org:                         "testOrg",
			InfrastructureProviderID:    providerID,
			TenantID:                    tenantID,
			ControllerOperatingSystemID: &dummyUUID,
			Version:                     cutil.GetPtr("version"),
			OsType:                      OperatingSystemTypeIPXE,
			ImageURL:                    cutil.GetPtr("iPXE"),
			IpxeScript:                  cutil.GetPtr("ipxeScript"),
			UserData:                    cutil.GetPtr("userData"),
			AllowOverride:               true,
			EnableBlockStorage:          true,
			PhoneHomeEnabled:            false,
			Status:                      OperatingSystemStatusPending,
			CreatedBy:                   user.ID,
		})
		require.NoError(t, err)
		require.NotNil(t, os)
		return os
	}

	associate := func(osID, siteID uuid.UUID) {
		ossa, err := ossaDAO.Create(ctx, nil, OperatingSystemSiteAssociationCreateInput{
			OperatingSystemID: osID,
			SiteID:            siteID,
			Status:            OperatingSystemSiteAssociationStatusSyncing,
			CreatedBy:         user.ID,
		})
		require.NoError(t, err)
		require.NotNil(t, ossa)
	}

	// Provider-owned OSes.
	provAtX := buildOS("prov-at-x", &ip.ID, nil)
	associate(provAtX.ID, siteX.ID)
	provAtY := buildOS("prov-at-y", &ip.ID, nil)
	associate(provAtY.ID, siteY.ID)
	buildOS("prov-no-site", &ip.ID, nil)
	provAtZ := buildOS("other-prov-at-z", &otherIP.ID, nil)
	associate(provAtZ.ID, siteZ.ID)

	// Tenant-owned OSes.
	buildOS("tenant-1", nil, &tenant.ID)
	buildOS("tenant-2", nil, &tenant.ID)

	tests := []struct {
		desc          string
		providerID    *uuid.UUID
		tenantIDs     []uuid.UUID
		expectedNames []string
	}{
		{
			desc:          "provider-only view returns all provider OSes",
			providerID:    &ip.ID,
			expectedNames: []string{"prov-at-x", "prov-at-y", "prov-no-site"},
		},
		{
			desc:          "tenant-only view returns only tenant OSes",
			tenantIDs:     []uuid.UUID{tenant.ID},
			expectedNames: []string{"tenant-1", "tenant-2"},
		},
		{
			desc:          "dual-role view returns tenant and provider OSes",
			providerID:    &ip.ID,
			tenantIDs:     []uuid.UUID{tenant.ID},
			expectedNames: []string{"prov-at-x", "prov-at-y", "prov-no-site", "tenant-1", "tenant-2"},
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			filter := OperatingSystemFilterInput{
				InfrastructureProviderID: tc.providerID,
				TenantIDs:                tc.tenantIDs,
			}
			page := paginator.PageInput{Limit: cutil.GetPtr(paginator.TotalLimit)}
			got, total, err := ossd.GetAll(ctx, nil, filter, page, nil)
			require.NoError(t, err)
			gotNames := make([]string, len(got))
			for i, os := range got {
				gotNames[i] = os.Name
			}
			assert.ElementsMatch(t, tc.expectedNames, gotNames)
			assert.Equal(t, len(tc.expectedNames), total)
		})
	}

}
