// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPITenantAccountCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APITenantAccountCreateRequest
		expectErr bool
		errStr    string
	}{
		{
			desc:      "ok when infrastructureProviderID is omitted (inferred from org by handler)",
			obj:       APITenantAccountCreateRequest{TenantID: cutil.GetPtr(uuid.New().String())},
			expectErr: false,
		},
		{
			desc:      "errors when infrastructureProviderID is invalid",
			obj:       APITenantAccountCreateRequest{InfrastructureProviderID: "non-uuid", TenantID: cutil.GetPtr(uuid.New().String())},
			expectErr: true,
			errStr:    "infrastructureProviderId: " + validationErrorInvalidUUID + ".",
		},
		{
			desc:      "ok when tenantID and tenantOrg are empty",
			obj:       APITenantAccountCreateRequest{InfrastructureProviderID: uuid.New().String()},
			expectErr: true,
			errStr:    "tenantId: " + validationErrorTenantIDOrOrgRequired + "; tenantOrg: " + validationErrorTenantIDOrOrgRequired + ".",
		},
		{
			desc:      "error when TenantID is invalid",
			obj:       APITenantAccountCreateRequest{InfrastructureProviderID: uuid.New().String(), TenantID: cutil.GetPtr("non-uuid")},
			expectErr: true,
			errStr:    "tenantId: " + validationErrorInvalidUUID + ".",
		},
		{
			desc:      "error when TenantOrg is invalid",
			obj:       APITenantAccountCreateRequest{InfrastructureProviderID: uuid.New().String(), TenantOrg: cutil.GetPtr("n")},
			expectErr: true,
			errStr:    "tenantOrg: " + validationErrorStringLength + ".",
		},
		{
			desc:      "ok with valid values - with tenantID",
			obj:       APITenantAccountCreateRequest{InfrastructureProviderID: uuid.New().String(), TenantID: cutil.GetPtr(uuid.New().String())},
			expectErr: false,
		},
		{
			desc:      "ok with valid values - with tenantOrg",
			obj:       APITenantAccountCreateRequest{InfrastructureProviderID: uuid.New().String(), TenantOrg: cutil.GetPtr("SomeOrgName")},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				assert.Equal(t, tc.errStr, err.Error())
			}
		})
	}
}

func TestAPITenantAccountUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APITenantAccountUpdateRequest
		expectErr bool
		errStr    string
	}{
		{
			desc:      "errors when tenantContactID is invalid",
			obj:       APITenantAccountUpdateRequest{TenantContactID: cutil.GetPtr("non-uuid")},
			expectErr: true,
			errStr:    "tenantContactId: " + validationErrorInvalidUUID + ".",
		},
		{
			desc:      "ok when tenantContactID is not provided",
			obj:       APITenantAccountUpdateRequest{},
			expectErr: false,
		},
		{
			desc:      "ok when tenantContactID is valid",
			obj:       APITenantAccountUpdateRequest{TenantContactID: cutil.GetPtr(uuid.New().String())},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				assert.Equal(t, tc.errStr, err.Error())
			}
		})
	}
}

// TestAPITenantAccountUpdateRequest_SiteCapabilitiesPresence verifies binding
// distinguishes an omitted or JSON-null siteCapabilities payload (absent) from a
// supplied-but-empty array (present), and that a supplied empty array fails
// validation so the handler can reject it with HTTP 400.
func TestAPITenantAccountUpdateRequest_SiteCapabilitiesPresence(t *testing.T) {
	tests := []struct {
		desc          string
		body          string
		wantPresent   bool
		wantCapsError bool
	}{
		{
			desc:        "omitted payload is absent",
			body:        `{}`,
			wantPresent: false,
		},
		{
			desc:        "explicit null payload is absent",
			body:        `{"siteCapabilities": null}`,
			wantPresent: false,
		},
		{
			desc:          "empty array payload is present but invalid",
			body:          `{"siteCapabilities": []}`,
			wantPresent:   true,
			wantCapsError: true,
		},
		{
			desc:          "populated payload is present and valid",
			body:          `{"siteCapabilities": [{"siteIds": [], "targetedInstanceCreation": true}]}`,
			wantPresent:   true,
			wantCapsError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			var req APITenantAccountUpdateRequest
			err := json.Unmarshal([]byte(tc.body), &req)
			assert.NoError(t, err)
			assert.Equal(t, tc.wantPresent, req.HasSiteCapabilities())
			if req.HasSiteCapabilities() {
				capsErr := req.SiteCapabilities.Validate()
				assert.Equal(t, tc.wantCapsError, capsErr != nil)
			}
		})
	}
}

