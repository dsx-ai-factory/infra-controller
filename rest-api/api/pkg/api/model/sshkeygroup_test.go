// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPISSHKeyGroupCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APISSHKeyGroupCreateRequest
		expectErr bool
	}{
		{
			desc:      "ok when only required fields are provided",
			obj:       APISSHKeyGroupCreateRequest{Name: "test"},
			expectErr: false,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APISSHKeyGroupCreateRequest{Name: "test", Description: cutil.GetPtr("test")},
			expectErr: false,
		},
		{
			desc:      "error when required fields are not provided",
			obj:       APISSHKeyGroupCreateRequest{Description: cutil.GetPtr("test")},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPISSHKeyGroupUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APISSHKeyGroupUpdateRequest
		expectErr bool
	}{
		{
			desc:      "ok when only some fields are provided",
			obj:       APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("updatedname")},
			expectErr: true,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APISSHKeyGroupUpdateRequest{Name: cutil.GetPtr("updatedname"), Description: cutil.GetPtr("updated"), Version: cutil.GetPtr("1234")},
			expectErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPISSHKeyGroupNew(t *testing.T) {
	dbskg := &cdbm.SSHKeyGroup{
		ID:          uuid.New(),
		Name:        "test",
		Description: cutil.GetPtr("test"),
		Org:         "test",
		Version:     cutil.GetPtr("12334"),
		TenantID:    uuid.New(),
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}
	dbsgas := []cdbm.SSHKeyGroupSiteAssociation{
		{ID: uuid.New(), SSHKeyGroupID: dbskg.ID, SiteID: uuid.New(), Version: cutil.GetPtr("1233"), Status: cdbm.SSHKeyGroupSiteAssociationStatusSyncing},
	}
	sttsmap := map[uuid.UUID]*cdbm.TenantSite{
		dbsgas[0].SiteID: {
			ID:                  uuid.New(),
			TenantID:            dbskg.TenantID,
			TenantOrg:           dbskg.Org,
			SiteID:              dbsgas[0].SiteID,
			EnableSerialConsole: true,
			Config:              cdbm.TenantSiteConfig{},
			Created:             cdb.GetCurTime(),
			Updated:             cdb.GetCurTime(),
		},
	}

	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbskg.ID.String(),
			Status:   cdbm.SSHKeyGroupStatusSyncing,
			Created:  cdb.GetCurTime(),
			Updated:  cdb.GetCurTime(),
		},
	}

	dbsk := cdbm.SSHKey{
		ID:          uuid.New(),
		Name:        "test",
		Org:         "test",
		PublicKey:   "test",
		Fingerprint: cutil.GetPtr("test"),
		TenantID:    uuid.New(),
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}

	dbksas := []cdbm.SSHKeyAssociation{
		{
			ID:            uuid.New(),
			SSHKeyID:      dbsk.ID,
			SSHKeyGroupID: dbskg.ID,
			Created:       cdb.GetCurTime(),
			Updated:       cdb.GetCurTime(),
		},
	}

	tests := []struct {
		desc    string
		skg     *cdbm.SSHKeyGroup
		skgsas  []cdbm.SSHKeyGroupSiteAssociation
		sttsmap map[uuid.UUID]*cdbm.TenantSite
		dbksas  []cdbm.SSHKeyAssociation
		dbsds   []cdbm.StatusDetail
	}{
		{
			desc:    "test creating API SSHKeyGroup",
			skg:     dbskg,
			skgsas:  dbsgas,
			sttsmap: sttsmap,
			dbksas:  dbksas,
			dbsds:   dbsds,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPISSHKeyGroup(tc.skg, tc.skgsas, tc.sttsmap, tc.dbksas, tc.dbsds)
			assert.Equal(t, tc.skg.ID.String(), got.ID)
		})
	}
}
