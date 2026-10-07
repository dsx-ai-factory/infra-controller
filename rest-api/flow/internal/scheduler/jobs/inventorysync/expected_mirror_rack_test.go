// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	daoconverter "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/dao"
	pbconverter "github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
)

func TestNaturalKeyIsCollisionFree(t *testing.T) {
	// The classic concatenation bug: "ab"+"cd" and "abc"+"d" collide if you
	// don't separate with a forbidden byte.
	a := naturalKey("ab", "cd")
	b := naturalKey("abc", "d")
	assert.NotEqual(t, a, b, "manufacturer/serial collisions must be impossible")
}

func TestNaturalKeyOrEmpty(t *testing.T) {
	tests := []struct {
		name         string
		manufacturer string
		serialNumber string
		wantEmpty    bool
	}{
		{name: "both halves populated", manufacturer: "Foxconn", serialNumber: "SN-1"},
		{name: "no manufacturer", serialNumber: "SN-1", wantEmpty: true},
		{name: "no serial", manufacturer: "Foxconn", wantEmpty: true},
		{name: "neither", wantEmpty: true},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := naturalKeyOrEmpty(tc.manufacturer, tc.serialNumber)
			if tc.wantEmpty {
				assert.Empty(t, got)
				return
			}
			assert.Equal(t, naturalKey(tc.manufacturer, tc.serialNumber), got)
		})
	}
}

func TestChassisSlotOwnedByOther(t *testing.T) {
	ownerID := uuid.New()
	otherID := uuid.New()
	owner := &model.Rack{
		ID:           ownerID,
		Manufacturer: "NVIDIA",
		SerialNumber: "SN-1",
	}
	flowByNaturalKey := map[string]*model.Rack{
		naturalKey("NVIDIA", "SN-1"): owner,
	}

	tests := []struct {
		name    string
		desired model.Rack
		want    bool
	}{
		{
			name:    "same rack already owns slot",
			desired: model.Rack{ID: ownerID, Manufacturer: "NVIDIA", SerialNumber: "SN-1"},
		},
		{
			name:    "another rack owns slot",
			desired: model.Rack{ID: otherID, Manufacturer: "NVIDIA", SerialNumber: "SN-1"},
			want:    true,
		},
		{
			name:    "new rack claims occupied slot",
			desired: model.Rack{Manufacturer: "NVIDIA", SerialNumber: "SN-1"},
			want:    true,
		},
		{
			name:    "slot is free",
			desired: model.Rack{ID: otherID, Manufacturer: "NVIDIA", SerialNumber: "SN-2"},
		},
		{
			name:    "incomplete pair occupies no slot",
			desired: model.Rack{ID: otherID, Manufacturer: "NVIDIA"},
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			assert.Equal(t, tc.want, chassisSlotOwnedByOther(flowByNaturalKey, &tc.desired))
		})
	}
}

func TestPlanRackNaturalKeyWinners(t *testing.T) {
	coreRacks := []nicoapi.ExpectedRackDetail{
		coreRackNamed("b34", "rack-b", "Mfg", "SHARED"),
		coreRackNamed("a12", "rack-a", "Mfg", "SHARED"),
	}
	key := naturalKey("Mfg", "SHARED")

	t.Run("rack id order is deterministic without an existing owner", func(t *testing.T) {
		winners := planRackNaturalKeyWinners(coreRacks, nil)
		assert.Equal(t, "a12", winners[key])
	})

	t.Run("the external id already holding the pair retains it", func(t *testing.T) {
		ownerID := "b34"
		winners := planRackNaturalKeyWinners(coreRacks, map[string]*model.Rack{
			key: {ExternalID: &ownerID},
		})
		assert.Equal(t, "b34", winners[key])
	})
}