func TestAPITenantAccountSiteCapabilitiesUpdateRequest_Validate(t *testing.T) {
	siteID := uuid.New().String()
	otherSiteID := uuid.New().String()

	tests := []struct {
		desc      string
		caps      APITenantAccountSiteCapabilitiesUpdateRequest
		expectErr bool
	}{
		{
			desc:      "rejects empty capability list",
			caps:      APITenantAccountSiteCapabilitiesUpdateRequest{},
			expectErr: true,
		},
		{
			desc: "rejects omitted targetedInstanceCreation",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{},
			},
			expectErr: true,
		},
		{
			desc: "accepts omitted siteIds for account capability",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{TargetedInstanceCreation: cutil.GetPtr(true)},
			},
		},
		{
			desc: "accepts empty siteIds for account capability",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{
					SiteIDs:                  []string{},
					TargetedInstanceCreation: cutil.GetPtr(false),
				},
			},
		},
		{
			desc: "accepts account capability and site overrides",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{TargetedInstanceCreation: cutil.GetPtr(true)},
				{
					SiteIDs:                  []string{siteID, otherSiteID},
					TargetedInstanceCreation: cutil.GetPtr(false),
				},
			},
		},
		{
			desc: "rejects request without account capability",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{
					SiteIDs:                  []string{siteID},
					TargetedInstanceCreation: cutil.GetPtr(true),
				},
			},
			expectErr: true,
		},
		{
			desc: "rejects multiple account capabilities",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{TargetedInstanceCreation: cutil.GetPtr(true)},
				{
					SiteIDs:                  []string{},
					TargetedInstanceCreation: cutil.GetPtr(false),
				},
			},
			expectErr: true,
		},
		{
			desc: "rejects invalid site ID",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{TargetedInstanceCreation: cutil.GetPtr(true)},
				{
					SiteIDs:                  []string{"not-a-uuid"},
					TargetedInstanceCreation: cutil.GetPtr(false),
				},
			},
			expectErr: true,
		},
		{
			desc: "rejects duplicate site IDs across entries",
			caps: APITenantAccountSiteCapabilitiesUpdateRequest{
				{TargetedInstanceCreation: cutil.GetPtr(true)},
				{
					SiteIDs:                  []string{siteID},
					TargetedInstanceCreation: cutil.GetPtr(false),
				},
				{
					SiteIDs:                  []string{siteID},
					TargetedInstanceCreation: cutil.GetPtr(true),
				},
			},
			expectErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.caps.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
		})
	}
}

