// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"strings"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIInfiniBandPartitionCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIInfiniBandPartitionCreateRequest
		expectErr bool
	}{
		{
			desc:      "ok when only required fields are provided",
			obj:       APIInfiniBandPartitionCreateRequest{Name: "test", SiteID: uuid.New().String()},
			expectErr: false,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APIInfiniBandPartitionCreateRequest{Name: "test", Description: cutil.GetPtr("test"), SiteID: uuid.New().String()},
			expectErr: false,
		},
		{
			desc:      "error when required fields are not provided",
			obj:       APIInfiniBandPartitionCreateRequest{Name: "test"},
			expectErr: true,
		},
		{
			desc:      "error when description is too long",
			obj:       APIInfiniBandPartitionCreateRequest{Name: "test", Description: cutil.GetPtr(strings.Repeat("x", 1025)), SiteID: uuid.New().String()},
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

func TestAPIInfiniBandPartitionCreateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"
	ibp := &cdbm.InfiniBandPartition{
		ID:          id,
		Org:         "org-1",
		Name:        "ibp-a",
		Description: &desc,
		Labels:      map[string]string{"env": "prod"},
	}

	t.Run("builds creation request from the entity proto", func(t *testing.T) {
		req := APIInfiniBandPartitionCreateRequest{Name: "ibp-a", SiteID: uuid.NewString()}
		got := req.ToProto(ibp)
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.Config)
		assert.Equal(t, "ibp-a", got.Config.Name)
		assert.Equal(t, "org-1", got.Config.TenantOrganizationId)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "ibp-a", got.Metadata.Name)
		assert.Equal(t, "primary", got.Metadata.Description)
		require.Len(t, got.Metadata.Labels, 1)
	})
}

func TestAPIInfiniBandPartitionUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIInfiniBandPartitionUpdateRequest
		expectErr bool
	}{
		{
			desc:      "ok when only some fields are provided",
			obj:       APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("updatedname")},
			expectErr: false,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("updatedname"), Description: cutil.GetPtr("updated")},
			expectErr: false,
		},
		{
			desc:      "error when description is too long",
			obj:       APIInfiniBandPartitionUpdateRequest{Description: cutil.GetPtr(strings.Repeat("x", 1025))},
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

func TestAPIInfiniBandPartitionUpdateRequest_ToProto(t *testing.T) {
	id := uuid.New()
	desc := "primary"
	ibp := &cdbm.InfiniBandPartition{
		ID:          id,
		Org:         "org-1",
		Name:        "ibp-a",
		Description: &desc,
		Labels:      map[string]string{"env": "prod"},
	}

	t.Run("builds update request from the entity proto", func(t *testing.T) {
		req := APIInfiniBandPartitionUpdateRequest{Name: cutil.GetPtr("ibp-a")}
		got := req.ToProto(ibp)
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, id.String(), got.Id.Value)
		require.NotNil(t, got.Config)
		assert.Equal(t, "ibp-a", got.Config.Name)
		assert.Equal(t, "org-1", got.Config.TenantOrganizationId)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "ibp-a", got.Metadata.Name)
		assert.Equal(t, "primary", got.Metadata.Description)
		require.Len(t, got.Metadata.Labels, 1)
	})
}

func TestAPIInfiniBandPartitionNew(t *testing.T) {
	dbIBP := &cdbm.InfiniBandPartition{
		ID:          uuid.New(),
		Name:        "test-ib-partition",
		Description: cutil.GetPtr("test"),
		SiteID:      uuid.New(),
		TenantID:    uuid.New(),
		Status:      cdbm.InfiniBandInterfaceStatusPending,
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}
	dbsds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbIBP.ID.String(),
			Status:   cdbm.InfiniBandInterfaceStatusPending,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}
	tests := []struct {
		desc  string
		dbObj *cdbm.InfiniBandPartition
		dbSds []cdbm.StatusDetail
	}{
		{
			desc:  "test creating API IB Partition",
			dbObj: dbIBP,
			dbSds: dbsds,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIInfiniBandPartition(tc.dbObj, tc.dbSds)
			assert.Equal(t, tc.dbObj.ID.String(), got.ID)
		})
	}
}

func TestNewAPIInfiniBandPartitionSummary(t *testing.T) {
	dbIBP := &cdbm.InfiniBandPartition{
		ID:          uuid.New(),
		Name:        "test-ib-partition",
		Description: cutil.GetPtr("test"),
		SiteID:      uuid.New(),
		TenantID:    uuid.New(),
		Status:      cdbm.InfiniBandInterfaceStatusPending,
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}
	tests := []struct {
		desc  string
		dbObj *cdbm.InfiniBandPartition
	}{
		{
			desc:  "test creating API IB Partition Summary",
			dbObj: dbIBP,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIInfiniBandPartitionSummary(tc.dbObj)
			assert.Equal(t, tc.dbObj.Name, got.Name)
			assert.Equal(t, tc.dbObj.SiteID.String(), got.SiteID)
		})
	}
}
