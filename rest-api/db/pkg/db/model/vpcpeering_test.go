// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

var (
	mockID = uuid.MustParse("123e4567-e89b-12d3-a456-426614174000")
)

func TestVpcPeering_ToProto(t *testing.T) {
	id := uuid.New()
	v1 := uuid.New()
	v2 := uuid.New()
	vp := &VpcPeering{ID: id, Vpc1ID: v1, Vpc2ID: v2}
	got := vp.ToProto()
	require.NotNil(t, got)
	require.NotNil(t, got.Id)
	assert.Equal(t, id.String(), got.Id.Value)
	require.NotNil(t, got.VpcId)
	assert.Equal(t, v1.String(), got.VpcId.Value)
	require.NotNil(t, got.PeerVpcId)
	assert.Equal(t, v2.String(), got.PeerVpcId.Value)
}

func TestVpcPeering_FromProto(t *testing.T) {
	t.Run("nil proto is a no-op", func(t *testing.T) {
		origID := uuid.New()
		origV1 := uuid.New()
		origV2 := uuid.New()
		vp := &VpcPeering{ID: origID, Vpc1ID: origV1, Vpc2ID: origV2}
		vp.FromProto(nil)
		assert.Equal(t, origID, vp.ID)
		assert.Equal(t, origV1, vp.Vpc1ID)
		assert.Equal(t, origV2, vp.Vpc2ID)
	})

	t.Run("populates all fields from a complete proto", func(t *testing.T) {
		id := uuid.New()
		v1 := uuid.New()
		v2 := uuid.New()
		vp := &VpcPeering{}
		vp.FromProto(&corev1.VpcPeering{
			Id:        &corev1.VpcPeeringId{Value: id.String()},
			VpcId:     &corev1.VpcId{Value: v1.String()},
			PeerVpcId: &corev1.VpcId{Value: v2.String()},
		})
		assert.Equal(t, id, vp.ID)
		assert.Equal(t, v1, vp.Vpc1ID)
		assert.Equal(t, v2, vp.Vpc2ID)
	})

	t.Run("preserves required IDs when proto omits or carries unparseable values", func(t *testing.T) {
		origID := uuid.New()
		origV1 := uuid.New()
		origV2 := uuid.New()
		vp := &VpcPeering{ID: origID, Vpc1ID: origV1, Vpc2ID: origV2}
		vp.FromProto(&corev1.VpcPeering{
			Id:        &corev1.VpcPeeringId{Value: "not-a-uuid"},
			VpcId:     nil,
			PeerVpcId: &corev1.VpcId{Value: "also-bad"},
		})
		assert.Equal(t, origID, vp.ID)
		assert.Equal(t, origV1, vp.Vpc1ID)
		assert.Equal(t, origV2, vp.Vpc2ID)
	})

	t.Run("round-trips through ToProto/FromProto", func(t *testing.T) {
		orig := &VpcPeering{
			ID:     uuid.New(),
			Vpc1ID: uuid.New(),
			Vpc2ID: uuid.New(),
		}
		round := &VpcPeering{}
		round.FromProto(orig.ToProto())
		assert.Equal(t, orig.ID, round.ID)
		assert.Equal(t, orig.Vpc1ID, round.Vpc1ID)
		assert.Equal(t, orig.Vpc2ID, round.Vpc2ID)
	})
}

func TestVpcPeering_ToDeletionRequestProto(t *testing.T) {
	id := uuid.New()
	vp := &VpcPeering{ID: id}
	req := vp.ToDeletionRequestProto()
	require.NotNil(t, req)
	require.NotNil(t, req.Id)
	assert.Equal(t, id.String(), req.Id.Value)
}

func testVpcPeeringSetupSchema(t *testing.T, dbSession *db.Session) {
	testInterfaceSetupSchema(t, dbSession)

	err := dbSession.DB.ResetModel(context.Background(), (*VpcPeering)(nil))
	assert.Nil(t, err)
}

func TestVpcPeeringSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant1 := testInstanceBuildTenant(t, dbSession, "testTenant1")
	tenant2 := testInstanceBuildTenant(t, dbSession, "testTenant2")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant1, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant1, "testVpc2")
	vpc3 := testInstanceBuildVpc(t, dbSession, ip, site, tenant1, "testVpc3")
	vpc4 := testInstanceBuildVpc(t, dbSession, ip, site, tenant2, "testVpc4")

	vpsd := NewVpcPeeringDAO(dbSession)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)
	explicitVpcPeeringID := uuid.New()

	tests := []struct {
		desc               string
		vps                []VpcPeering
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "Create multiple VPC peerings",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc2.ID, SiteID: site.ID, IsMultiTenant: false, CreatedBy: user.ID,
				},
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc3.ID, SiteID: site.ID, IsMultiTenant: false, CreatedBy: user.ID,
				},
				{
					Vpc1ID: vpc2.ID, Vpc2ID: vpc3.ID, SiteID: site.ID, IsMultiTenant: false, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "Create succeeds with IsMultiTenant=true",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc4.ID, SiteID: site.ID, IsMultiTenant: true, InfrastructureProviderID: &ip.ID, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "Create succeeds with TenantID set",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc2.ID, SiteID: site.ID, IsMultiTenant: false, TenantID: &tenant1.ID, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "Create succeeds with supplied ID and status",
			vps: []VpcPeering{
				{
					ID: explicitVpcPeeringID, Vpc1ID: vpc2.ID, Vpc2ID: vpc4.ID, SiteID: site.ID, IsMultiTenant: true, InfrastructureProviderID: &ip.ID, Status: VpcPeeringStatusReady, CreatedBy: user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "Create fails due to constraint that two VPC ids cannot be the same",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc1.ID, SiteID: site.ID, IsMultiTenant: false, CreatedBy: user.ID,
				},
			},
			expectError:        true,
			verifyChildSpanner: true,
		},
		{
			desc: "Create fails due to foreign key constraint on Vpc1ID",
			vps: []VpcPeering{
				{
					Vpc1ID: mockID, Vpc2ID: vpc1.ID, SiteID: site.ID, IsMultiTenant: false, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "Create fails due to foreign key constraint on Vpc2ID",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: mockID, SiteID: site.ID, IsMultiTenant: false, CreatedBy: user.ID,
				},
			},
			expectError:        true,
			verifyChildSpanner: true,
		},
		{
			desc: "Create fails due to foreign key constraint on InfrastructureProviderID",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc2.ID, SiteID: site.ID, IsMultiTenant: true, InfrastructureProviderID: &mockID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
		{
			desc: "Create fails due to foreign key constraint on TenantID",
			vps: []VpcPeering{
				{
					Vpc1ID: vpc1.ID, Vpc2ID: vpc2.ID, SiteID: site.ID, IsMultiTenant: false, TenantID: &mockID, CreatedBy: user.ID,
				},
			},
			expectError: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {

			for _, i := range tc.vps {
				var vpcPeeringID *uuid.UUID
				if i.ID != uuid.Nil {
					vpcPeeringID = &i.ID
				}

				got, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
					VpcPeeringID:             vpcPeeringID,
					Vpc1ID:                   i.Vpc1ID,
					Vpc2ID:                   i.Vpc2ID,
					SiteID:                   i.SiteID,
					IsMultiTenant:            i.IsMultiTenant,
					InfrastructureProviderID: i.InfrastructureProviderID,
					TenantID:                 i.TenantID,
					Status:                   i.Status,
					CreatedByID:              i.CreatedBy,
				})
				assert.Equal(t, tc.expectError, err != nil)
				if !tc.expectError {
					assert.NotNil(t, got)
					if i.ID != uuid.Nil {
						assert.Equal(t, i.ID, got.ID)
					}
					assert.Equal(t, i.SiteID, got.SiteID)
					assert.Equal(t, i.IsMultiTenant, got.IsMultiTenant)
					assert.Equal(t, i.InfrastructureProviderID, got.InfrastructureProviderID)
					assert.Equal(t, i.TenantID, got.TenantID)
					assert.Equal(t, i.CreatedBy, got.CreatedBy)

					expectedStatus := i.Status
					if expectedStatus == "" {
						expectedStatus = VpcPeeringStatusPending
					}
					assert.Equal(t, expectedStatus, got.Status)
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

func TestVpcPeeringSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	tenant2 := testInstanceBuildTenant(t, dbSession, "testTenant2")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")
	vpc3 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc3")
	vpc4 := testInstanceBuildVpc(t, dbSession, ip, site, tenant2, "testVpc4")

	vpsd := NewVpcPeeringDAO(dbSession)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	vp12, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID:                   vpc1.ID,
		Vpc2ID:                   vpc2.ID,
		SiteID:                   site.ID,
		IsMultiTenant:            false,
		InfrastructureProviderID: nil,
		TenantID:                 &tenant.ID,
		CreatedByID:              user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp12)

	vp13, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID:                   vpc1.ID,
		Vpc2ID:                   vpc3.ID,
		SiteID:                   site.ID,
		IsMultiTenant:            false,
		InfrastructureProviderID: nil,
		TenantID:                 &tenant.ID,
		CreatedByID:              user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp13)

	vp23, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc2.ID,
		Vpc2ID: vpc3.ID,
		SiteID: site.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp23)

	vp14, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID:                   vpc1.ID,
		Vpc2ID:                   vpc4.ID,
		SiteID:                   site.ID,
		IsMultiTenant:            true,
		InfrastructureProviderID: &ip.ID,
		TenantID:                 nil,
		CreatedByID:              user.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp14)

	tests := []struct {
		desc string

		paramOffset  *int
		paramLimit   *int
		paramOrderBy *paginator.OrderBy

		ids                       []uuid.UUID
		vpcIDs                    []uuid.UUID
		siteIDs                   []uuid.UUID
		isMultiTenant             *bool
		infrastructureProviderIDs []uuid.UUID
		tenantIDs                 []uuid.UUID
		peerTenantIDs             []uuid.UUID
		statuses                  []string

		expectError bool
		expectCount int

		includeRelations []string

		verifyChildSpanner bool
	}{
		{
			desc:               "GetAll with no filters",
			ids:                nil,
			vpcIDs:             nil,
			siteIDs:            nil,
			statuses:           nil,
			expectError:        false,
			expectCount:        4,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with filters on IDs",
			ids:                []uuid.UUID{vp12.ID, vp13.ID},
			vpcIDs:             nil,
			siteIDs:            nil,
			statuses:           nil,
			expectError:        false,
			expectCount:        2,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with filters on VpcIDs",
			ids:                nil,
			vpcIDs:             []uuid.UUID{vpc1.ID},
			siteIDs:            nil,
			statuses:           nil,
			expectError:        false,
			expectCount:        3,
			verifyChildSpanner: true,
		},
		{
			desc:                      "GetAll with filters on IsMultiTenant",
			isMultiTenant:             cutil.GetPtr(true),
			infrastructureProviderIDs: nil,
			tenantIDs:                 nil,
			expectError:               false,
			expectCount:               1,
			verifyChildSpanner:        true,
		},
		{
			desc:                      "GetAll with filters on InfrastructureProviderIDs",
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			expectError:               false,
			expectCount:               1,
			verifyChildSpanner:        true,
		},
		{
			desc:                      "GetAll with filters on TenantIDs and InfrastructureProviderIDs",
			tenantIDs:                 []uuid.UUID{tenant.ID},
			infrastructureProviderIDs: []uuid.UUID{ip.ID},
			expectError:               false,
			expectCount:               4,
			verifyChildSpanner:        true,
		},
		{
			desc:               "GetAll with filters on TenantIDs",
			tenantIDs:          []uuid.UUID{tenant2.ID},
			expectError:        false,
			expectCount:        1,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with filters on PeerTenantIDs",
			peerTenantIDs:      []uuid.UUID{tenant2.ID},
			expectError:        false,
			expectCount:        1,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with filters on TenantIDs and PeerTenantIDs",
			tenantIDs:          []uuid.UUID{tenant.ID},
			peerTenantIDs:      []uuid.UUID{tenant2.ID},
			expectError:        false,
			expectCount:        1,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with filters on pending statuses",
			ids:                nil,
			vpcIDs:             nil,
			siteIDs:            nil,
			statuses:           []string{VpcPeeringStatusPending},
			expectError:        false,
			expectCount:        4,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with filters on deleting statuses",
			ids:                nil,
			vpcIDs:             nil,
			statuses:           []string{VpcPeeringStatusDeleting},
			expectError:        false,
			expectCount:        0,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with empty IDs",
			ids:                []uuid.UUID{},
			vpcIDs:             nil,
			statuses:           []string{VpcPeeringStatusDeleting},
			expectError:        false,
			expectCount:        0,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with empty statuses",
			ids:                nil,
			vpcIDs:             nil,
			statuses:           []string{},
			expectError:        false,
			expectCount:        0,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetAll with empty site IDs",
			ids:                nil,
			vpcIDs:             nil,
			siteIDs:            []uuid.UUID{},
			statuses:           nil,
			expectError:        false,
			expectCount:        0,
			verifyChildSpanner: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, count, err := vpsd.GetAll(
				ctx,
				nil,
				VpcPeeringFilterInput{
					IDs:                       tc.ids,
					VpcIDs:                    tc.vpcIDs,
					SiteIDs:                   tc.siteIDs,
					IsMultiTenant:             tc.isMultiTenant,
					InfrastructureProviderIDs: tc.infrastructureProviderIDs,
					TenantIDs:                 tc.tenantIDs,
					PeerTenantIDs:             tc.peerTenantIDs,
					Statuses:                  tc.statuses,
				},
				paginator.PageInput{Offset: tc.paramOffset, Limit: tc.paramLimit, OrderBy: tc.paramOrderBy}, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				assert.Equal(t, tc.expectCount, count)
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

func TestVpcPeeringSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")

	vpsd := NewVpcPeeringDAO(dbSession)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	vp12, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc2.ID,
		SiteID: site.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp12)

	tests := []struct {
		desc string

		id uuid.UUID

		expectError bool

		includeRelations   []string
		verifyChildSpanner bool
	}{
		{
			desc:               "GetByID",
			id:                 vp12.ID,
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:               "GetByID fails for non-existent ID",
			id:                 mockID,
			expectError:        true,
			verifyChildSpanner: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := vpsd.GetByID(
				ctx,
				nil,
				tc.id,
				tc.includeRelations,
			)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
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

func TestVpcPeeringSQLDAO_UpdateStatusByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")

	vpsd := NewVpcPeeringDAO(dbSession)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	vp, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc2.ID,
		SiteID: site.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp)
	assert.Equal(t, vp.Status, VpcPeeringStatusPending)

	originalTS := vp.Updated
	originalStatus := vp.Status

	// Test updating status to valid status
	err = vpsd.UpdateStatusByID(ctx, nil, vp.ID, VpcPeeringStatusConfiguring)
	assert.NoError(t, err)
	updatedVP, err := vpsd.GetByID(ctx, nil, vp.ID, nil)
	assert.NoError(t, err)
	assert.NotEqual(t, originalStatus, updatedVP.Status)
	assert.True(t, updatedVP.Updated.After(originalTS))

	// Test updating status to invalid status string
	err = vpsd.UpdateStatusByID(ctx, nil, vp.ID, "invalid_status")
	assert.Error(t, err)
}

func TestVpcPeeringSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testVpcPeeringSetupSchema(t, dbSession)

	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")
	vpcPeeringDAO := NewVpcPeeringDAO(dbSession)

	vpcPeering, err := vpcPeeringDAO.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID:      vpc1.ID,
		Vpc2ID:      vpc2.ID,
		SiteID:      site.ID,
		TenantID:    &tenant.ID,
		Status:      VpcPeeringStatusDeleting,
		CreatedByID: user.ID,
	})
	require.NoError(t, err)
	require.NoError(t, vpcPeeringDAO.Delete(ctx, nil, vpcPeering.ID))

	deleted, _, err := vpcPeeringDAO.GetAll(
		ctx,
		nil,
		VpcPeeringFilterInput{
			IDs:            []uuid.UUID{vpcPeering.ID},
			IncludeDeleted: true,
		},
		paginator.PageInput{},
		nil,
	)
	require.NoError(t, err)
	require.Len(t, deleted, 1)
	require.NotNil(t, deleted[0].Deleted)

	tests := []struct {
		name          string
		vpcPeeringID  uuid.UUID
		expectedError error
	}{
		{
			name:         "clears soft-delete marker",
			vpcPeeringID: vpcPeering.ID,
		},
		{
			name:          "returns not found for unknown VPC Peering",
			vpcPeeringID:  uuid.New(),
			expectedError: db.ErrDoesNotExist,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			cleared, clearErr := vpcPeeringDAO.Clear(ctx, nil, VpcPeeringClearInput{
				VpcPeeringID: test.vpcPeeringID,
				Deleted:      true,
			})
			if test.expectedError != nil {
				require.ErrorIs(t, clearErr, test.expectedError)
				assert.Nil(t, cleared)
				return
			}

			require.NoError(t, clearErr)
			require.NotNil(t, cleared)
			assert.Nil(t, cleared.Deleted)
			assert.Equal(t, VpcPeeringStatusDeleting, cleared.Status)
		})
	}
}

func TestVpcPeeringSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")

	vpsd := NewVpcPeeringDAO(dbSession)

	// Create a VPC peering
	vp, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc2.ID,
		SiteID: site.ID,
	})
	assert.NoError(t, err)
	assert.NotNil(t, vp)
	assert.Equal(t, vp.Status, VpcPeeringStatusPending)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc          string
		id            uuid.UUID
		expectedError bool
	}{
		{
			desc:          "delete existing peering",
			id:            vp.ID,
			expectedError: false,
		},
		{
			desc:          "delete non-existing peering",
			id:            uuid.New(),
			expectedError: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := vpsd.Delete(ctx, nil, tc.id)
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				// Check soft-deleted entry
				deleted := &VpcPeering{}
				err = dbSession.DB.NewSelect().
					Model(deleted).
					Where("id = ?", vp.ID).
					WhereDeleted().
					Scan(ctx)
				assert.NoError(t, err)
				assert.NotNil(t, deleted.Deleted)
			}
		})
	}
}

func TestVpcPeeringSQLDAO_DeleteByVpcID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")
	vpc3 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc3")
	vpc4 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc4")
	vpc5 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc5")

	vpsd := NewVpcPeeringDAO(dbSession)

	// Create a VPC peering
	vp12, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc2.ID,
		SiteID: site.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp12)
	vp13, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc3.ID,
		SiteID: site.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp13)
	vp23, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc2.ID,
		Vpc2ID: vpc3.ID,
		SiteID: site.ID,
	})
	assert.Nil(t, err)
	assert.NotNil(t, vp23)
	vp45, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc4.ID,
		Vpc2ID: vpc5.ID,
		SiteID: site.ID,
	})
	assert.NoError(t, err)

	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc          string
		vpcID         uuid.UUID
		expectedError bool
		vpIDs         []uuid.UUID
	}{
		{
			desc:          "delete peerings involving VPC4 (single peering with VPC5)",
			vpcID:         vpc4.ID,
			expectedError: false,
			vpIDs:         []uuid.UUID{vp45.ID},
		},
		{
			desc:          "delete all peerings involving VPC1 (peerings with VPC2 and VPC3)",
			vpcID:         vpc1.ID,
			expectedError: false,
			vpIDs:         []uuid.UUID{vp12.ID, vp13.ID},
		},
		{
			desc:          "delete all peerings involving VPC2 (peerings with VPC1 and VPC3)",
			vpcID:         vpc2.ID,
			expectedError: false,
			vpIDs:         []uuid.UUID{vp12.ID, vp23.ID},
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := vpsd.DeleteByVpcID(ctx, nil, tc.vpcID)
			assert.Equal(t, tc.expectedError, err != nil)
			if tc.vpIDs != nil {
				for _, vpID := range tc.vpIDs {
					// Check soft-deleted entry
					deleted := &VpcPeering{}
					err = dbSession.DB.NewSelect().
						Model(deleted).
						Where("id = ?", vpID).
						WhereDeleted().
						Scan(ctx)
					assert.NoError(t, err)
					assert.NotNil(t, deleted.Deleted)
				}
			}
		})
	}
}