func TestAPITenantAccountNew(t *testing.T) {
	dbObj := &cdbm.TenantAccount{
		ID:                        uuid.New(),
		TenantID:                  cutil.GetPtr(uuid.New()),
		TenantOrg:                 "testOrg",
		InfrastructureProviderID:  uuid.New(),
		InfrastructureProviderOrg: "testIPOrg",
		TenantContactID:           cutil.GetPtr(uuid.New()),
		Status:                    "Invited",
		Created:                   cdb.GetCurTime(),
		Updated:                   cdb.GetCurTime(),
	}
	dbUsr := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr("sf"),
		FirstName:   cutil.GetPtr("t"),
		LastName:    cutil.GetPtr("s"),
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}
	dbObj2 := &cdbm.TenantAccount{
		ID:                        uuid.New(),
		TenantID:                  cutil.GetPtr(uuid.New()),
		TenantOrg:                 "testOrg",
		InfrastructureProviderID:  uuid.New(),
		InfrastructureProviderOrg: "testIPOrg",
		TenantContact:             dbUsr,
		TenantContactID:           cutil.GetPtr(uuid.New()),
		Status:                    "Invited",
		Created:                   cdb.GetCurTime(),
		Updated:                   cdb.GetCurTime(),
	}
	apiUsr := NewAPIUserFromDBUser(*dbUsr)

	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbObj.ID.String(),
			Status:   cdbm.TenantAccountStatusInvited,
			Message:  cutil.GetPtr("received tenant account creation request, pending accept"),
			Created:  time.Now(),
			Updated:  time.Now(),
		},
		{
			ID:       uuid.New(),
			EntityID: dbObj.ID.String(),
			Status:   cdbm.TenantAccountStatusReady,
			Message:  cutil.GetPtr("received tenant account update request, ready"),
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	msh := []APIStatusDetail{}
	for _, s := range dbsds {
		msh = append(msh, NewAPIStatusDetail(s))
	}

	providerID := uuid.MustParse("11111111-1111-4111-8111-111111111111")
	otherProviderID := uuid.MustParse("22222222-2222-4222-8222-222222222222")
	tenantID := uuid.MustParse("33333333-3333-4333-8333-333333333333")
	siteEnabledA := uuid.MustParse("a0000000-0000-4000-8000-000000000001")
	siteEnabledB := uuid.MustParse("b0000000-0000-4000-8000-000000000002")
	siteMatchesAccountDefault := uuid.MustParse("c0000000-0000-4000-8000-000000000003")
	siteOtherProvider := uuid.MustParse("d0000000-0000-4000-8000-000000000004")
	siteDisabledA := uuid.MustParse("a0000000-0000-4000-8000-000000000010")
	siteDisabledB := uuid.MustParse("b0000000-0000-4000-8000-000000000011")

	dbObjWithSiteCapabilities := &cdbm.TenantAccount{
		ID:                        uuid.New(),
		TenantID:                  &tenantID,
		TenantOrg:                 "testOrg",
		InfrastructureProviderID:  providerID,
		InfrastructureProviderOrg: "testIPOrg",
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    cdbm.TenantAccountConfig{TargetedInstanceCreation: false},
		Created:                   cdb.GetCurTime(),
		Updated:                   cdb.GetCurTime(),
	}
	dbObjWithDisabledOverrides := &cdbm.TenantAccount{
		ID:                        uuid.New(),
		TenantID:                  &tenantID,
		TenantOrg:                 "testOrg",
		InfrastructureProviderID:  providerID,
		InfrastructureProviderOrg: "testIPOrg",
		Status:                    cdbm.TenantAccountStatusReady,
		Config:                    cdbm.TenantAccountConfig{TargetedInstanceCreation: true},
		Created:                   cdb.GetCurTime(),
		Updated:                   cdb.GetCurTime(),
	}

	accountDefaultFalseTenantSites := []cdbm.TenantSite{
		{
			SiteID: siteEnabledB,
			Site:   &cdbm.Site{InfrastructureProviderID: providerID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
		},
		{
			SiteID: siteEnabledA,
			Site:   &cdbm.Site{InfrastructureProviderID: providerID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
		},
		{
			SiteID: siteMatchesAccountDefault,
			Site:   &cdbm.Site{InfrastructureProviderID: providerID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)},
		},
		{
			SiteID: siteOtherProvider,
			Site:   &cdbm.Site{InfrastructureProviderID: otherProviderID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(true)},
		},
		{
			SiteID: uuid.MustParse("e0000000-0000-4000-8000-000000000005"),
			Site:   &cdbm.Site{InfrastructureProviderID: providerID},
			Config: cdbm.TenantSiteConfig{},
		},
	}
	accountDefaultTrueTenantSites := []cdbm.TenantSite{
		{
			SiteID: siteDisabledB,
			Site:   &cdbm.Site{InfrastructureProviderID: providerID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)},
		},
		{
			SiteID: siteDisabledA,
			Site:   &cdbm.Site{InfrastructureProviderID: providerID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)},
		},
		{
			SiteID: siteOtherProvider,
			Site:   &cdbm.Site{InfrastructureProviderID: otherProviderID},
			Config: cdbm.TenantSiteConfig{TargetedInstanceCreation: cutil.GetPtr(false)},
		},
	}

	tests := []struct {
		desc        string
		dbObj       *cdbm.TenantAccount
		sdObj       []cdbm.StatusDetail
		tenantSites []cdbm.TenantSite
		apiObj      *APITenantAccount
	}{
		{
			desc:  "success with TenantContact nil",
			dbObj: dbObj,
			sdObj: dbsds,
			apiObj: &APITenantAccount{
				ID:                        dbObj.ID.String(),
				InfrastructureProviderID:  dbObj.InfrastructureProviderID.String(),
				InfrastructureProviderOrg: dbObj.InfrastructureProviderOrg,
				TenantID:                  cutil.GetPtr(dbObj.TenantID.String()),
				TenantOrg:                 dbObj.TenantOrg,
				TenantContact:             nil,
				AllocationCount:           2,
				Status:                    dbObj.Status,
				StatusHistory:             msh,
				Created:                   dbObj.Created,
				Updated:                   dbObj.Updated,
				SiteCapabilities: []APITenantAccountSiteCapability{
					{
						SiteIDs:                  []string{},
						TargetedInstanceCreation: false,
					},
				},
			},
		},
		{
			desc:  "success with TenantContact not nil",
			dbObj: dbObj2,
			sdObj: dbsds,
			apiObj: &APITenantAccount{
				ID:                        dbObj2.ID.String(),
				InfrastructureProviderID:  dbObj2.InfrastructureProviderID.String(),
				InfrastructureProviderOrg: dbObj2.InfrastructureProviderOrg,
				TenantID:                  cutil.GetPtr(dbObj2.TenantID.String()),
				TenantOrg:                 dbObj.TenantOrg,
				TenantContact:             apiUsr,
				AllocationCount:           2,
				Status:                    dbObj2.Status,
				StatusHistory:             msh,
				Created:                   dbObj2.Created,
				Updated:                   dbObj2.Updated,
				SiteCapabilities: []APITenantAccountSiteCapability{
					{
						SiteIDs:                  []string{},
						TargetedInstanceCreation: false,
					},
				},
			},
		},
		{
			desc:  "status history is an empty slice (not nil) when no status details exist",
			dbObj: dbObj,
			sdObj: []cdbm.StatusDetail{},
			apiObj: &APITenantAccount{
				ID:                         dbObj.ID.String(),
				AccountNumberDeprecated:    cutil.GetPtr(dbObj.AccountNumber),
				InfrastructureProviderID:   dbObj.InfrastructureProviderID.String(),
				InfrastructureProviderOrg:  dbObj.InfrastructureProviderOrg,
				SubscriptionIDDeprecated:   dbObj.SubscriptionID,
				SubscriptionTierDeprecated: dbObj.SubscriptionTier,
				TenantID:                   cutil.GetPtr(dbObj.TenantID.String()),
				TenantOrg:                  dbObj.TenantOrg,
				TenantContact:              nil,
				AllocationCount:            2,
				Status:                     dbObj.Status,
				StatusHistory:              []APIStatusDetail{},
				Created:                    dbObj.Created,
				Updated:                    dbObj.Updated,
				SiteCapabilities: []APITenantAccountSiteCapability{
					{
						SiteIDs:                  []string{},
						TargetedInstanceCreation: false,
					},
				},
			},
		},
		{
			desc:        "siteCapabilities derive false account default, enabled overrides, and provider filtering",
			dbObj:       dbObjWithSiteCapabilities,
			sdObj:       dbsds,
			tenantSites: accountDefaultFalseTenantSites,
			apiObj: &APITenantAccount{
				ID:                        dbObjWithSiteCapabilities.ID.String(),
				InfrastructureProviderID:  providerID.String(),
				InfrastructureProviderOrg: dbObjWithSiteCapabilities.InfrastructureProviderOrg,
				TenantID:                  cutil.GetPtr(tenantID.String()),
				TenantOrg:                 dbObjWithSiteCapabilities.TenantOrg,
				TenantContact:             nil,
				AllocationCount:           2,
				Status:                    dbObjWithSiteCapabilities.Status,
				StatusHistory:             msh,
				Created:                   dbObjWithSiteCapabilities.Created,
				Updated:                   dbObjWithSiteCapabilities.Updated,
				SiteCapabilities: []APITenantAccountSiteCapability{
					{
						SiteIDs:                  []string{},
						TargetedInstanceCreation: false,
					},
					{
						SiteIDs:                  []string{siteEnabledA.String(), siteEnabledB.String()},
						TargetedInstanceCreation: true,
					},
				},
			},
		},
		{
			desc:        "siteCapabilities derive true account default, disabled overrides sorted before other provider sites are dropped",
			dbObj:       dbObjWithDisabledOverrides,
			sdObj:       dbsds,
			tenantSites: accountDefaultTrueTenantSites,
			apiObj: &APITenantAccount{
				ID:                        dbObjWithDisabledOverrides.ID.String(),
				InfrastructureProviderID:  providerID.String(),
				InfrastructureProviderOrg: dbObjWithDisabledOverrides.InfrastructureProviderOrg,
				TenantID:                  cutil.GetPtr(tenantID.String()),
				TenantOrg:                 dbObjWithDisabledOverrides.TenantOrg,
				TenantContact:             nil,
				AllocationCount:           2,
				Status:                    dbObjWithDisabledOverrides.Status,
				StatusHistory:             msh,
				Created:                   dbObjWithDisabledOverrides.Created,
				Updated:                   dbObjWithDisabledOverrides.Updated,
				SiteCapabilities: []APITenantAccountSiteCapability{
					{
						SiteIDs:                  []string{},
						TargetedInstanceCreation: true,
					},
					{
						SiteIDs:                  []string{siteDisabledA.String(), siteDisabledB.String()},
						TargetedInstanceCreation: false,
					},
				},
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPITenantAccount(tc.dbObj, tc.sdObj, 2, tc.tenantSites)
			assert.Equal(t, tc.apiObj.ID, got.ID)
			assert.Equal(t, tc.apiObj.InfrastructureProviderID, got.InfrastructureProviderID)
			assert.Equal(t, tc.apiObj.InfrastructureProviderOrg, got.InfrastructureProviderOrg)
			assert.Equal(t, tc.apiObj.TenantID, got.TenantID)
			assert.Equal(t, tc.apiObj.TenantOrg, got.TenantOrg)
			assert.Equal(t, tc.apiObj.TenantContact, got.TenantContact)
			assert.Equal(t, tc.apiObj.AllocationCount, got.AllocationCount)
			assert.Equal(t, tc.apiObj.Status, got.Status)
			assert.Equal(t, tc.apiObj.StatusHistory, got.StatusHistory)
			assert.NotNil(t, got.StatusHistory)
			assert.Equal(t, tc.apiObj.SiteCapabilities, got.SiteCapabilities)
		})
	}
}
