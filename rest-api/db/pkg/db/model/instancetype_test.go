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

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/uptrace/bun/extra/bundebug"
)

func TestInstanceType_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"

	t.Run("populates id metadata and capabilities sourced from it.Capabilities", func(t *testing.T) {
		it := &InstanceType{
			ID:          id,
			Name:        "small",
			Description: &desc,
			Labels:      map[string]string{"env": "prod"},
			Capabilities: []*MachineCapability{
				{Type: MachineCapabilityTypeCPU, Name: "cpu-0"},
			},
		}
		proto := it.ToProto()
		require.NotNil(t, proto)
		assert.Equal(t, id.String(), proto.Id)
		require.NotNil(t, proto.Metadata)
		assert.Equal(t, "small", proto.Metadata.Name)
		assert.Equal(t, "primary", proto.Metadata.Description)
		require.Len(t, proto.Metadata.Labels, 1)
		assert.Equal(t, "env", proto.Metadata.Labels[0].Key)
		require.NotNil(t, proto.Metadata.Labels[0].Value)
		assert.Equal(t, "prod", *proto.Metadata.Labels[0].Value)
		require.NotNil(t, proto.Attributes)
		require.Len(t, proto.Attributes.DesiredCapabilities, 1)
		assert.Equal(t, corev1.MachineCapabilityType_CAP_TYPE_CPU, proto.Attributes.DesiredCapabilities[0].CapabilityType)
		require.NotNil(t, proto.Attributes.DesiredCapabilities[0].Name)
		assert.Equal(t, "cpu-0", *proto.Attributes.DesiredCapabilities[0].Name)
	})

	t.Run("nil description and labels yield empty metadata fields, no capabilities yields nil", func(t *testing.T) {
		it := &InstanceType{ID: id, Name: "small"}
		proto := it.ToProto()
		require.NotNil(t, proto)
		assert.Equal(t, "", proto.Metadata.Description)
		assert.Nil(t, proto.Metadata.Labels)
		require.NotNil(t, proto.Attributes)
		assert.Nil(t, proto.Attributes.DesiredCapabilities)
	})

	t.Run("nil entries in Capabilities are skipped", func(t *testing.T) {
		it := &InstanceType{
			ID:   id,
			Name: "small",
			Capabilities: []*MachineCapability{
				nil,
				{Type: MachineCapabilityTypeMemory, Name: "mem-0"},
				nil,
			},
		}
		proto := it.ToProto()
		require.NotNil(t, proto.Attributes)
		require.Len(t, proto.Attributes.DesiredCapabilities, 1)
		assert.Equal(t, corev1.MachineCapabilityType_CAP_TYPE_MEMORY, proto.Attributes.DesiredCapabilities[0].CapabilityType)
	})
}

func TestInstanceType_FromProto(t *testing.T) {
	id := uuid.New()

	t.Run("nil proto leaves the receiver untouched", func(t *testing.T) {
		desc := "original"
		it := &InstanceType{
			ID:          id,
			Name:        "name",
			Description: &desc,
			Labels:      map[string]string{"a": "1"},
		}
		it.FromProto(nil)
		assert.Equal(t, id, it.ID)
		assert.Equal(t, "name", it.Name)
		require.NotNil(t, it.Description)
		assert.Equal(t, "original", *it.Description)
		assert.Equal(t, Labels{"a": "1"}, it.Labels)
	})

	t.Run("populates from proto metadata", func(t *testing.T) {
		v := "v1"
		proto := &corev1.InstanceType{
			Id: id.String(),
			Metadata: &corev1.Metadata{
				Name:        "small",
				Description: "primary",
				Labels:      []*corev1.Label{{Key: "env", Value: &v}},
			},
		}
		it := &InstanceType{}
		it.FromProto(proto)
		assert.Equal(t, id, it.ID)
		assert.Equal(t, "small", it.Name)
		require.NotNil(t, it.Description)
		assert.Equal(t, "primary", *it.Description)
		assert.Equal(t, Labels{"env": "v1"}, it.Labels)
	})

	t.Run("clears optional fields when proto Metadata omits them", func(t *testing.T) {
		desc := "original"
		it := &InstanceType{
			ID:          id,
			Description: &desc,
			Labels:      map[string]string{"a": "1"},
		}
		proto := &corev1.InstanceType{
			Id:       id.String(),
			Metadata: &corev1.Metadata{Name: "small"},
		}
		it.FromProto(proto)
		assert.Equal(t, "small", it.Name)
		assert.Nil(t, it.Description)
		assert.Nil(t, it.Labels)
	})

	t.Run("preserves existing ID when proto Id is unparseable", func(t *testing.T) {
		it := &InstanceType{ID: id}
		proto := &corev1.InstanceType{Id: "not-a-uuid"}
		it.FromProto(proto)
		assert.Equal(t, id, it.ID)
	})

	t.Run("clears Name, Description, Labels when proto Metadata is nil", func(t *testing.T) {
		desc := "stale"
		it := &InstanceType{
			ID:          id,
			Name:        "stale",
			Description: &desc,
			Labels:      map[string]string{"old": "val"},
		}
		proto := &corev1.InstanceType{Id: id.String()}
		it.FromProto(proto)
		assert.Equal(t, "", it.Name)
		assert.Nil(t, it.Description)
		assert.Nil(t, it.Labels)
	})
}