func TestVpcPeeringSQLDAO_RecreateAfterDeletion(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()

	testVpcPeeringSetupSchema(t, dbSession)

	testVpcPeeringSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	vpc1 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc1")
	vpc2 := testInstanceBuildVpc(t, dbSession, ip, site, tenant, "testVpc2")

	vpsd := NewVpcPeeringDAO(dbSession)

	// Create a VPC peering
	vp12, err := vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc2.ID,
		SiteID: site.ID,
	})
	assert.NoError(t, err)

	err = vpsd.Delete(ctx, nil, vp12.ID)
	assert.NoError(t, err)

	deleted := &VpcPeering{}
	err = dbSession.DB.NewSelect().
		Model(deleted).
		Where("id = ?", vp12.ID).
		WhereDeleted().
		Scan(ctx)
	assert.NoError(t, err)
	assert.NotNil(t, deleted.Deleted)

	vp12, err = vpsd.Create(ctx, nil, VpcPeeringCreateInput{
		Vpc1ID: vpc1.ID,
		Vpc2ID: vpc2.ID,
		SiteID: site.ID,
	})
	assert.NoError(t, err)

	err = vpsd.Delete(ctx, nil, vp12.ID)
	assert.NoError(t, err)

	var deletedPeerings []VpcPeering
	err = dbSession.DB.NewSelect().
		Model(&deletedPeerings).
		Where("vpc1_id = ? AND vpc2_id = ?", vpc1.ID, vpc2.ID).
		WhereDeleted().
		Scan(ctx)
	assert.NoError(t, err)
	assert.Equal(t, len(deletedPeerings), 2)

}
