// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestNewAPIOperatingSystemSiteAssociation(t *testing.T) {
	ossa := cdbm.OperatingSystemSiteAssociation{
		ID:                uuid.New(),
		OperatingSystemID: uuid.New(),
		SiteID:            uuid.New(),
		Version:           cutil.GetPtr("1234"),
		Status:            cdbm.OperatingSystemSiteAssociationStatusSyncing,
		Created:           time.Now(),
		Updated:           time.Now(),
	}
	apiossa := NewAPIOperatingSystemSiteAssociation(&ossa, nil)
	assert.Equal(t, apiossa.Version, ossa.Version)
	assert.Equal(t, apiossa.Status, ossa.Status)
}