func TestLabels_ToProto(t *testing.T) {
	t.Run("nil map yields nil slice", func(t *testing.T) {
		var l Labels
		assert.Nil(t, l.ToProto())
	})
	t.Run("empty map yields empty slice", func(t *testing.T) {
		got := Labels{}.ToProto()
		require.NotNil(t, got)
		assert.Len(t, got, 0)
	})
	t.Run("populated map round-trips through ordering-agnostic comparison", func(t *testing.T) {
		got := Labels{"a": "1", "b": "2"}.ToProto()
		require.Len(t, got, 2)
		seen := map[string]string{}
		for _, l := range got {
			require.NotNil(t, l.Value)
			seen[l.Key] = *l.Value
		}
		assert.Equal(t, map[string]string{"a": "1", "b": "2"}, seen)
	})
}

func testInstanceTypeInitDB(t *testing.T) *db.Session {
	dbSession := util.GetTestDBSession(t, false)
	dbSession.DB.AddQueryHook(bundebug.NewQueryHook(
		bundebug.WithEnabled(true),
		//bundebug.WithVerbose(true),
		bundebug.FromEnv(""),
	))
	return dbSession
}

// reset the tables needed for InstanceType tests
func testInstanceTypeSetupSchema(t *testing.T, dbSession *db.Session) {
	// create Infrastructure Provider table
	err := dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create User table
	err = dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create Instance Type table
	err = dbSession.DB.ResetModel(context.Background(), (*InstanceType)(nil))
	assert.Nil(t, err)
}

