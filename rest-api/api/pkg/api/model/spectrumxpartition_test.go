// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"math"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPISpectrumXPartitionCreateRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		request APISpectrumXPartitionCreateRequest
		wantErr bool
	}{
		{
			name: "test validation success, VNI omitted",
			request: APISpectrumXPartitionCreateRequest{
				Name:   "east-west-net",
				SiteID: uuid.NewString(),
			},
		},
		{
			name: "test validation success, explicit zero VNI",
			request: APISpectrumXPartitionCreateRequest{
				Name:   "east-west-net",
				SiteID: uuid.NewString(),
				VNI:    cutil.GetPtr(0),
			},
		},
		{
			name: "test validation success, VNI at the pool upper bound",
			request: APISpectrumXPartitionCreateRequest{
				Name:   "east-west-net",
				SiteID: uuid.NewString(),
				VNI:    cutil.GetPtr(math.MaxInt32),
			},
		},
		{
			name: "test validation failure, missing name",
			request: APISpectrumXPartitionCreateRequest{
				SiteID: uuid.NewString(),
			},
			wantErr: true,
		},
		{
			name: "test validation failure, missing siteId",
			request: APISpectrumXPartitionCreateRequest{
				Name: "east-west-net",
			},
			wantErr: true,
		},
		{
			name: "test validation failure, invalid siteId",
			request: APISpectrumXPartitionCreateRequest{
				Name:   "east-west-net",
				SiteID: "badid",
			},
			wantErr: true,
		},
		{
			name: "test validation failure, negative VNI",
			request: APISpectrumXPartitionCreateRequest{
				Name:   "east-west-net",
				SiteID: uuid.NewString(),
				VNI:    cutil.GetPtr(-1),
			},
			wantErr: true,
		},
		{
			// The Site holds VNIs in a signed 32-bit pool, so anything above that
			// cannot be represented and must be rejected before the wire cast.
			name: "test validation failure, VNI above the pool upper bound",
			request: APISpectrumXPartitionCreateRequest{
				Name:   "east-west-net",
				SiteID: uuid.NewString(),
				VNI:    cutil.GetPtr(math.MaxInt32 + 1),
			},
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.request.Validate()
			if tt.wantErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}

func TestAPISpectrumXPartitionCreateRequest_ToProto(t *testing.T) {
	partitionID := uuid.New()
	persisted := &cdbm.SpectrumXPartition{
		ID:   partitionID,
		Name: "east-west-net",
		Org:  "test-org",
	}

	// The optional wire field is what tells the Site to allocate a VNI itself, so the
	// nil and set cases have to stay distinguishable through the conversion.
	t.Run("omitted VNI leaves the optional wire field unset", func(t *testing.T) {
		request := APISpectrumXPartitionCreateRequest{Name: "east-west-net", SiteID: uuid.NewString()}

		got := request.ToProto(persisted)
		require.NotNil(t, got)
		require.NotNil(t, got.Id)
		assert.Equal(t, partitionID.String(), got.Id.Value)
		assert.Equal(t, "test-org", got.TenantOrganizationId)
		require.NotNil(t, got.Metadata)
		assert.Equal(t, "east-west-net", got.Metadata.Name)
		assert.Nil(t, got.Vni)
	})

	t.Run("requested VNI is carried through", func(t *testing.T) {
		request := APISpectrumXPartitionCreateRequest{Name: "east-west-net", SiteID: uuid.NewString(), VNI: cutil.GetPtr(10200)}

		got := request.ToProto(persisted)
		require.NotNil(t, got.Vni)
		assert.Equal(t, uint32(10200), *got.Vni)
	})

	t.Run("explicit zero VNI is carried through rather than dropped", func(t *testing.T) {
		request := APISpectrumXPartitionCreateRequest{Name: "east-west-net", SiteID: uuid.NewString(), VNI: cutil.GetPtr(0)}

		got := request.ToProto(persisted)
		require.NotNil(t, got.Vni)
		assert.Equal(t, uint32(0), *got.Vni)
	})
}

func TestAPISpectrumXPartition_FromDB(t *testing.T) {
	partitionID := uuid.New()
	siteID := uuid.New()
	tenantID := uuid.New()
	created := time.Now().UTC()

	t.Run("nil DB model is a no-op", func(t *testing.T) {
		apiSXP := &APISpectrumXPartition{ID: "preserved"}
		apiSXP.FromDB(nil, nil)
		assert.Equal(t, "preserved", apiSXP.ID)
	})

	t.Run("populates scalar fields and status history", func(t *testing.T) {
		dbSXP := &cdbm.SpectrumXPartition{
			ID:          partitionID,
			Name:        "east-west-net",
			Description: cutil.GetPtr("east-west"),
			SiteID:      siteID,
			TenantID:    tenantID,
			VNI:         cutil.GetPtr(10200),
			Labels:      map[string]string{"env": "prod"},
			Status:      cdbm.SpectrumXPartitionStatusReady,
			Created:     created,
			Updated:     created,
		}

		apiSXP := &APISpectrumXPartition{}
		apiSXP.FromDB(dbSXP, []cdbm.StatusDetail{{EntityID: partitionID.String(), Status: string(cdbm.SpectrumXPartitionStatusReady)}})

		assert.Equal(t, partitionID.String(), apiSXP.ID)
		assert.Equal(t, "east-west-net", apiSXP.Name)
		assert.Equal(t, siteID.String(), apiSXP.SiteID)
		assert.Equal(t, tenantID.String(), apiSXP.TenantID)
		require.NotNil(t, apiSXP.VNI)
		assert.Equal(t, 10200, *apiSXP.VNI)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, apiSXP.Status)
		assert.Len(t, apiSXP.StatusHistory, 1)
	})

	// A reused response struct must not keep status history from a previous population.
	t.Run("an absent status history clears any previous value", func(t *testing.T) {
		apiSXP := &APISpectrumXPartition{StatusHistory: []APIStatusDetail{{Status: "stale"}}}
		apiSXP.FromDB(&cdbm.SpectrumXPartition{ID: partitionID, SiteID: siteID, TenantID: tenantID, Name: "n", Status: cdbm.SpectrumXPartitionStatusPending}, nil)

		assert.Empty(t, apiSXP.StatusHistory)
	})
}

func TestNewAPISpectrumXPartitionSummary(t *testing.T) {
	t.Run("nil DB model returns nil", func(t *testing.T) {
		assert.Nil(t, NewAPISpectrumXPartitionSummary(nil))
	})

	t.Run("populates summary fields", func(t *testing.T) {
		partitionID := uuid.New()
		siteID := uuid.New()

		got := NewAPISpectrumXPartitionSummary(&cdbm.SpectrumXPartition{
			ID:     partitionID,
			Name:   "east-west-net",
			SiteID: siteID,
			VNI:    cutil.GetPtr(10200),
			Status: cdbm.SpectrumXPartitionStatusReady,
		})

		require.NotNil(t, got)
		assert.Equal(t, partitionID.String(), got.ID)
		assert.Equal(t, "east-west-net", got.Name)
		assert.Equal(t, siteID.String(), got.SiteID)
		assert.Equal(t, cdbm.SpectrumXPartitionStatusReady, got.Status)
	})
}