func TestBuildRackFromCore(t *testing.T) {
	tests := []struct {
		name      string
		in        nicoapi.ExpectedRackDetail
		wantOK    bool
		assertRow func(t *testing.T, r model.Rack)
	}{
		{
			name: "happy path with all labels",
			in: nicoapi.ExpectedRackDetail{
				RackID: "a12",
				Name:   "Rack A12",
				Labels: map[string]string{
					labelChassisManufacturer: "Foxconn",
					labelChassisSerialNumber: "SN-A12",
					labelChassisModel:        "MGX-Rack-Gen2",
					labelLocationRegion:      "us-east",
					labelLocationDatacenter:  "DC1",
				},
				Description: "Building 1, Row 3",
			},
			wantOK: true,
			assertRow: func(t *testing.T, r model.Rack) {
				assert.Equal(t, "Rack A12", r.Name)
				assert.Equal(t, "Foxconn", r.Manufacturer)
				assert.Equal(t, "SN-A12", r.SerialNumber)
				require.NotNil(t, r.ExternalID)
				assert.Equal(t, "a12", *r.ExternalID)
				assert.Equal(t, "MGX-Rack-Gen2", r.Description["model"])
				assert.Equal(t, "Building 1, Row 3", r.Description["text"])
				assert.Equal(t, "us-east", r.Location["region"])
				assert.Equal(t, "DC1", r.Location["data_center"])
				assert.NotContains(t, r.Location, "datacenter")
				assert.NotContains(t, r.Location, "room")
				assert.NotContains(t, r.Location, "position")
			},
		},
		{
			name: "empty name falls back to rack_id so the NOT NULL name constraint holds",
			in: nicoapi.ExpectedRackDetail{
				RackID: "b07",
				Labels: map[string]string{
					labelChassisManufacturer: "Foxconn",
					labelChassisSerialNumber: "SN-B07",
				},
			},
			wantOK: true,
			assertRow: func(t *testing.T, r model.Rack) {
				assert.Equal(t, "b07", r.Name)
			},
		},
		{
			name: "missing manufacturer is mirrored without it",
			in: nicoapi.ExpectedRackDetail{
				RackID: "c01",
				Labels: map[string]string{
					labelChassisSerialNumber: "SN-C01",
				},
			},
			wantOK: true,
			assertRow: func(t *testing.T, r model.Rack) {
				assert.Empty(t, r.Manufacturer)
				assert.Equal(t, "SN-C01", r.SerialNumber)
				require.NotNil(t, r.ExternalID)
				assert.Equal(t, "c01", *r.ExternalID)
			},
		},
		{
			name: "missing serial is mirrored without it",
			in: nicoapi.ExpectedRackDetail{
				RackID: "c02",
				Labels: map[string]string{
					labelChassisManufacturer: "Foxconn",
				},
			},
			wantOK: true,
			assertRow: func(t *testing.T, r model.Rack) {
				assert.Equal(t, "Foxconn", r.Manufacturer)
				assert.Empty(t, r.SerialNumber)
			},
		},
		{
			name: "no chassis labels at all is still mirrored under rack_id",
			in: nicoapi.ExpectedRackDetail{
				RackID: "c03",
				Name:   "klamath-1",
			},
			wantOK: true,
			assertRow: func(t *testing.T, r model.Rack) {
				assert.Empty(t, r.Manufacturer)
				assert.Empty(t, r.SerialNumber)
				assert.Equal(t, "klamath-1", r.Name)
			},
		},
		{
			name: "no description/location labels leaves jsonb columns nil",
			in: nicoapi.ExpectedRackDetail{
				RackID: "d05",
				Name:   "bare",
				Labels: map[string]string{
					labelChassisManufacturer: "Foxconn",
					labelChassisSerialNumber: "SN-D05",
				},
			},
			wantOK: true,
			assertRow: func(t *testing.T, r model.Rack) {
				assert.Nil(t, r.Description)
				assert.Nil(t, r.Location)
			},
		},
		{
			name: "missing rack_id is unusable: nothing to identify the rack by",
			in: nicoapi.ExpectedRackDetail{
				Name: "noext",
				Labels: map[string]string{
					labelChassisManufacturer: "Foxconn",
					labelChassisSerialNumber: "SN-E01",
				},
			},
			wantOK: false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := buildRackFromCore(tc.in)
			assert.Equal(t, tc.wantOK, ok)
			if ok && tc.assertRow != nil {
				tc.assertRow(t, got)
			}
		})
	}
}

func TestRackFromCoreRoundTripsThroughPublicFlowModel(t *testing.T) {
	built, ok := buildRackFromCore(nicoapi.ExpectedRackDetail{
		RackID:      "a12",
		Name:        "Rack A12",
		Description: "Building 1, Row 3",
		Labels: map[string]string{
			labelChassisManufacturer: "Foxconn",
			labelChassisSerialNumber: "SN-A12",
			labelChassisModel:        "MGX-Rack-Gen2",
			labelLocationRegion:      "us-east",
			labelLocationDatacenter:  "DC1",
			labelLocationRoom:        "room-3",
			labelLocationPosition:    "row-3",
		},
	})
	require.True(t, ok)

	public := pbconverter.RackTo(daoconverter.RackFrom(&built))
	require.NotNil(t, public)
	require.NotNil(t, public.GetInfo())
	assert.Equal(t, "Foxconn", public.GetInfo().GetManufacturer())
	assert.Equal(t, "SN-A12", public.GetInfo().GetSerialNumber())
	assert.Equal(t, "MGX-Rack-Gen2", public.GetInfo().GetModel())
	assert.Equal(t, "Building 1, Row 3", public.GetInfo().GetDescription())
	require.NotNil(t, public.GetLocation())
	assert.Equal(t, "us-east", public.GetLocation().GetRegion())
	assert.Equal(t, "DC1", public.GetLocation().GetDatacenter())
	assert.Equal(t, "room-3", public.GetLocation().GetRoom())
	assert.Equal(t, "row-3", public.GetLocation().GetPosition())
}

