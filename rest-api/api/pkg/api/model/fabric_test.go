// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

func TestMachine_NewAPIFabric(t *testing.T) {
	fguid := "Ifabric01"
	dbf := &cdbm.Fabric{
		ID:                       fguid,
		InfrastructureProviderID: uuid.New(),
		SiteID:                   uuid.New(),
		Status:                   cdbm.FabricStatusPending,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
	}

	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbf.ID,
			Status:   dbf.Status,
			Created:  cdb.GetCurTime(),
			Updated:  cdb.GetCurTime(),
		},
	}

	dbf.Site = &cdbm.Site{
		ID:                       dbf.SiteID,
		Name:                     "test-site",
		Description:              cutil.GetPtr("Test Description"),
		InfrastructureProviderID: dbf.InfrastructureProviderID,
		Status:                   cdbm.SiteStatusRegistered,
		Created:                  cdb.GetCurTime(),
		Updated:                  cdb.GetCurTime(),
		CreatedBy:                uuid.New(),
	}

	apif := NewAPIFabric(dbf, dbsds)
	assert.NotNil(t, apif)

	assert.Equal(t, apif.ID, dbf.ID)
	assert.Equal(t, apif.InfrastructureProviderID, dbf.InfrastructureProviderID.String())
	assert.Equal(t, apif.SiteID, dbf.SiteID.String())
	assert.NotNil(t, apif.Site)
	assert.Equal(t, apif.Site.Name, dbf.Site.Name)

	for i, v := range dbsds {
		assert.Equal(t, apif.StatusHistory[i].Status, v.Status)
	}
}