func testInstanceTypeBuildInfrastructureProvider(t *testing.T, dbSession *db.Session, name string) *InfrastructureProvider {
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

func testInstanceTypeBuildSite(t *testing.T, dbSession *db.Session, ip *InfrastructureProvider, name string) *Site {
	st := &Site{
		ID:                          uuid.New(),
		Name:                        name,
		DisplayName:                 cutil.GetPtr("Test"),
		Org:                         "test",
		InfrastructureProviderID:    ip.ID,
		SiteControllerVersion:       cutil.GetPtr("1.0.0"),
		SiteAgentVersion:            cutil.GetPtr("1.0.0"),
		RegistrationToken:           cutil.GetPtr("1234-5678-9012-3456"),
		RegistrationTokenExpiration: cutil.GetPtr(db.GetCurTime()),
		Status:                      SiteStatusPending,
		CreatedBy:                   uuid.New(),
	}
	_, err := dbSession.DB.NewInsert().Model(st).Exec(context.Background())
	assert.Nil(t, err)
	return st
}

func testInstanceTypeBuildUser(t *testing.T, dbSession *db.Session, starfleetID string) *User {
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

func TestInstanceTypeSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testInstanceTypeSetupSchema(t, dbSession)
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceTypeBuildSite(t, dbSession, ip, "testSite")
	user := testInstanceTypeBuildUser(t, dbSession, "testUser")
	itsd := NewInstanceTypeDAO(dbSession)
	dummyUUID := uuid.New()
	infinityResourceTypeID := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	knownID := uuid.New()

	tests := []struct {
		desc               string
		knownID            *uuid.UUID
		its                []InstanceType
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			its: []InstanceType{
				{
					Name: "test", InfrastructureProviderID: ip.ID, InfinityResourceTypeID: &infinityResourceTypeID, SiteID: &site.ID, Labels: map[string]string{"test1": "test1"}, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with nullable site field",
			its: []InstanceType{
				{
					Name: "test1", InfrastructureProviderID: ip.ID, InfinityResourceTypeID: &infinityResourceTypeID, SiteID: &site.ID, CreatedBy: user.ID,
				},
				{
					Name: "test2", InfrastructureProviderID: ip.ID, InfinityResourceTypeID: nil, SiteID: nil, CreatedBy: user.ID,
				},
				{
					Name: "test3", InfrastructureProviderID: ip.ID, SiteID: &site.ID, CreatedBy: user.ID,
				},
			},
			expectError: false,
		},
		{
			desc:    "create one with known ID",
			knownID: &knownID,
			its: []InstanceType{
				{
					Name: "test", InfrastructureProviderID: ip.ID, InfinityResourceTypeID: &infinityResourceTypeID, SiteID: &site.ID, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:    "create multiple with the same known ID should fail",
			knownID: &knownID,
			its: []InstanceType{
				{
					Name: "test1", InfrastructureProviderID: ip.ID, InfinityResourceTypeID: &infinityResourceTypeID, SiteID: &site.ID, CreatedBy: user.ID,
				},
				{
					Name: "test2", InfrastructureProviderID: ip.ID, InfinityResourceTypeID: &infinityResourceTypeID, SiteID: &site.ID, CreatedBy: user.ID,
				},
			},
			expectError:        true,
			verifyChildSpanner: true,
		},
		{
			desc: "failure - foreign key violation on infrastructure_provider_id",
			its: []InstanceType{
				{
					Name: "test", InfrastructureProviderID: uuid.New(), SiteID: nil, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "failure - foreign key violation on site_id",
			its: []InstanceType{
				{
					Name: "test", InfrastructureProviderID: ip.ID, SiteID: &dummyUUID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, i := range tc.its {
				var id *uuid.UUID = nil
				if tc.knownID != nil {
					id = tc.knownID
				}
				it, err := itsd.Create(
					ctx, nil, InstanceTypeCreateInput{ID: id, Name: i.Name, DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
						InfrastructureProviderID: i.InfrastructureProviderID, InfinityResourceTypeID: i.InfinityResourceTypeID, SiteID: i.SiteID, Labels: i.Labels, Status: InstanceTypeStatusPending, CreatedBy: i.CreatedBy},
				)
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, it)

					if tc.knownID != nil {
						assert.Equal(t, *tc.knownID, it.ID)
					}
					if i.Labels != nil {
						assert.Equal(t, i.Labels, it.Labels)
					}
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestInstanceTypeSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testInstanceTypeSetupSchema(t, dbSession)
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceTypeBuildSite(t, dbSession, ip, "testSite")
	user := testInstanceTypeBuildUser(t, dbSession, "testUser")
	itsd := NewInstanceTypeDAO(dbSession)
	it, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test1", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site.ID, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)
	it2, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test2", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: nil, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                           string
		id                             uuid.UUID
		it                             *InstanceType
		paramRelations                 []string
		expectedError                  bool
		expectedErrVal                 error
		expectedInfrastructureProvider bool
		expectedSite                   bool
		verifyChildSpanner             bool
	}{
		{
			desc:                           "GetById success when InstanceType exists",
			id:                             it.ID,
			it:                             it,
			paramRelations:                 []string{},
			expectedError:                  false,
			expectedInfrastructureProvider: false,
			expectedSite:                   false,
			verifyChildSpanner:             true,
		},
		{
			desc:                           "GetById error when not found",
			id:                             uuid.New(),
			paramRelations:                 []string{},
			expectedError:                  true,
			expectedErrVal:                 db.ErrDoesNotExist,
			expectedInfrastructureProvider: false,
			expectedSite:                   false,
		},
		{
			desc:                           "GetById with the infrastructure_provider relation",
			id:                             it.ID,
			it:                             it,
			paramRelations:                 []string{"InfrastructureProvider"},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedSite:                   false,
		},
		{
			desc:                           "GetById with both the infrastructure_provider and site relations",
			id:                             it.ID,
			it:                             it,
			paramRelations:                 []string{"InfrastructureProvider", "Site"},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedSite:                   true,
		},
		{
			desc:                           "GetById with both the infrastructure_provider and site relations when site_id is nil",
			id:                             it2.ID,
			it:                             it2,
			paramRelations:                 []string{"InfrastructureProvider", "Site"},
			expectedError:                  false,
			expectedInfrastructureProvider: true,
			expectedSite:                   false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := itsd.GetByID(ctx, nil, tc.id, tc.paramRelations)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.EqualValues(t, tc.it.ID, tmp.ID)
				assert.Equal(t, tc.expectedInfrastructureProvider, tmp.InfrastructureProvider != nil)
				if tc.expectedInfrastructureProvider {
					assert.EqualValues(t, tc.it.InfrastructureProviderID, tmp.InfrastructureProvider.ID)
				}
				assert.Equal(t, tc.expectedSite, tmp.Site != nil)
				if tc.expectedSite {
					assert.EqualValues(t, *tc.it.SiteID, tmp.Site.ID)
				} else {
					assert.Nil(t, tmp.Site)
				}
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInstanceTypeSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testAllocationSetupSchema(t, dbSession)
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "testIP")
	site1 := testInstanceTypeBuildSite(t, dbSession, ip, "testSite1")
	site2 := testInstanceTypeBuildSite(t, dbSession, ip, "testSite2")
	site3 := testInstanceTypeBuildSite(t, dbSession, ip, "testSite3")
	user := testInstanceTypeBuildUser(t, dbSession, "testUser")
	itsd := NewInstanceTypeDAO(dbSession)

	tenant1 := testAllocationBuildTenant(t, dbSession, "testTenant1")

	aDAO := NewAllocationDAO(dbSession)
	acDAO := NewAllocationConstraintDAO(dbSession)

	at1, err := aDAO.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test-t1",
		Description:              cutil.GetPtr("Test Allocation 1 for Tenant 1"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant1.ID,
		SiteID:                   site1.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, at1)

	at2, err := aDAO.Create(ctx, nil, AllocationCreateInput{
		Name:                     "test-t2",
		Description:              cutil.GetPtr("Test Allocation 2 for Tenant 1"),
		InfrastructureProviderID: ip.ID,
		TenantID:                 tenant1.ID,
		SiteID:                   site2.ID,
		Status:                   AllocationStatusPending,
		CreatedBy:                user.ID,
	})

	assert.Nil(t, err)
	assert.NotNil(t, at2)

	site2its := []InstanceType{}

	totalCount := 30

	for i := 0; i < totalCount; i++ {
		if i%2 == 1 {
			it, err := itsd.Create(
				ctx, nil, InstanceTypeCreateInput{Name: fmt.Sprintf("test-%v", i), DisplayName: cutil.GetPtr(fmt.Sprintf("test-displayname-%v", i)), Description: cutil.GetPtr("Test Description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
					InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site1.ID, Labels: map[string]string{fmt.Sprintf("label-key-%v", i): fmt.Sprintf("label-value-%v", i)}, Status: InstanceTypeStatusPending, CreatedBy: user.ID})
			assert.NoError(t, err)

			// Create a single allocation with a constraint > 0
			if i == 1 {
				_, serr := acDAO.Create(ctx, nil, AllocationConstraintCreateInput{
					AllocationID: at1.ID, ResourceType: AllocationResourceTypeInstanceType,
					ResourceTypeID: it.ID, ConstraintType: AllocationConstraintTypeReserved,
					ConstraintValue: 5, CreatedBy: user.ID,
				})
				assert.NoError(t, serr)
			}

			// Create an allocation but with no real constraint.  An allocation that exists but is empty.
			if i == 3 {
				_, serr := acDAO.Create(ctx, nil, AllocationConstraintCreateInput{
					AllocationID: at2.ID, ResourceType: AllocationResourceTypeInstanceType,
					ResourceTypeID: it.ID, ConstraintType: AllocationConstraintTypeReserved,
					ConstraintValue: 0, CreatedBy: user.ID,
				})
				assert.NoError(t, serr)
			}

		} else {
			it, err := itsd.Create(
				ctx, nil, InstanceTypeCreateInput{Name: fmt.Sprintf("test-%v", i), DisplayName: cutil.GetPtr(fmt.Sprintf("test-displayname-%v", i)), Description: cutil.GetPtr("Test Description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
					InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site2.ID, Labels: map[string]string{fmt.Sprintf("label-key-%v", i): fmt.Sprintf("label-value-%v", i)}, Status: InstanceTypeStatusPending, CreatedBy: user.ID})
			assert.NoError(t, err)
			site2its = append(site2its, *it)
		}
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		itName             *string
		itDisplayName      *string
		ipID               *uuid.UUID
		siteID             *uuid.UUID
		searchQuery        *string
		ids                []uuid.UUID
		tenantIDs          []uuid.UUID
		status             *string
		offset             *int
		limit              *int
		orderBy            *paginator.OrderBy
		firstEntry         *InstanceType
		expectedCount      int
		expectedTotal      *int
		expectedError      bool
		paramRelations     []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with name filter returns objects",
			itName:             cutil.GetPtr("test-1"),
			ipID:               nil,
			siteID:             nil,
			expectedCount:      1,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "GetAll with display name filter returns objects",
			itDisplayName: cutil.GetPtr("test-displayname-1"),
			ipID:          nil,
			siteID:        nil,
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with name and display name filter returns objects",
			itName:        cutil.GetPtr("test-1"),
			itDisplayName: cutil.GetPtr("test-displayname-1"),
			ipID:          nil,
			siteID:        nil,
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:           "GetAll with relation returns objects",
			itName:         nil,
			ipID:           nil,
			siteID:         nil,
			expectedCount:  paginator.DefaultLimit,
			expectedError:  false,
			paramRelations: []string{InfrastructureProviderRelationName, SiteRelationName},
		},
		{
			desc:          "GetAll with non-existent name filter returns no objects",
			ipID:          nil,
			itName:        cutil.GetPtr("notfound"),
			siteID:        nil,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with infrastructure provider filters returns objects",
			itName:        nil,
			ipID:          cutil.GetPtr(ip.ID),
			siteID:        nil,
			expectedCount: paginator.DefaultLimit,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with invalid infrastructure provider filters returns no objects",
			itName:        nil,
			ipID:          cutil.GetPtr(uuid.New()),
			siteID:        nil,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with site filter returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        &site1.ID,
			expectedCount: totalCount / 2,
			expectedError: false,
		},
		{
			desc:          "GetAll with site filter returns no objects",
			itName:        nil,
			ipID:          nil,
			siteID:        &site3.ID,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with name and site filters returns objects",
			itName:        cutil.GetPtr("test-1"),
			ipID:          nil,
			siteID:        &site1.ID,
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with name and unassociated site filters returns no objects",
			itName:        cutil.GetPtr("test-1"),
			ipID:          nil,
			siteID:        &site3.ID,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with provider, name and site filters returns objects",
			itName:        cutil.GetPtr("test-1"),
			ipID:          cutil.GetPtr(ip.ID),
			siteID:        &site1.ID,
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with provider, name and unassociated site filters returns no objects",
			itName:        cutil.GetPtr("test-1"),
			ipID:          nil,
			siteID:        &site3.ID,
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with ids filter returns objects",
			ids:           []uuid.UUID{site2its[0].ID, site2its[1].ID, site2its[2].ID},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc:          "GetAll with limit returns objects",
			itName:        nil,
			ipID:          cutil.GetPtr(ip.ID),
			siteID:        nil,
			offset:        cutil.GetPtr(0),
			limit:         cutil.GetPtr(5),
			expectedCount: 5,
			expectedTotal: &totalCount,
			expectedError: false,
		},
		{
			desc:          "GetAll with offset returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        &site1.ID,
			offset:        cutil.GetPtr(5),
			expectedCount: 10,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:   "GetAll with order by returns objects",
			itName: nil,
			ipID:   nil,
			siteID: &site2.ID,
			orderBy: &paginator.OrderBy{
				Field: "name",
				Order: paginator.OrderDescending,
			},
			firstEntry:    &site2its[4], // 5th entry is "test-8" and would appear first in descending order
			expectedCount: totalCount / 2,
			expectedTotal: cutil.GetPtr(totalCount / 2),
			expectedError: false,
		},
		{
			desc:          "GetAll with name search query returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr("test-"),
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:          "GetAll with name search query returns no objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr("cutting-"),
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with description search query returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr("Test Description"),
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:          "GetAll with display name search query returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr("Test Instance Type"),
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:          "GetAll with status search query returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(InstanceTypeStatusPending),
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:          "GetAll with InstanceTypeStatusPending status returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(InstanceTypeStatusPending),
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:          "GetAll with InstanceTypeStatusError status returns no objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(InstanceTypeStatusError),
			expectedCount: 0,
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(""),
			expectedCount: paginator.DefaultLimit,
			expectedError: false,
		},
		{
			desc:          "GetAll with label search query returns objects",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr("label-key-10"),
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns nothing when hiding empty allocations for tenants if no tenant IDs",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(""),
			tenantIDs:     []uuid.UUID{},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},
		{
			desc:          "GetAll with empty search query returns nothing when hiding empty allocations for tenants if unmatching tenant IDs",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(""),
			tenantIDs:     []uuid.UUID{uuid.New()},
			expectedCount: 0,
			expectedTotal: cutil.GetPtr(0),
			expectedError: false,
		},

		{
			desc:          "GetAll with empty search query returns the only instance type with allocations when hiding empty allocations for tenants if matching tenant IDs",
			itName:        nil,
			ipID:          nil,
			siteID:        nil,
			searchQuery:   cutil.GetPtr(""),
			tenantIDs:     []uuid.UUID{tenant1.ID},
			expectedCount: 1,
			expectedTotal: cutil.GetPtr(1),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			var siteIDs []uuid.UUID
			if tc.siteID != nil {
				siteIDs = []uuid.UUID{*tc.siteID}
			}
			got, total, err := itsd.GetAll(ctx, nil, InstanceTypeFilterInput{Name: tc.itName, TenantIDs: tc.tenantIDs, DisplayName: tc.itDisplayName, InfrastructureProviderID: tc.ipID, SiteIDs: siteIDs, Status: tc.status, SearchQuery: tc.searchQuery, InstanceTypeIDs: tc.ids}, tc.paramRelations, tc.offset, tc.limit, tc.orderBy)
			require.Equal(t, tc.expectedError, err != nil)

			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))

				if len(tc.paramRelations) > 0 {
					assert.NotNil(t, got[0].InfrastructureProvider)
					assert.NotNil(t, got[0].Site)
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
			}
		})
	}
}

func TestInstanceTypeSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testInstanceTypeSetupSchema(t, dbSession)
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceTypeBuildSite(t, dbSession, ip, "testSite")
	site2 := testInstanceTypeBuildSite(t, dbSession, ip, "testSite2")
	user := testInstanceTypeBuildUser(t, dbSession, "testUser")
	itsd := NewInstanceTypeDAO(dbSession)
	infinityResourceTypeID := uuid.New()
	it1, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test1", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeID), SiteID: &site.ID, Labels: map[string]string{"test1": "test1"}, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)
	assert.NotNil(t, it1)
	infinityResourceTypeIDUpdated := uuid.New()

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	version := "anything"

	tests := []struct {
		desc                           string
		paramName                      *string
		paramDisplayName               *string
		paramDescription               *string
		paramInfinityResourceTypeID    *uuid.UUID
		paramSiteID                    *uuid.UUID
		paramLabels                    map[string]string
		paramStatus                    *string
		paramVersion                   *string
		expectedName                   *string
		expectedDisplayName            *string
		expectedDescription            *string
		expectedInfinityResourceTypeID *uuid.UUID
		expectedSiteID                 *uuid.UUID
		expectedLabels                 Labels
		expectedStatus                 *string
		expectedVersion                *string
		verifyChildSpanner             bool
	}{
		{
			desc:                           "can update name",
			paramName:                      cutil.GetPtr("updatedName"),
			paramDisplayName:               nil,
			paramDescription:               nil,
			paramSiteID:                    nil,
			paramStatus:                    nil,
			expectedName:                   cutil.GetPtr("updatedName"),
			expectedDisplayName:            cutil.GetPtr("displayName"),
			expectedDescription:            cutil.GetPtr("description"),
			expectedInfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeID),
			expectedSiteID:                 &site.ID,
			expectedStatus:                 cutil.GetPtr(InstanceTypeStatusPending),
			verifyChildSpanner:             true,
		},
		{
			desc:                           "can update display_name",
			paramName:                      nil,
			paramDisplayName:               cutil.GetPtr("updatedDisplayName"),
			paramDescription:               nil,
			paramSiteID:                    nil,
			paramStatus:                    nil,
			expectedName:                   cutil.GetPtr("updatedName"),
			expectedDisplayName:            cutil.GetPtr("updatedDisplayName"),
			expectedDescription:            cutil.GetPtr("description"),
			expectedInfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeID),
			expectedSiteID:                 &site.ID,
			expectedStatus:                 cutil.GetPtr(InstanceTypeStatusPending),
		},
		{
			desc:                           "can update description",
			paramName:                      nil,
			paramDisplayName:               nil,
			paramDescription:               cutil.GetPtr("updatedDescription"),
			paramSiteID:                    nil,
			paramStatus:                    nil,
			expectedName:                   cutil.GetPtr("updatedName"),
			expectedDisplayName:            cutil.GetPtr("updatedDisplayName"),
			expectedDescription:            cutil.GetPtr("updatedDescription"),
			expectedInfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeID),
			expectedSiteID:                 &site.ID,
			expectedStatus:                 cutil.GetPtr(InstanceTypeStatusPending),
		},
		{
			desc:                           "can update site_id",
			paramName:                      nil,
			paramDisplayName:               nil,
			paramDescription:               nil,
			paramSiteID:                    &site2.ID,
			paramStatus:                    nil,
			expectedName:                   cutil.GetPtr("updatedName"),
			expectedDisplayName:            cutil.GetPtr("updatedDisplayName"),
			expectedDescription:            cutil.GetPtr("updatedDescription"),
			expectedInfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeID),
			expectedSiteID:                 &site2.ID,
			expectedStatus:                 cutil.GetPtr(InstanceTypeStatusPending),
		},
		{
			desc:                "can update labels",
			paramName:           nil,
			paramDisplayName:    nil,
			paramDescription:    nil,
			paramSiteID:         nil,
			paramStatus:         nil,
			paramLabels:         map[string]string{"test2": "test2"},
			expectedName:        cutil.GetPtr("updatedName"),
			expectedDisplayName: cutil.GetPtr("updatedDisplayName"),
			expectedDescription: cutil.GetPtr("updatedDescription"),
			expectedSiteID:      &site2.ID,
			expectedLabels:      Labels{"test2": "test2"},
			expectedStatus:      cutil.GetPtr(InstanceTypeStatusPending),
		},
		{
			desc:                "can update status",
			paramName:           nil,
			paramDisplayName:    nil,
			paramDescription:    nil,
			paramSiteID:         nil,
			paramStatus:         cutil.GetPtr(InstanceTypeStatusReady),
			expectedName:        cutil.GetPtr("updatedName"),
			expectedDisplayName: cutil.GetPtr("updatedDisplayName"),
			expectedDescription: cutil.GetPtr("updatedDescription"),
			expectedSiteID:      &site2.ID,
			expectedStatus:      cutil.GetPtr(InstanceTypeStatusReady),
		},
		{
			desc:                           "can multiple fields at once",
			paramName:                      cutil.GetPtr("name"),
			paramDisplayName:               cutil.GetPtr("displayName"),
			paramDescription:               cutil.GetPtr("description"),
			paramInfinityResourceTypeID:    cutil.GetPtr(infinityResourceTypeIDUpdated),
			paramSiteID:                    &site.ID,
			paramStatus:                    cutil.GetPtr(InstanceTypeStatusPending),
			paramVersion:                   &version,
			expectedName:                   cutil.GetPtr("name"),
			expectedDisplayName:            cutil.GetPtr("displayName"),
			expectedDescription:            cutil.GetPtr("description"),
			expectedInfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeIDUpdated),
			expectedSiteID:                 &site.ID,
			expectedVersion:                &version,
			expectedStatus:                 cutil.GetPtr(InstanceTypeStatusPending),
		},
		{
			desc:                           "no fields are updated",
			paramName:                      nil,
			paramDisplayName:               nil,
			paramDescription:               nil,
			paramSiteID:                    nil,
			paramInfinityResourceTypeID:    nil,
			paramStatus:                    nil,
			expectedName:                   cutil.GetPtr("name"),
			expectedDisplayName:            cutil.GetPtr("displayName"),
			expectedDescription:            cutil.GetPtr("description"),
			expectedInfinityResourceTypeID: cutil.GetPtr(infinityResourceTypeIDUpdated),
			expectedSiteID:                 &site.ID,
			expectedStatus:                 cutil.GetPtr(InstanceTypeStatusPending),
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {

			got, err := itsd.Update(ctx, nil, InstanceTypeUpdateInput{
				ID:                     it1.ID,
				Name:                   tc.paramName,
				DisplayName:            tc.paramDisplayName,
				Description:            tc.paramDescription,
				SiteID:                 tc.paramSiteID,
				Labels:                 tc.paramLabels,
				InfinityResourceTypeID: tc.paramInfinityResourceTypeID,
				Status:                 tc.paramStatus,
				Version:                tc.paramVersion,
			})

			assert.Nil(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, *tc.expectedName, got.Name)
			assert.Equal(t, *tc.expectedDisplayName, *got.DisplayName)
			assert.Equal(t, *tc.expectedDescription, *got.Description)
			if tc.expectedInfinityResourceTypeID != nil {
				assert.Equal(t, *tc.expectedInfinityResourceTypeID, *got.InfinityResourceTypeID)
			}
			assert.Equal(t, *tc.expectedSiteID, *got.SiteID)
			if tc.expectedLabels != nil {
				assert.Equal(t, tc.expectedLabels, got.Labels)
			}
			assert.Equal(t, *tc.expectedStatus, got.Status)
			assert.True(t, tc.expectedVersion == nil || got.Version == *tc.expectedVersion)

			if got.Updated.String() == it1.Updated.String() {
				t.Errorf("got.Updated = %v, want different value", got.Updated)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInstanceTypeSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testInstanceTypeSetupSchema(t, dbSession)
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceTypeBuildSite(t, dbSession, ip, "testSite")
	user := testInstanceTypeBuildUser(t, dbSession, "testUser")
	itsd := NewInstanceTypeDAO(dbSession)
	it1, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test1", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site.ID, Labels: map[string]string{"test1": "test1"}, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)
	assert.NotNil(t, it1)
	it2, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test2", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site.ID, Labels: map[string]string{"test2": "test2"}, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)
	assert.NotNil(t, it2)
	it3, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test3", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site.ID, Labels: map[string]string{"test3": "test3"}, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)
	assert.NotNil(t, it3)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                string
		it                  *InstanceType
		paramDisplayName    bool
		paramDescription    bool
		paramSiteID         bool
		paramLabels         bool
		expectedUpdate      bool
		expectedDisplayName *string
		expectedDescription *string
		expectedSiteID      *uuid.UUID
		expectedLabels      Labels
		verifyChildSpanner  bool
	}{
		{
			desc:                "can clear display_name",
			it:                  it1,
			paramDisplayName:    true,
			paramDescription:    false,
			paramSiteID:         false,
			expectedUpdate:      true,
			expectedDisplayName: nil,
			expectedDescription: cutil.GetPtr("description"),
			expectedSiteID:      &site.ID,
			verifyChildSpanner:  true,
		},
		{
			desc:                "can clear description",
			it:                  it1,
			paramDisplayName:    false,
			paramDescription:    true,
			paramSiteID:         false,
			expectedUpdate:      true,
			expectedDisplayName: nil,
			expectedDescription: nil,
			expectedSiteID:      &site.ID,
		},
		{
			desc:                "can clear site",
			it:                  it1,
			paramDisplayName:    false,
			paramDescription:    false,
			paramSiteID:         true,
			expectedUpdate:      true,
			expectedDisplayName: nil,
			expectedDescription: nil,
			expectedSiteID:      nil,
			expectedLabels:      Labels{"test1": "test1"},
		},
		{
			desc:             "can clear labels",
			it:               it1,
			paramDisplayName: false,
			paramDescription: false,
			paramSiteID:      false,
			paramLabels:      true,
			expectedUpdate:   true,
			expectedLabels:   nil,
		},
		{
			desc:                "can clear multiple fields at once",
			it:                  it2,
			paramDisplayName:    true,
			paramDescription:    true,
			paramSiteID:         true,
			expectedUpdate:      true,
			expectedDisplayName: nil,
			expectedDescription: nil,
			expectedSiteID:      nil,
		},
		{
			desc:                "can clear multiple fields at once",
			it:                  it2,
			paramDisplayName:    true,
			paramDescription:    true,
			paramSiteID:         true,
			expectedUpdate:      true,
			expectedDisplayName: nil,
			expectedDescription: nil,
			expectedSiteID:      nil,
		},
		{
			desc:                "nop when no cleared fields are specified",
			it:                  it3,
			paramDisplayName:    false,
			paramDescription:    false,
			paramSiteID:         false,
			expectedDisplayName: cutil.GetPtr("displayName"),
			expectedDescription: cutil.GetPtr("description"),
			expectedSiteID:      &site.ID,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := itsd.Clear(ctx, nil, InstanceTypeClearInput{InstanceTypeID: tc.it.ID, DisplayName: tc.paramDisplayName, Description: tc.paramDescription, SiteID: tc.paramSiteID, Labels: tc.paramLabels})
			assert.Nil(t, err)
			assert.NotNil(t, tmp)
			assert.Equal(t, tc.expectedDisplayName == nil, tmp.DisplayName == nil)
			if tc.expectedDisplayName != nil {
				assert.Equal(t, *tc.expectedDisplayName, *tmp.DisplayName)
			}
			assert.Equal(t, tc.expectedDescription == nil, tmp.Description == nil)
			if tc.expectedDescription != nil {
				assert.Equal(t, *tc.expectedDescription, *tmp.Description)
			}
			assert.Equal(t, tc.expectedSiteID == nil, tmp.SiteID == nil)
			if tc.expectedSiteID != nil {
				assert.Equal(t, *tc.expectedSiteID, *tmp.SiteID)
			}
			if tc.expectedLabels != nil {
				assert.Equal(t, tc.expectedLabels, tmp.Labels)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.it.Updated))
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestInstanceTypeSQLDAO_DeleteByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceTypeInitDB(t)
	defer dbSession.Close()
	testInstanceTypeSetupSchema(t, dbSession)
	ip := testInstanceTypeBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceTypeBuildSite(t, dbSession, ip, "testSite")
	user := testInstanceTypeBuildUser(t, dbSession, "testUser")
	itsd := NewInstanceTypeDAO(dbSession)
	it1, err := itsd.Create(
		ctx, nil, InstanceTypeCreateInput{Name: "test1", DisplayName: cutil.GetPtr("displayName"), Description: cutil.GetPtr("description"), ControllerMachineType: cutil.GetPtr("controllerMachineType"),
			InfrastructureProviderID: ip.ID, InfinityResourceTypeID: cutil.GetPtr(uuid.New()), SiteID: &site.ID, Status: InstanceTypeStatusPending, CreatedBy: user.ID},
	)
	assert.Nil(t, err)
	assert.NotNil(t, it1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		itID               uuid.UUID
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:          "can delete existing object",
			itID:          it1.ID,
			expectedError: false,
		},
		{
			desc:          "delete non-existing object",
			itID:          uuid.New(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := itsd.DeleteByID(ctx, nil, tc.itID)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := itsd.GetByID(ctx, nil, tc.itID, nil)
				assert.NotNil(t, err)
				assert.Nil(t, tmp)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}
