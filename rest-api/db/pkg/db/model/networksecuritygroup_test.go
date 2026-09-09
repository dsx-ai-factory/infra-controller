// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"fmt"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"
	"google.golang.org/protobuf/proto"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func getIntPtrToUint32Ptr(i *int) *uint32 {
	if i == nil {
		return nil
	}

	i32 := uint32(*i)

	return &i32
}

// reset the tables needed for NetworkSecurityGroup tests
func testNetworkSecurityGroupSetupSchema(t *testing.T, dbSession *db.Session) {
	// interface setup covers all tables needed
	testInterfaceSetupSchema(t, dbSession)
	// create Security Group table
	err := dbSession.DB.ResetModel(context.Background(), (*NetworkSecurityGroup)(nil))
	assert.Nil(t, err)
}

func TestNetworkSecurityGroupPropagationDetails_Equal(t *testing.T) {
	mk := func() *NetworkSecurityGroupPropagationDetails {
		return &NetworkSecurityGroupPropagationDetails{
			FriendlyStatus: "Propagated",
			NetworkSecurityGroupPropagationObjectStatus: &corev1.NetworkSecurityGroupPropagationObjectStatus{
				Status:                  corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_FULL,
				Details:                 cutil.GetPtr("ok"),
				UnpropagatedInstanceIds: []string{"i-1"},
				RelatedInstanceIds:      []string{"i-2"},
			},
		}
	}

	t.Run("nil equals nil", func(t *testing.T) {
		var a, b *NetworkSecurityGroupPropagationDetails
		assert.True(t, a.Equal(b))
	})
	t.Run("nil does not equal non-nil", func(t *testing.T) {
		var a *NetworkSecurityGroupPropagationDetails
		b := mk()
		assert.False(t, a.Equal(b))
		assert.False(t, b.Equal(a))
	})
	t.Run("identical wrappers are equal", func(t *testing.T) {
		assert.True(t, mk().Equal(mk()))
	})
	t.Run("differing Status returns false", func(t *testing.T) {
		a := mk()
		b := mk()
		b.Status = corev1.NetworkSecurityGroupPropagationStatus_NSG_PROP_STATUS_ERROR
		assert.False(t, a.Equal(b))
	})
	t.Run("differing Details returns false", func(t *testing.T) {
		a := mk()
		b := mk()
		b.Details = cutil.GetPtr("changed")
		assert.False(t, a.Equal(b))
	})
	t.Run("nil-vs-set Details returns false", func(t *testing.T) {
		a := mk()
		b := mk()
		b.Details = nil
		assert.False(t, a.Equal(b))
	})
	t.Run("differing UnpropagatedInstanceIds returns false", func(t *testing.T) {
		a := mk()
		b := mk()
		b.UnpropagatedInstanceIds = []string{"i-9"}
		assert.False(t, a.Equal(b))
	})
}

func TestNetworkSecurityGroupSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testNetworkSecurityGroupSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	sgsd := NewNetworkSecurityGroupDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	labels := map[string]string{}

	labels["key"] = "value"

	rule := &NetworkSecurityGroupRule{
		&corev1.NetworkSecurityGroupRuleAttributes{
			Id:             cutil.GetPtr(uuid.NewString()),
			Direction:      corev1.NetworkSecurityGroupRuleDirection_NSG_RULE_DIRECTION_EGRESS,
			Protocol:       corev1.NetworkSecurityGroupRuleProtocol_NSG_RULE_PROTO_ANY,
			Action:         corev1.NetworkSecurityGroupRuleAction_NSG_RULE_ACTION_DENY,
			Priority:       55,
			Ipv6:           false, // We have support for it in ACLs but pretty much nowhere else, so we hide this for now.
			SrcPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(55)),
			SrcPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(56)),
			DstPortStart:   getIntPtrToUint32Ptr(cutil.GetPtr(57)),
			DstPortEnd:     getIntPtrToUint32Ptr(cutil.GetPtr(58)),
			SourceNet:      &corev1.NetworkSecurityGroupRuleAttributes_SrcPrefix{SrcPrefix: "0.0.0.0/0"},
			DestinationNet: &corev1.NetworkSecurityGroupRuleAttributes_DstPrefix{DstPrefix: "1.1.1.1/0"},
		},
	}

	rules := []*NetworkSecurityGroupRule{
		rule,
	}

	badRules := []*NetworkSecurityGroupRule{
		rule,
		nil,
	}

	tests := []struct {
		desc               string
		sg                 NetworkSecurityGroup
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "success - create one with all fields set",
			sg: NetworkSecurityGroup{
				Version:        "anything",
				ID:             uuid.NewString(),
				Name:           "test",
				Description:    cutil.GetPtr("test"),
				SiteID:         site.ID,
				TenantOrg:      tenant.Org,
				TenantID:       tenant.ID,
				Status:         NetworkSecurityGroupStatusPending,
				CreatedBy:      user.ID,
				Labels:         labels,
				StatefulEgress: true,
				Rules:          rules,
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "success - with nullable fields not set",
			sg: NetworkSecurityGroup{
				Version: "anything", ID: uuid.NewString(), Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedBy: user.ID,
			},
			expectError: false,
		},
		{
			desc: "failure - rule list with nil rule",
			sg: NetworkSecurityGroup{
				ID: uuid.NewString(), Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedBy: user.ID,
				Rules: badRules,
			},
			expectError: true,
		},

		{
			desc: "error - when foreign key fails on non-null tenant ID",
			sg: NetworkSecurityGroup{
				ID: uuid.NewString(), Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantID: uuid.New(), Status: NetworkSecurityGroupStatusPending, CreatedBy: user.ID,
			},
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := sgsd.Create(
				ctx, nil,
				NetworkSecurityGroupCreateInput{
					NetworkSecurityGroupID: cutil.GetPtr(tc.sg.ID),
					Name:                   tc.sg.Name,
					Description:            tc.sg.Description,
					SiteID:                 tc.sg.SiteID,
					TenantOrg:              tc.sg.TenantOrg,
					TenantID:               tc.sg.TenantID,
					Status:                 tc.sg.Status,
					CreatedByID:            tc.sg.CreatedBy,
					Rules:                  tc.sg.Rules,
					Labels:                 tc.sg.Labels,
					Version:                &tc.sg.Version,
				},
			)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				if !assert.NotNil(t, got) {
					t.Errorf("Error: %s", err)
				}
			}

			if err != nil {
				return
			}

			assert.Equal(t, tc.sg.ID, got.ID)
			assert.Equal(t, tc.sg.Version, got.Version)
			assert.Equal(t, tc.sg.Name, got.Name)
			assert.True(t, tc.sg.Description == nil || *tc.sg.Description == *got.Description)
			assert.Equal(t, tc.sg.SiteID, got.SiteID)
			assert.Equal(t, tc.sg.TenantOrg, got.TenantOrg)
			assert.Equal(t, tc.sg.TenantID, got.TenantID)
			assert.Equal(t, tc.sg.CreatedBy, got.CreatedBy)
			assert.Equal(t, tc.sg.CreatedBy, got.UpdatedBy) // It's not a typo
			assert.Equal(t, tc.sg.Labels, got.Labels)
			assert.Equal(t, tc.sg.Rules, got.Rules)

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNetworkSecurityGroupSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testNetworkSecurityGroupSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	sgsd := NewNetworkSecurityGroupDAO(dbSession)
	sg1, err := sgsd.Create(ctx, nil, NetworkSecurityGroupCreateInput{Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedByID: user.ID})
	assert.Nil(t, err)
	assert.NotNil(t, sg1)

	sg2, err := sgsd.Create(ctx, nil, NetworkSecurityGroupCreateInput{Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedByID: user.ID})
	assert.Nil(t, err)
	assert.NotNil(t, sg1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		sgID               string
		includeRelations   []string
		expectNotNilSite   bool
		expectNotNilTenant bool
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc:               "success without relations",
			sgID:               sg1.ID,
			includeRelations:   []string{},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:               "success with relations",
			sgID:               sg1.ID,
			includeRelations:   []string{"Site", "Tenant"},
			expectError:        false,
			expectNotNilSite:   true,
			expectNotNilTenant: true,
		},
		{
			desc:             "error when not found",
			sgID:             uuid.NewString(),
			includeRelations: []string{},
			expectError:      true,
		},
		{
			desc:               "success with relations when nullable",
			sgID:               sg2.ID,
			includeRelations:   []string{"Site", "Tenant"},
			expectError:        false,
			expectNotNilSite:   true,
			expectNotNilTenant: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := sgsd.GetByID(ctx, nil, tc.sgID, tc.includeRelations)
			assert.Equal(t, tc.expectError, err != nil)
			if !tc.expectError {
				assert.NotNil(t, got)
				if tc.expectNotNilSite {
					assert.NotNil(t, got.Site)
				}
				if tc.expectNotNilTenant {
					assert.NotNil(t, got.Tenant)
				}
				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
				}
			}
		})
	}
}

func TestNetworkSecurityGroupSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testNetworkSecurityGroupSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	sgsd := NewNetworkSecurityGroupDAO(dbSession)

	nsgIDs := []string{}

	for i := 1; i < 26; i++ {
		sg1, err := sgsd.Create(ctx, nil, NetworkSecurityGroupCreateInput{Name: fmt.Sprintf("test%d", i), Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedByID: user.ID})
		assert.Nil(t, err)
		assert.NotNil(t, sg1)

		nsgIDs = append(nsgIDs, sg1.ID)
	}

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc             string
		includeRelations []string

		paramOffset    *int
		paramLimit     *int
		paramID        []string
		paramSiteID    []uuid.UUID
		paramTenantID  []uuid.UUID
		paramTenantOrg []string
		paramName      *string
		paramStatus    []string
		searchQuery    *string
		paramOrderBy   *paginator.OrderBy

		expectCnt             int
		expectTotal           int
		expectFirstObjectName string
		expectNotNilSite      bool
		expectNotNilTenant    bool
		expectError           bool
		verifyChildSpanner    bool
	}{
		{
			desc:                  "getall with filters on nsgs",
			paramID:               []string{nsgIDs[1], nsgIDs[2]},
			includeRelations:      []string{},
			expectFirstObjectName: "test2",
			expectError:           false,
			expectTotal:           2,
			expectCnt:             2,
			verifyChildSpanner:    true,
		},
		{
			desc:                  "getall with filters but no relations returns objects",
			paramSiteID:           []uuid.UUID{site.ID},
			paramTenantOrg:        []string{tenant.Org},
			paramTenantID:         []uuid.UUID{tenant.ID},
			paramStatus:           []string{NetworkSecurityGroupStatusPending},
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
			verifyChildSpanner:    true,
		},
		{
			desc:           "getall with site, tenant, and name returns objects",
			paramSiteID:    []uuid.UUID{site.ID},
			paramTenantOrg: []string{tenant.Org},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:           "getall name filter",
			paramSiteID:    []uuid.UUID{site.ID},
			paramTenantOrg: []string{tenant.Org},
			paramName:      cutil.GetPtr("test1"),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           1,
			expectCnt:             1,
		},
		{
			desc:             "getall with filters and relations returns objects",
			includeRelations: []string{"Site", "Tenant"},
			paramSiteID:      []uuid.UUID{site.ID},
			paramTenantOrg:   []string{tenant.Org},
			paramStatus:      []string{NetworkSecurityGroupStatusPending},
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
			expectNotNilSite:      true,
			expectNotNilTenant:    true,
		},
		{
			desc:             "getall with offset, limit returns objects",
			includeRelations: []string{},

			paramOffset: cutil.GetPtr(10),
			paramLimit:  cutil.GetPtr(10),
			paramOrderBy: &paginator.OrderBy{
				Field: "updated",
				Order: paginator.OrderAscending,
			},
			paramSiteID:           []uuid.UUID{site.ID},
			paramTenantOrg:        []string{tenant.Org},
			paramStatus:           []string{NetworkSecurityGroupStatusPending},
			expectFirstObjectName: "test11",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             10,
		},
		{
			desc:                  "getall with name search query returns objects",
			paramSiteID:           nil,
			paramTenantOrg:        nil,
			paramStatus:           nil,
			searchQuery:           cutil.GetPtr("test"),
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with description search query returns objects",
			paramSiteID:           nil,
			paramTenantOrg:        nil,
			paramStatus:           nil,
			searchQuery:           cutil.GetPtr("test"),
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with status search query returns objects",
			paramSiteID:           nil,
			paramTenantOrg:        nil,
			paramStatus:           nil,
			searchQuery:           cutil.GetPtr(NetworkSecurityGroupStatusPending),
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
		{
			desc:                  "getall with empty search query returns objects",
			paramSiteID:           nil,
			paramTenantOrg:        nil,
			paramStatus:           nil,
			searchQuery:           cutil.GetPtr("test"),
			includeRelations:      []string{},
			expectFirstObjectName: "test1",
			expectError:           false,
			expectTotal:           25,
			expectCnt:             20,
		},
	}

	for _, tc := range tests {

		t.Run(tc.desc, func(t *testing.T) {
			objs, tot, err := sgsd.GetAll(ctx, nil, NetworkSecurityGroupFilterInput{NetworkSecurityGroupIDs: tc.paramID,
				SiteIDs: tc.paramSiteID, TenantOrgs: tc.paramTenantOrg, TenantIDs: tc.paramTenantID, Name: tc.paramName, Statuses: tc.paramStatus, SearchQuery: tc.searchQuery,
			}, paginator.PageInput{Offset: tc.paramOffset, Limit: tc.paramLimit, OrderBy: tc.paramOrderBy}, tc.includeRelations)

			if err != nil && !tc.expectError {
				fmt.Printf("\n%s\n", err)
			}

			assert.Equal(t, tc.expectError, err != nil)

			assert.Equal(t, tc.expectCnt, len(objs))
			assert.Equal(t, tc.expectTotal, tot)
			if len(objs) > 0 {
				assert.Equal(t, tc.expectFirstObjectName, objs[0].Name)
				if tc.expectNotNilSite {
					assert.NotNil(t, objs[0].Site)
				}
				if tc.expectNotNilTenant {
					assert.NotNil(t, objs[0].Tenant)
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNetworkSecurityGroupSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testNetworkSecurityGroupSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")
	user2 := testInstanceBuildUser(t, dbSession, "testUser2")

	labels := map[string]string{}
	labels["key"] = "value"

	rules := []*NetworkSecurityGroupRule{
		&NetworkSecurityGroupRule{&corev1.NetworkSecurityGroupRuleAttributes{}},
	}

	badRules := []*NetworkSecurityGroupRule{
		&NetworkSecurityGroupRule{&corev1.NetworkSecurityGroupRuleAttributes{}},
		nil,
	}

	sgsd := NewNetworkSecurityGroupDAO(dbSession)
	sg1, err := sgsd.Create(ctx, nil, NetworkSecurityGroupCreateInput{Version: cutil.GetPtr("12345"), Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedByID: user.ID})
	assert.Nil(t, err)
	assert.NotNil(t, sg1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	ptrTrue := true
	tests := []struct {
		desc string
		id   string

		paramVersion        *string
		paramName           *string
		paramDescription    *string
		paramStatus         *string
		paramStatefulEgress *bool
		paramRules          []*NetworkSecurityGroupRule
		paramLabels         map[string]string
		paramUpdatedByID    uuid.UUID

		expectedUpdatedByID    *uuid.UUID
		expectedTenantID       *uuid.UUID
		expectedTenantOrg      *string
		expectedSiteID         *uuid.UUID
		expectedVersion        *string
		expectedName           *string
		expectedDescription    *string
		expectedStatus         *string
		expectedStatefulEgress *bool
		expectedRules          []*NetworkSecurityGroupRule
		expectedLabels         Labels

		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "can update all fields",
			id:   sg1.ID,

			paramName:           cutil.GetPtr("updatedName"),
			paramDescription:    cutil.GetPtr("updatedDesc"),
			paramStatus:         cutil.GetPtr(NetworkSecurityGroupStatusReady),
			paramVersion:        cutil.GetPtr("555555"),
			paramStatefulEgress: &ptrTrue,
			paramRules:          rules,
			paramLabels:         map[string]string{"key": "value"},
			paramUpdatedByID:    user2.ID,

			expectedName:           cutil.GetPtr("updatedName"),
			expectedDescription:    cutil.GetPtr("updatedDesc"),
			expectedStatus:         cutil.GetPtr(NetworkSecurityGroupStatusReady),
			expectedVersion:        cutil.GetPtr("555555"),
			expectedLabels:         Labels{"key": "value"},
			expectedUpdatedByID:    &user2.ID,
			expectedTenantID:       &sg1.TenantID,
			expectedTenantOrg:      &sg1.TenantOrg,
			expectedSiteID:         &sg1.SiteID,
			expectedStatefulEgress: &ptrTrue,
			expectedRules:          rules,

			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:               "reject bad rule list",
			id:                 sg1.ID,
			paramRules:         badRules,
			expectError:        true,
			verifyChildSpanner: true,
		},
		{
			desc:        "error when ID not found",
			id:          uuid.NewString(),
			expectError: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := sgsd.Update(ctx, nil, NetworkSecurityGroupUpdateInput{
				Version:                tc.paramVersion,
				NetworkSecurityGroupID: tc.id,
				Name:                   tc.paramName,
				Description:            tc.paramDescription,
				Status:                 tc.paramStatus,
				Labels:                 labels,
				StatefulEgress:         tc.paramStatefulEgress,
				Rules:                  tc.paramRules,
				UpdatedByID:            tc.paramUpdatedByID,
			})

			assert.Equal(t, tc.expectError, err != nil)
			if err == nil {
				assert.NotEqual(t, got.Updated.String(), sg1.Updated.String())

				assert.True(t, tc.expectedName == nil || *tc.expectedName == got.Name)
				assert.True(t, tc.expectedDescription == nil || *tc.expectedDescription == *got.Description)
				assert.True(t, tc.expectedStatus == nil || *tc.expectedStatus == got.Status)
				assert.True(t, tc.expectedTenantID == nil || *tc.expectedTenantID == got.TenantID)
				assert.True(t, tc.expectedTenantOrg == nil || *tc.expectedTenantOrg == got.TenantOrg)
				assert.True(t, tc.expectedSiteID == nil || *tc.expectedSiteID == got.SiteID)
				assert.True(t, tc.expectedStatefulEgress == nil || *tc.expectedStatefulEgress == got.StatefulEgress)

				assert.Equal(t, tc.paramUpdatedByID, got.UpdatedBy)

				assert.Equal(t, tc.expectedLabels, got.Labels)

				if tc.expectedRules != nil {
					assert.Equal(t, len(tc.expectedRules), len(got.Rules))

					for i, _ := range tc.expectedRules {
						assert.True(t, proto.Equal(got.Rules[i], tc.expectedRules[i]))
					}
				}
			}
			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
			}
		})
	}
}

func TestNetworkSecurityGroupSQLDAO_DeleteByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInstanceInitDB(t)
	defer dbSession.Close()
	testNetworkSecurityGroupSetupSchema(t, dbSession)
	ip := testInstanceBuildInfrastructureProvider(t, dbSession, "testIP")
	site := testInstanceBuildSite(t, dbSession, ip, "testSite")
	tenant := testInstanceBuildTenant(t, dbSession, "testTenant")
	user := testInstanceBuildUser(t, dbSession, "testUser")

	sgsd := NewNetworkSecurityGroupDAO(dbSession)
	sg1, err := sgsd.Create(ctx, nil, NetworkSecurityGroupCreateInput{Name: "test", Description: cutil.GetPtr("test"), SiteID: site.ID, TenantOrg: tenant.Org, TenantID: tenant.ID, Status: NetworkSecurityGroupStatusPending, CreatedByID: user.ID})
	assert.Nil(t, err)
	assert.NotNil(t, sg1)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		id                 string
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object",
			id:                 sg1.ID,
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc:          "delete non-existing object",
			id:            uuid.NewString(),
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := sgsd.Delete(ctx, nil, NetworkSecurityGroupDeleteInput{NetworkSecurityGroupID: tc.id, UpdatedByID: user.ID})
			assert.Equal(t, tc.expectedError, err != nil)
			if !tc.expectedError {
				tmp, err := sgsd.GetByID(ctx, nil, tc.id, nil)
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