func TestRackUpdatedFromCore(t *testing.T) {
	id := uuid.New()
	base := func() *model.Rack {
		return &model.Rack{
			ID:           id,
			Name:         "old-name",
			Manufacturer: "Foxconn",
			SerialNumber: "SN-1",
			Description:  map[string]any{"model": "old-model"},
			Location:     map[string]any{"region": "us-west"},
		}
	}

	t.Run("name change produces an update", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Name = "new-name"
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Equal(t, "new-name", got.Name)
	})

	t.Run("identical inputs produce no update", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		assert.Nil(t, rackUpdatedFromCore(existing, &fromCore))
	})

	t.Run("description swap is detected", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Description = map[string]any{"model": "new-model"}
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Equal(t, "new-model", got.Description["model"])
	})

	t.Run("location swap is detected", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Location = map[string]any{"region": "us-east"}
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Equal(t, "us-east", got.Location["region"])
	})

	t.Run("adoption: existing has nil external_id, core provides one", func(t *testing.T) {
		existing := base()
		existing.ExternalID = nil
		fromCore := *base()
		ext := "a12"
		fromCore.ExternalID = &ext
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		require.NotNil(t, got.ExternalID)
		assert.Equal(t, "a12", *got.ExternalID)
	})

	t.Run("empty name in fromCore does not clobber existing name", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Name = ""
		assert.Nil(t, rackUpdatedFromCore(existing, &fromCore))
	})

	t.Run("chassis labels are backfilled once Core starts sending them", func(t *testing.T) {
		existing := base()
		existing.Manufacturer = ""
		existing.SerialNumber = ""
		fromCore := *base()
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Equal(t, "Foxconn", got.Manufacturer)
		assert.Equal(t, "SN-1", got.SerialNumber)
	})

	t.Run("Core dropping chassis labels clears Flow's copy", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Manufacturer = ""
		fromCore.SerialNumber = ""
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Empty(t, got.Manufacturer)
		assert.Empty(t, got.SerialNumber)
	})

	t.Run("Core corrections overwrite populated chassis labels", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Manufacturer = "Wistron"
		fromCore.SerialNumber = "SN-2"
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Equal(t, "Wistron", got.Manufacturer)
		assert.Equal(t, "SN-2", got.SerialNumber)
	})

	t.Run("missing Core description and location clear Flow's copy", func(t *testing.T) {
		existing := base()
		fromCore := *base()
		fromCore.Description = nil
		fromCore.Location = nil
		got := rackUpdatedFromCore(existing, &fromCore)
		require.NotNil(t, got)
		assert.Nil(t, got.Description)
		assert.Nil(t, got.Location)
	})
}

func TestAdoptableByNaturalKey(t *testing.T) {
	coreExtIDs := map[string]struct{}{"live": {}}

	tests := []struct {
		name       string
		externalID *string
		want       bool
	}{
		{name: "no external_id has never been claimed", want: true},
		{name: "empty external_id has never been claimed", externalID: strPtr(""), want: true},
		{name: "external_id Core no longer reports may be re-pointed", externalID: strPtr("retired"), want: true},
		{name: "external_id Core still reports owns the row", externalID: strPtr("live")},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			r := &model.Rack{ExternalID: tc.externalID}
			assert.Equal(t, tc.want, adoptableByNaturalKey(r, coreExtIDs))
		})
	}
}

// errExpectedRacksClient is a tiny test wrapper around the production mock
// that overrides GetAllExpectedRackDetails to inject either an RPC error or a
// custom row set. It satisfies the same nicoapi.Client interface so it slots
// straight into pullExpectedRacks without touching the production mock.
type errExpectedRacksClient struct {
	nicoapi.Client
	err  error
	rows []nicoapi.ExpectedRackDetail
}

func (c *errExpectedRacksClient) GetAllExpectedRackDetails(_ context.Context) ([]nicoapi.ExpectedRackDetail, error) {
	if c.err != nil {
		return nil, c.err
	}
	return c.rows, nil
}

func TestPullExpectedRacks(t *testing.T) {
	ctx := context.Background()

	t.Run("rpc error returns rpcOK=false so caller leaves Flow untouched", func(t *testing.T) {
		c := &errExpectedRacksClient{Client: nicoapi.NewMockClient(), err: errors.New("boom")}
		rows, rpcOK := pullExpectedRacks(ctx, c)
		assert.Nil(t, rows)
		assert.False(t, rpcOK)
	})

	t.Run("empty response is an authoritative rpcOK=true so caller soft-deletes all", func(t *testing.T) {
		c := &errExpectedRacksClient{Client: nicoapi.NewMockClient()}
		rows, rpcOK := pullExpectedRacks(ctx, c)
		assert.Empty(t, rows)
		assert.True(t, rpcOK)
	})

	t.Run("populated response returns rpcOK=true", func(t *testing.T) {
		c := &errExpectedRacksClient{
			Client: nicoapi.NewMockClient(),
			rows: []nicoapi.ExpectedRackDetail{
				{RackID: "a12"},
			},
		}
		rows, rpcOK := pullExpectedRacks(ctx, c)
		assert.Len(t, rows, 1)
		assert.True(t, rpcOK)
	})
}
