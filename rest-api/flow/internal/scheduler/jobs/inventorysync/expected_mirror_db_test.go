// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"os"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// These tests exercise the mirror's write paths against a real database —
// the half that pure-function tests can't reach and where the
// resurrection / rename / runtime-preservation / eviction bugs lived. They
// skip without a DB (CI provides one via DB_PORT).

func mirrorTestPool(t *testing.T) (context.Context, *cdb.Session) {
	t.Helper()
	ctx := context.Background()
	if os.Getenv("DB_PORT") == "" {
		log.Warn().Msg("Not running DB-backed mirror test: no DB environment specified")
		t.SkipNow()
	}
	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := utils.UnitTestDB(ctx, t, dbConf)
	t.Cleanup(pool.Close)
	require.NoError(t, err)
	return ctx, pool
}

func TestMirrorTestPool(t *testing.T) {
	var pool *cdb.Session
	if !t.Run("session", func(t *testing.T) {
		ctx, session := mirrorTestPool(t)
		pool = session
		require.NoError(t, pool.DB.PingContext(ctx))
	}) {
		return
	}
	if pool == nil {
		t.Skip("database fixture was skipped")
	}
	defer pool.Close()
	assert.EqualError(t, pool.DB.PingContext(context.Background()), "sql: database is closed")
}

func strPtr(s string) *string { return &s }

func coreRack(rackID, mfr, serial string) nicoapi.ExpectedRackDetail {
	return nicoapi.ExpectedRackDetail{
		RackID: rackID,
		Name:   "rack-" + serial,
		Labels: map[string]string{
			labelChassisManufacturer: mfr,
			labelChassisSerialNumber: serial,
		},
	}
}

// coreRackNamed is coreRack with an explicit name for tests that need stable
// display metadata independent of the chassis pair.
func coreRackNamed(rackID, name, mfr, serial string) nicoapi.ExpectedRackDetail {
	r := coreRack(rackID, mfr, serial)
	r.Name = name
	return r
}

func computeSpec(mfr, serial, mac string) expectedComponentSpec {
	return expectedComponentSpec{
		Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		Manufacturer: mfr,
		SerialNumber: serial,
		Name:         "node-" + serial,
		BMC:          expectedBMCSpec{MACAddress: mac, IPAddress: "10.0.0.1"},
	}
}

// --- rack mirror ----------------------------------------------------------

// A successful but empty Core response soft-deletes both mirror-adopted and
// legacy racks because no remaining row can be adopted from this snapshot.
func TestMirrorRacks_EmptyCoreDeletesAllRows(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	adopted := model.Rack{Name: "adopted", Manufacturer: "Mfg", SerialNumber: "AD-1", ExternalID: strPtr("a12")}
	require.NoError(t, adopted.Create(ctx, pool.DB))
	identifiableLegacy := model.Rack{Name: "identifiable-legacy", Manufacturer: "Mfg", SerialNumber: "LG-1"}
	require.NoError(t, identifiableLegacy.Create(ctx, pool.DB))
	identitylessLegacy := []model.Rack{
		{Name: "identityless-legacy"},
		{Name: "manufacturer-only-legacy", Manufacturer: "Mfg"},
		{Name: "serial-only-legacy", SerialNumber: "LG-2"},
	}
	for i := range identitylessLegacy {
		require.NoError(t, identitylessLegacy[i].Create(ctx, pool.DB))
	}

	result := mirrorExpectedRacks(ctx, pool, nil)
	assert.Equal(t, 5, result.softDeleted, "summary count must reflect every row actually soft-deleted")

	gotAdopted, err := (&model.Rack{ID: adopted.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.NotNil(t, gotAdopted.DeletedAt, "adopted rack absent from Core must be soft-deleted")

	gotIdentifiableLegacy, err := (&model.Rack{ID: identifiableLegacy.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.NotNil(t, gotIdentifiableLegacy.DeletedAt, "unmatched legacy rack must be soft-deleted")

	for _, legacy := range identitylessLegacy {
		gotIdentitylessLegacy, err := (&model.Rack{ID: legacy.ID}).GetIncludingDeleted(ctx, pool.DB)
		require.NoError(t, err)
		assert.NotNil(t, gotIdentitylessLegacy.DeletedAt, "legacy rack without a complete identity must be soft-deleted")
	}
}

// An identity-less legacy rack and a Core rack may share a non-unique name. The
// authoritative pass removes the orphan and mirrors the Core rack immediately.
func TestMirrorRacks_IdentitylessRowCleanupDoesNotBlockDuplicateName(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	orphan := model.Rack{Name: "reserved-name"}
	require.NoError(t, orphan.Create(ctx, pool.DB))
	core := coreRackNamed("a12", "reserved-name", "Mfg", "CORE-1")

	result := mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{core})
	assert.Equal(t, 1, result.softDeleted)
	assert.Equal(t, 1, result.inserted)

	var mirrored model.Rack
	require.NoError(t, pool.DB.NewSelect().Model(&mirrored).Where("external_id = ?", "a12").Scan(ctx))
	assert.Equal(t, "reserved-name", mirrored.Name)
	gotOrphan, err := (&model.Rack{ID: orphan.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.NotNil(t, gotOrphan.DeletedAt, "legacy rack must remain recoverable as a tombstone")
}

// #1: a soft-deleted rack is resurrected (deleted_at cleared) when Core
// re-reports it, keeping the UUID stable.
func TestMirrorRacks_ResurrectOnReReport(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	r := model.Rack{Name: "res", Manufacturer: "Mfg", SerialNumber: "RS-1", ExternalID: strPtr("a12")}
	require.NoError(t, r.Create(ctx, pool.DB))
	require.NoError(t, r.Delete(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{coreRack("a12", "Mfg", "RS-1")})

	got, err := (&model.Rack{ID: r.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.DeletedAt, "re-reported rack must be resurrected (deleted_at cleared)")
	assert.Equal(t, r.ID, got.ID, "resurrection must keep the original UUID")
}

// #2: renaming a rack's Core rack_id updates external_id in place; the stale
// id must not cause a soft-delete in the same cycle.
func TestMirrorRacks_RenameKeepsRow(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	r := model.Rack{Name: "rename", Manufacturer: "Mfg", SerialNumber: "RN-1", ExternalID: strPtr("old")}
	require.NoError(t, r.Create(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{coreRack("new", "Mfg", "RN-1")})

	got, err := (&model.Rack{ID: r.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.DeletedAt, "renamed rack must not be soft-deleted")
	require.NotNil(t, got.ExternalID)
	assert.Equal(t, "new", *got.ExternalID, "external_id must be updated to Core's new rack_id")
}

// #3: a Core row missing a chassis label is authoritative and clears the
// existing Flow value without deleting the rack.
func TestMirrorRacks_MissingChassisLabelStillMirrored(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	r := model.Rack{Name: "labelled", Manufacturer: "Mfg", SerialNumber: "MF-1", ExternalID: strPtr("a12")}
	require.NoError(t, r.Create(ctx, pool.DB))

	unlabelled := nicoapi.ExpectedRackDetail{
		RackID: "a12",
		Name:   "still-here",
		Labels: map[string]string{labelChassisSerialNumber: "MF-1"}, // manufacturer missing
	}
	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{unlabelled})

	got, err := (&model.Rack{ID: r.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.DeletedAt, "rack still listed by Core must survive")
	assert.Equal(t, "still-here", got.Name, "the row must be updated, proving Core's row was not skipped")
	assert.Empty(t, got.Manufacturer, "Core omitting a label must clear Flow's stale copy")
	assert.Equal(t, "MF-1", got.SerialNumber)
}

// A Core rack carrying no chassis labels at all is mirrored under its rack_id.
// This is the case that used to be skipped outright.
func TestMirrorRacks_NoChassisLabelsStillMirrored(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		{RackID: "klamath-1", Name: "klamath-1"},
	})

	var got model.Rack
	require.NoError(t, pool.DB.NewSelect().Model(&got).Where("external_id = ?", "klamath-1").Scan(ctx))
	assert.Empty(t, got.Manufacturer)
	assert.Empty(t, got.SerialNumber)
}

// Several Core racks with no chassis labels must all be mirrored: the labels
// land as NULL, which is distinct in Postgres, so they don't collapse onto one
// slot in rack_manufacturer_serial_idx.
func TestMirrorRacks_UnlabelledRacksDoNotCollapse(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		{RackID: "klamath-1", Name: "klamath-1", Labels: map[string]string{labelChassisManufacturer: "NVIDIA"}},
		{RackID: "klamath-2", Name: "klamath-2", Labels: map[string]string{labelChassisManufacturer: "NVIDIA"}},
		{RackID: "klamath-3", Name: "klamath-3", Labels: map[string]string{labelChassisManufacturer: "NVIDIA"}},
		{RackID: "klamath-4", Name: "klamath-4", Labels: map[string]string{labelChassisManufacturer: "NVIDIA"}},
	})

	n, err := pool.DB.NewSelect().Model((*model.Rack)(nil)).Where("manufacturer = ?", "NVIDIA").Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 4, n, "every serial-less rack must be mirrored, not deduplicated onto one chassis key")
}

// A rack the mirror has never reached is adopted by its chassis pair: Core's
// rack_id is written onto the existing row rather than inserted as a new one.
func TestMirrorRacks_LegacyRackAdoptedByNaturalKey(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	legacy := model.Rack{Name: "legacy", Manufacturer: "Mfg", SerialNumber: "LG-1"}
	require.NoError(t, legacy.Create(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{coreRack("a12", "Mfg", "LG-1")})

	total, err := pool.DB.NewSelect().Model((*model.Rack)(nil)).Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 1, total, "adoption must reuse the existing row, not insert a second rack")

	got, err := (&model.Rack{ID: legacy.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.DeletedAt)
	require.NotNil(t, got.ExternalID)
	assert.Equal(t, "a12", *got.ExternalID)
}

// A rack stored without chassis labels picks them up once Core starts sending
// them.
func TestMirrorRacks_ChassisLabelsBackfilled(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	r := model.Rack{Name: "backfill", ExternalID: strPtr("a12")}
	require.NoError(t, r.Create(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{coreRack("a12", "Mfg", "BF-1")})

	got, err := (&model.Rack{ID: r.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, "Mfg", got.Manufacturer)
	assert.Equal(t, "BF-1", got.SerialNumber)
}

// A Core rack whose chassis pair matches an already-identified Flow rack must
// not take that rack's row while Core still reports the rack_id on it.
func TestMirrorRacks_AdoptionDoesNotStealAnIdentifiedRack(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	owner := model.Rack{Name: "owner", Manufacturer: "Mfg", SerialNumber: "ST-1", ExternalID: strPtr("a12")}
	require.NoError(t, owner.Create(ctx, pool.DB))

	// Both racks claim the same chassis; a12 already holds the Flow row.
	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		coreRackNamed("b34", "rack-b34", "Mfg", "ST-1"),
		coreRackNamed("a12", "rack-a12", "Mfg", "ST-1"),
	})

	got, err := (&model.Rack{ID: owner.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.DeletedAt)
	require.NotNil(t, got.ExternalID)
	assert.Equal(t, "a12", *got.ExternalID, "the row must stay with the rack_id already on it")
	assert.Equal(t, "ST-1", got.SerialNumber, "the owner keeps the contested chassis pair")

	var intruder model.Rack
	require.NoError(t, pool.DB.NewSelect().Model(&intruder).Where("external_id = ?", "b34").Scan(ctx))
	assert.Empty(t, intruder.SerialNumber, "the second rack is mirrored without the contested pair")
}

// #4: two Core racks reporting the same chassis must not abort the cycle on
// rack_manufacturer_serial_idx. Both racks are mirrored; only the first keeps
// the contested labels.
func TestMirrorRacks_DuplicateChassisNoAbort(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		coreRackNamed("a12", "rack-a12", "Mfg", "DUP-1"),
		coreRackNamed("b34", "rack-b34", "Mfg", "DUP-1"),
	})

	total, err := pool.DB.NewSelect().Model((*model.Rack)(nil)).Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 2, total, "both racks must be mirrored under their own rack_id")

	withSerial, err := pool.DB.NewSelect().Model((*model.Rack)(nil)).Where("serial_number = ?", "DUP-1").Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 1, withSerial, "only one rack may hold the contested chassis pair")
}

// Names are Core metadata, not identity. Two Core racks with the same name are
// both mirrored under their distinct external IDs.
func TestMirrorRacks_DuplicateNamesBothConverge(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		coreRackNamed("a12", "same-name", "Mfg", "NM-1"),
		coreRackNamed("b34", "same-name", "Mfg", "NM-2"),
	})

	total, err := pool.DB.NewSelect().Model((*model.Rack)(nil)).Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 2, total)

	var got []model.Rack
	require.NoError(t, pool.DB.NewSelect().Model(&got).Where("name = ?", "same-name").Order("external_id").Scan(ctx))
	require.Len(t, got, 2)
	assert.Equal(t, "a12", *got[0].ExternalID)
	assert.Equal(t, "b34", *got[1].ExternalID)
}

// #8: an empty Core description clears stale Flow metadata.
func TestMirrorRacks_EmptyDescriptionCleared(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	r := model.Rack{
		Name:         "desc",
		Manufacturer: "Mfg",
		SerialNumber: "DS-1",
		ExternalID:   strPtr("a12"),
		Description:  map[string]any{"text": "operator note"},
	}
	require.NoError(t, r.Create(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{coreRack("a12", "Mfg", "DS-1")})

	got, err := (&model.Rack{ID: r.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.Description, "empty Core description must clear stale Flow metadata")
}

func TestMirrorRacks_CoreMetadataCorrectionConvergesExistingExternalID(t *testing.T) {
	ctx, pool := mirrorTestPool(t)
	domain := model.NVLDomain{Name: "domain-a"}
	require.NoError(t, domain.Create(ctx, pool.DB))
	ingestedAt := time.Now().UTC().Truncate(time.Microsecond)

	r := model.Rack{
		Name:         "rack-a12",
		Manufacturer: "OldMfg",
		SerialNumber: "OLD-1",
		ExternalID:   strPtr("a12"),
		Description:  map[string]any{"model": "old-model", "text": "old description"},
		Location:     map[string]any{"region": "old-region", "room": "old-room"},
		NVLDomainID:  domain.ID,
		Status:       model.RackStatusIngested,
		IngestedAt:   &ingestedAt,
	}
	require.NoError(t, r.Create(ctx, pool.DB))

	core := nicoapi.ExpectedRackDetail{
		RackID:      "a12",
		Name:        "rack-a12",
		Description: "new description",
		Labels: map[string]string{
			labelChassisManufacturer: "NewMfg",
			labelChassisSerialNumber: "NEW-1",
			labelChassisModel:        "new-model",
			labelLocationRegion:      "new-region",
			labelLocationDatacenter:  "new-dc",
			labelLocationPosition:    "new-position",
		},
	}

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{core})

	got, err := (&model.Rack{ID: r.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, r.ID, got.ID)
	assert.Equal(t, "NewMfg", got.Manufacturer)
	assert.Equal(t, "NEW-1", got.SerialNumber)
	assert.Equal(t, map[string]any{"model": "new-model", "text": "new description"}, got.Description)
	assert.Equal(t, map[string]any{
		"region":      "new-region",
		"data_center": "new-dc",
		"position":    "new-position",
	}, got.Location)
	assert.Equal(t, domain.ID, got.NVLDomainID)
	assert.Equal(t, model.RackStatusIngested, got.Status)
	require.NotNil(t, got.IngestedAt)
	assert.Equal(t, ingestedAt, got.IngestedAt.UTC())
}

func TestMirrorRacks_ExternalIDCorrectionReclaimsLegacyChassisSlot(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	authoritative := model.Rack{
		Name:         "rack-a12",
		Manufacturer: "Mfg",
		SerialNumber: "OLD-1",
		ExternalID:   strPtr("a12"),
	}
	require.NoError(t, authoritative.Create(ctx, pool.DB))
	legacy := model.Rack{Name: "legacy-holder", Manufacturer: "Mfg", SerialNumber: "NEW-1"}
	require.NoError(t, legacy.Create(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		coreRackNamed("a12", "rack-a12", "Mfg", "NEW-1"),
	})

	gotAuthoritative, err := (&model.Rack{ID: authoritative.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, "NEW-1", gotAuthoritative.SerialNumber)
	gotLegacy, err := (&model.Rack{ID: legacy.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.NotNil(t, gotLegacy.DeletedAt)
	assert.Empty(t, gotLegacy.Manufacturer)
	assert.Empty(t, gotLegacy.SerialNumber)
}

func TestMirrorRacks_ExternalIDRowsCanSwapChassisSlots(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	a := model.Rack{Name: "rack-a", Manufacturer: "Mfg", SerialNumber: "A", ExternalID: strPtr("a")}
	b := model.Rack{Name: "rack-b", Manufacturer: "Mfg", SerialNumber: "B", ExternalID: strPtr("b")}
	require.NoError(t, a.Create(ctx, pool.DB))
	require.NoError(t, b.Create(ctx, pool.DB))

	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		coreRackNamed("a", "rack-a", "Mfg", "B"),
		coreRackNamed("b", "rack-b", "Mfg", "A"),
	})

	gotA, err := (&model.Rack{ID: a.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	gotB, err := (&model.Rack{ID: b.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, "B", gotA.SerialNumber)
	assert.Equal(t, "A", gotB.SerialNumber)
}

// #6: a Core rack may share a name with a different live Flow rack.
func TestMirrorRacks_NameCollisionWithLiveRackConverges(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	live := model.Rack{Name: "collide", Manufacturer: "Mfg", SerialNumber: "LIVE-1", ExternalID: strPtr("x")}
	require.NoError(t, live.Create(ctx, pool.DB))

	collidingCore := nicoapi.ExpectedRackDetail{
		RackID: "y",
		Name:   "collide", // same name, different chassis
		Labels: map[string]string{
			labelChassisManufacturer: "Mfg",
			labelChassisSerialNumber: "NEW-1",
		},
	}
	// Include the live rack's own Core row so it isn't soft-deleted for absence.
	mirrorExpectedRacks(ctx, pool, []nicoapi.ExpectedRackDetail{
		coreRackNamed("x", "collide", "Mfg", "LIVE-1"),
		collidingCore,
	})

	gotLive, err := (&model.Rack{ID: live.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, gotLive.DeletedAt, "the live rack holding the name must survive")

	n, err := pool.DB.NewSelect().Model((*model.Rack)(nil)).Where("name = ?", "collide").Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 2, n)
}

// --- component mirror -----------------------------------------------------

func compType() string {
	return devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute)
}

func TestMirrorComponents_DescriptionLifecycle(t *testing.T) {
	for _, tc := range []struct {
		name          string
		componentType devicetypes.ComponentType
		serial        string
		mac           string
	}{
		{"ExpectedMachine", devicetypes.ComponentTypeCompute, "DESC-COMPUTE", "aa:bb:cc:dd:ef:01"},
		{"ExpectedSwitch", devicetypes.ComponentTypeNVSwitch, "DESC-SWITCH", "aa:bb:cc:dd:ef:02"},
		{"ExpectedPowerShelf", devicetypes.ComponentTypePowerShelf, "DESC-POWER", "aa:bb:cc:dd:ef:03"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			componentType := devicetypes.ComponentTypeToString(tc.componentType)
			spec := expectedComponentSpec{
				Type:         componentType,
				Manufacturer: "Mfg",
				SerialNumber: tc.serial,
				Name:         tc.serial,
				Description:  "initial description",
				BMC:          expectedBMCSpec{MACAddress: tc.mac},
			}

			mirrorExpectedComponents(ctx, pool, componentType, []expectedComponentSpec{spec}, map[string]uuid.UUID{})

			loadComponent := func() model.Component {
				var component model.Component
				err := pool.DB.NewSelect().Model(&component).
					Where("manufacturer = ? AND serial_number = ?", spec.Manufacturer, spec.SerialNumber).
					Scan(ctx)
				require.NoError(t, err)
				return component
			}

			component := loadComponent()
			assert.Equal(t, "initial description", component.Description[expectedDescriptionKey])

			component.Description["operator"] = "keep"
			component.Description["nvos_ip"] = "10.0.0.2"
			_, err := pool.DB.NewUpdate().Model(&component).Column("description").WherePK().Exec(ctx)
			require.NoError(t, err)

			spec.Description = "updated description"
			mirrorExpectedComponents(ctx, pool, componentType, []expectedComponentSpec{spec}, map[string]uuid.UUID{})
			component = loadComponent()
			assert.Equal(t, "updated description", component.Description[expectedDescriptionKey])
			assert.Equal(t, "keep", component.Description["operator"])
			assert.Equal(t, "10.0.0.2", component.Description["nvos_ip"])

			spec.Description = ""
			mirrorExpectedComponents(ctx, pool, componentType, []expectedComponentSpec{spec}, map[string]uuid.UUID{})
			component = loadComponent()
			assert.NotContains(t, component.Description, expectedDescriptionKey)
			assert.Equal(t, "keep", component.Description["operator"])
			assert.Equal(t, "10.0.0.2", component.Description["nvos_ip"])
		})
	}
}

func TestUpdateMirroredComponent_PreservesRuntimeWriteAfterReconciliationRead(t *testing.T) {
	for _, tc := range []struct {
		name                string
		expectedDescription string
		wantExpected        bool
	}{
		{"set expected description", "updated description", true},
		{"clear expected description", "", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			component := model.Component{
				Type:         compType(),
				Manufacturer: "Mfg",
				SerialNumber: "INTERLEAVE-" + tc.name,
				Name:         "before",
				Description: map[string]any{
					expectedDescriptionKey: "initial description",
					"operator":             "keep",
				},
			}
			require.NoError(t, component.Create(ctx, pool.DB))

			var reconciliationSnapshot model.Component
			require.NoError(t, pool.DB.NewSelect().Model(&reconciliationSnapshot).Where("id = ?", component.ID).Scan(ctx))

			// Simulate runtime sync committing after reconciliation read the row
			// but before the mirror writes its planned update.
			_, err := pool.DB.NewUpdate().
				Model((*model.Component)(nil)).
				Set(
					"description = jsonb_set(COALESCE(description, '{}'::jsonb), ARRAY[?::text], to_jsonb(?::text), true)",
					nvosIPDescriptionKey,
					"10.0.0.2",
				).
				Where("id = ?", component.ID).
				Exec(ctx)
			require.NoError(t, err)

			reconciliationSnapshot.Name = "after"
			reconciliationSnapshot.UpdatedAt = time.Now()
			require.NoError(t, updateMirroredComponent(ctx, pool.DB, &reconciliationSnapshot, tc.expectedDescription))

			var updated model.Component
			require.NoError(t, pool.DB.NewSelect().Model(&updated).Where("id = ?", component.ID).Scan(ctx))
			assert.Equal(t, "after", updated.Name)
			assert.Equal(t, "10.0.0.2", updated.Description[nvosIPDescriptionKey])
			assert.Equal(t, "keep", updated.Description["operator"])
			if tc.wantExpected {
				assert.Equal(t, tc.expectedDescription, updated.Description[expectedDescriptionKey])
			} else {
				assert.NotContains(t, updated.Description, expectedDescriptionKey)
			}
		})
	}
}

// #11: a successful but empty Core response soft-deletes all Flow components
// of the type.
func TestMirrorComponents_EmptyCoreSoftDeletesAll(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	c := model.Component{Type: compType(), Manufacturer: "Mfg", SerialNumber: "C-DEL-1"}
	require.NoError(t, c.Create(ctx, pool.DB))

	result := mirrorExpectedComponents(ctx, pool, compType(), nil, map[string]uuid.UUID{})
	assert.Equal(t, 1, result.softDeleted, "summary count must reflect the row actually soft-deleted")

	got, err := (&model.Component{ID: c.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.NotNil(t, got.DeletedAt, "component absent from a successful empty Core response must be soft-deleted")
}

// #1: a soft-deleted component is resurrected when Core re-reports it.
func TestMirrorComponents_ResurrectOnReReport(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	c := model.Component{Type: compType(), Manufacturer: "Mfg", SerialNumber: "C-RES-1"}
	require.NoError(t, c.Create(ctx, pool.DB))
	require.NoError(t, c.Delete(ctx, pool.DB))

	mirrorExpectedComponents(ctx, pool, compType(),
		[]expectedComponentSpec{computeSpec("Mfg", "C-RES-1", "aa:bb:cc:dd:ee:01")},
		map[string]uuid.UUID{})

	got, err := (&model.Component{ID: c.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Nil(t, got.DeletedAt, "re-reported component must be resurrected")
	assert.Equal(t, c.ID, got.ID, "resurrection must keep the original UUID")
}

// #5: an UPDATE must touch only mirror-managed columns and leave runtime-owned
// columns (external_id, power_state, firmware_version) intact.
func TestMirrorComponents_UpdatePreservesRuntimeColumns(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	on := nicoapi.PowerStateOn
	c := model.Component{
		Type:            compType(),
		Manufacturer:    "Mfg",
		SerialNumber:    "C-UPD-1",
		Model:           "old-model",
		ComponentID:     strPtr("runtime-ext-id"),
		PowerState:      &on,
		FirmwareVersion: "9.9.9",
	}
	require.NoError(t, c.Create(ctx, pool.DB))
	hostBMC := model.BMC{
		MacAddress:  "aa:bb:cc:dd:ee:10",
		Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
		ComponentID: c.ID,
		IPAddress:   strPtr("10.0.0.1"),
	}
	_, err := pool.DB.NewInsert().Model(&hostBMC).Exec(ctx)
	require.NoError(t, err)

	spec := computeSpec("Mfg", "C-UPD-1", "aa:bb:cc:dd:ee:10")
	spec.Model = "new-model"
	mirrorExpectedComponents(ctx, pool, compType(), []expectedComponentSpec{spec}, map[string]uuid.UUID{})

	got, err := (&model.Component{ID: c.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Equal(t, "new-model", got.Model, "mirror-managed model must be updated")
	require.NotNil(t, got.ComponentID)
	assert.Equal(t, "runtime-ext-id", *got.ComponentID, "external_id is runtime-owned, must survive")
	require.NotNil(t, got.PowerState)
	assert.Equal(t, nicoapi.PowerStateOn, *got.PowerState, "power_state is runtime-owned, must survive")
	assert.Equal(t, "9.9.9", got.FirmwareVersion, "firmware_version is runtime-owned, must survive")
}

func TestMirrorComponents_PositionPresence(t *testing.T) {
	for _, tc := range []struct {
		name        string
		labels      map[string]string
		preexisting bool
		wantSlot    int
		wantTray    int
		wantHostID  int
	}{
		{
			name:        "missing labels clear stale position to unknown",
			labels:      map[string]string{},
			preexisting: true,
			wantSlot:    unknownPositionValue,
			wantTray:    unknownPositionValue,
			wantHostID:  unknownPositionValue,
		},
		{
			name: "explicit zero remains valid",
			labels: map[string]string{
				labelComponentSlotID:  "0",
				labelComponentTrayIdx: "0",
				labelComponentHostID:  "0",
			},
			preexisting: true,
			wantSlot:    0,
			wantTray:    0,
			wantHostID:  0,
		},
		{
			name:       "fresh missing labels insert unknown",
			labels:     map[string]string{},
			wantSlot:   unknownPositionValue,
			wantTray:   unknownPositionValue,
			wantHostID: unknownPositionValue,
		},
		{
			name: "fresh malformed labels insert unknown",
			labels: map[string]string{
				labelComponentSlotID:  "not-a-slot",
				labelComponentTrayIdx: "not-a-tray",
				labelComponentHostID:  "not-a-host",
			},
			wantSlot:   unknownPositionValue,
			wantTray:   unknownPositionValue,
			wantHostID: unknownPositionValue,
		},
		{
			name: "negative labels preserve existing position",
			labels: map[string]string{
				labelComponentSlotID:  "-2",
				labelComponentTrayIdx: "-3",
				labelComponentHostID:  "-4",
			},
			preexisting: true,
			wantSlot:    7,
			wantTray:    8,
			wantHostID:  9,
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			const mac = "aa:bb:cc:dd:ee:11"
			var componentID uuid.UUID
			if tc.preexisting {
				component := model.Component{
					Type:         compType(),
					Manufacturer: "Mfg",
					SerialNumber: "POSITION-1",
					SlotID:       7,
					TrayIndex:    8,
					HostID:       9,
				}
				require.NoError(t, component.Create(ctx, pool.DB))
				componentID = component.ID
				_, err := pool.DB.NewInsert().Model(&model.BMC{
					MacAddress:  mac,
					Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
					ComponentID: component.ID,
				}).Exec(ctx)
				require.NoError(t, err)
			}

			detail := nicoapi.ExpectedMachineDetail{
				BMCMACAddress:       mac,
				ChassisSerialNumber: "POSITION-1",
				Labels:              tc.labels,
			}
			mirrorExpectedComponents(
				ctx,
				pool,
				compType(),
				[]expectedComponentSpec{machineDetailToSpec(detail)},
				map[string]uuid.UUID{},
			)

			if componentID == uuid.Nil {
				var bmc model.BMC
				require.NoError(t, pool.DB.NewSelect().Model(&bmc).Where("mac_address = ?", mac).Scan(ctx))
				componentID = bmc.ComponentID
			}
			got, err := (&model.Component{ID: componentID}).GetIncludingDeleted(ctx, pool.DB)
			require.NoError(t, err)
			assert.Equal(t, tc.wantSlot, got.SlotID)
			assert.Equal(t, tc.wantTray, got.TrayIndex)
			assert.Equal(t, tc.wantHostID, got.HostID)
		})
	}
}

// #6: a host BMC insert whose MAC collides with an existing non-host (DPU) BMC
// must be refused — the DPU row must not be evicted.
func TestMirrorComponents_EvictRefusesNonHostBMC(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	const sharedMAC = "aa:bb:cc:dd:ee:50"

	// Component A keeps a host BMC (so it isn't re-inserted) plus a DPU BMC
	// on the contested MAC.
	a := model.Component{Type: compType(), Manufacturer: "Mfg", SerialNumber: "C-A"}
	require.NoError(t, a.Create(ctx, pool.DB))
	for _, b := range []model.BMC{
		{MacAddress: "aa:bb:cc:dd:ee:0a", Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeHost), ComponentID: a.ID, IPAddress: strPtr("10.0.0.10")},
		{MacAddress: sharedMAC, Type: devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU), ComponentID: a.ID, IPAddress: strPtr("10.0.0.50")},
	} {
		b := b
		_, err := pool.DB.NewInsert().Model(&b).Exec(ctx)
		require.NoError(t, err)
	}

	specs := []expectedComponentSpec{
		computeSpec("Mfg", "C-A", "aa:bb:cc:dd:ee:0a"), // matches A's existing host BMC
		computeSpec("Mfg", "C-B", sharedMAC),           // new component, host BMC collides with A's DPU
	}
	mirrorExpectedComponents(ctx, pool, compType(), specs, map[string]uuid.UUID{})

	// A's DPU BMC must still be present and still a DPU.
	var dpu model.BMC
	err := pool.DB.NewSelect().Model(&dpu).Where("mac_address = ?", sharedMAC).Scan(ctx)
	require.NoError(t, err, "the DPU BMC must not have been evicted")
	assert.Equal(t, devicetypes.BMCTypeToString(devicetypes.BMCTypeDPU), dpu.Type)
	assert.Equal(t, a.ID, dpu.ComponentID, "DPU BMC must still belong to component A")

	// B must be inserted but carry no BMC (the colliding host insert was skipped).
	b, err := (&model.Component{Manufacturer: "Mfg", SerialNumber: "C-B"}).Get(ctx, pool.DB)
	require.NoError(t, err)
	assert.Empty(t, b.BMCs, "B's host BMC insert must be skipped, not steal the DPU MAC")
}

// #4: two specs reporting the same manufacturer and serial must not abort the
// transaction on component_manufacturer_serial_idx. Both are mirrored under
// their own BMC MAC; only the first keeps the contested labels.
func TestMirrorComponents_DuplicateSerialNoAbort(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	specs := []expectedComponentSpec{
		computeSpec("Mfg", "C-DUP", "aa:bb:cc:dd:ee:21"),
		computeSpec("Mfg", "C-DUP", "aa:bb:cc:dd:ee:22"),
	}
	mirrorExpectedComponents(ctx, pool, compType(), specs, map[string]uuid.UUID{})

	total, err := pool.DB.NewSelect().Model((*model.Component)(nil)).Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 2, total, "both components must be mirrored under their own BMC MAC")

	withSerial, err := pool.DB.NewSelect().Model((*model.Component)(nil)).Where("serial_number = ?", "C-DUP").Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 1, withSerial, "only one component may hold the contested manufacturer and serial")
}

// Core reporting the same BMC MAC on two specs is a Core-side fault:
// bmc.mac_address is a primary key, so the later spec is dropped.
func TestMirrorComponents_DuplicateMACSkipsLaterSpec(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	specs := []expectedComponentSpec{
		computeSpec("Mfg", "C-MAC-1", "aa:bb:cc:dd:ee:31"),
		computeSpec("Mfg", "C-MAC-2", "aa:bb:cc:dd:ee:31"),
	}
	mirrorExpectedComponents(ctx, pool, compType(), specs, map[string]uuid.UUID{})

	total, err := pool.DB.NewSelect().Model((*model.Component)(nil)).Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 1, total, "only the first spec on a MAC may be mirrored")
}

// A spec carrying only a BMC MAC is mirrored; manufacturer and serial land as
// NULL rather than blocking the row.
func TestMirrorComponents_NoLabelsStillMirrored(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	mirrorExpectedComponents(ctx, pool, compType(),
		[]expectedComponentSpec{computeSpec("", "", "aa:bb:cc:dd:ee:41")},
		map[string]uuid.UUID{})

	var bmc model.BMC
	require.NoError(t, pool.DB.NewSelect().Model(&bmc).Where("mac_address = ?", "aa:bb:cc:dd:ee:41").Scan(ctx))

	got, err := (&model.Component{ID: bmc.ComponentID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	assert.Empty(t, got.Manufacturer)
	assert.Empty(t, got.SerialNumber)
}

// Clearing labels must store SQL NULL rather than empty strings so several
// components can remain unlabelled without colliding on the unique index.
func TestMirrorComponents_ClearingLabelsReleasesUniqueSlots(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	specs := []expectedComponentSpec{
		computeSpec("Mfg", "C-CLEAR-1", "aa:bb:cc:dd:ee:45"),
		computeSpec("Mfg", "C-CLEAR-2", "aa:bb:cc:dd:ee:46"),
	}
	mirrorExpectedComponents(ctx, pool, compType(), specs, map[string]uuid.UUID{})

	for i := range specs {
		specs[i].Manufacturer = ""
		specs[i].SerialNumber = ""
	}
	mirrorExpectedComponents(ctx, pool, compType(), specs, map[string]uuid.UUID{})

	nullLabels, err := pool.DB.NewSelect().
		Model((*model.Component)(nil)).
		Where("manufacturer IS NULL").
		Where("serial_number IS NULL").
		Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, len(specs), nullLabels)
}

// Relabelling a chassis in Core must not fork the component: the host BMC MAC
// is its identity, so the existing row is updated in place.
func TestMirrorComponents_MatchByMACSurvivesRelabel(t *testing.T) {
	for _, tc := range []struct {
		name          string
		componentType devicetypes.ComponentType
		mac           string
	}{
		{"ExpectedMachine", devicetypes.ComponentTypeCompute, "aa:bb:cc:dd:ee:51"},
		{"ExpectedSwitch", devicetypes.ComponentTypeNVSwitch, "aa:bb:cc:dd:ee:52"},
		{"ExpectedPowerShelf", devicetypes.ComponentTypePowerShelf, "aa:bb:cc:dd:ee:53"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			componentType := devicetypes.ComponentTypeToString(tc.componentType)
			component := model.Component{
				Name:         "component",
				Type:         componentType,
				Manufacturer: "OldMfg",
				SerialNumber: "OLD-SERIAL",
				Description:  map[string]any{expectedDescriptionKey: "old description"},
			}
			require.NoError(t, component.Create(ctx, pool.DB))
			hostBMC := model.BMC{
				MacAddress:  tc.mac,
				Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
				ComponentID: component.ID,
			}
			_, err := pool.DB.NewInsert().Model(&hostBMC).Exec(ctx)
			require.NoError(t, err)

			spec := expectedComponentSpec{
				Type:         componentType,
				Name:         "component",
				Manufacturer: "NewMfg",
				SerialNumber: "NEW-SERIAL",
				Description:  "new description",
				BMC:          expectedBMCSpec{MACAddress: tc.mac},
			}
			mirrorExpectedComponents(ctx, pool, componentType,
				[]expectedComponentSpec{spec}, map[string]uuid.UUID{})

			total, err := pool.DB.NewSelect().Model((*model.Component)(nil)).Count(ctx)
			require.NoError(t, err)
			assert.Equal(t, 1, total, "the MAC match must update in place, not insert a second component")

			got, err := (&model.Component{ID: component.ID}).GetIncludingDeleted(ctx, pool.DB)
			require.NoError(t, err)
			assert.Nil(t, got.DeletedAt)
			assert.Equal(t, component.ID, got.ID)
			assert.Equal(t, "NewMfg", got.Manufacturer)
			assert.Equal(t, "NEW-SERIAL", got.SerialNumber)
			assert.Equal(t, "new description", got.Description[expectedDescriptionKey])

			spec.Manufacturer = ""
			spec.SerialNumber = ""
			mirrorExpectedComponents(ctx, pool, componentType,
				[]expectedComponentSpec{spec}, map[string]uuid.UUID{})

			got, err = (&model.Component{ID: component.ID}).GetIncludingDeleted(ctx, pool.DB)
			require.NoError(t, err)
			assert.Empty(t, got.Manufacturer, "Core clearing manufacturer must clear Flow's stale value")
			assert.Empty(t, got.SerialNumber, "Core clearing serial_number must clear Flow's stale value")
			assert.Equal(t, component.ID, got.ID, "clearing descriptive labels must keep the component UUID")
		})
	}
}

// Label transfers and swaps must converge in one pass regardless of spec order.
// The host BMC MAC, not the chassis pair, identifies each component.
func TestMirrorComponents_ChassisLabelOwnershipTransitions(t *testing.T) {
	type labels struct {
		name         string
		manufacturer string
		serial       string
		mac          string
	}

	for _, tc := range []struct {
		name    string
		initial []labels
		desired []labels
	}{
		{
			name: "transfer is ordered recipient before current owner",
			initial: []labels{
				{name: "owner", manufacturer: "Mfg", serial: "PAIR-A", mac: "aa:bb:cc:dd:ee:71"},
				{name: "recipient", manufacturer: "Mfg", serial: "PAIR-B", mac: "aa:bb:cc:dd:ee:72"},
			},
			desired: []labels{
				{name: "recipient", manufacturer: "Mfg", serial: "PAIR-A", mac: "aa:bb:cc:dd:ee:72"},
				{name: "owner", manufacturer: "Mfg", serial: "PAIR-C", mac: "aa:bb:cc:dd:ee:71"},
			},
		},
		{
			name: "two components swap pairs",
			initial: []labels{
				{name: "left", manufacturer: "Mfg", serial: "PAIR-LEFT", mac: "aa:bb:cc:dd:ee:73"},
				{name: "right", manufacturer: "Mfg", serial: "PAIR-RIGHT", mac: "aa:bb:cc:dd:ee:74"},
			},
			desired: []labels{
				{name: "left", manufacturer: "Mfg", serial: "PAIR-RIGHT", mac: "aa:bb:cc:dd:ee:73"},
				{name: "right", manufacturer: "Mfg", serial: "PAIR-LEFT", mac: "aa:bb:cc:dd:ee:74"},
			},
		},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			idsByName := make(map[string]uuid.UUID, len(tc.initial))
			for _, initial := range tc.initial {
				component := model.Component{
					Name:         initial.name,
					Type:         compType(),
					Manufacturer: initial.manufacturer,
					SerialNumber: initial.serial,
				}
				require.NoError(t, component.Create(ctx, pool.DB))
				idsByName[initial.name] = component.ID
				bmc := model.BMC{
					MacAddress:  initial.mac,
					Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
					ComponentID: component.ID,
				}
				_, err := pool.DB.NewInsert().Model(&bmc).Exec(ctx)
				require.NoError(t, err)
			}

			specs := make([]expectedComponentSpec, 0, len(tc.desired))
			for _, desired := range tc.desired {
				specs = append(specs, expectedComponentSpec{
					Type:         compType(),
					Name:         desired.name,
					Manufacturer: desired.manufacturer,
					SerialNumber: desired.serial,
					BMC:          expectedBMCSpec{MACAddress: desired.mac},
				})
			}

			mirrorExpectedComponents(ctx, pool, compType(), specs, map[string]uuid.UUID{})

			total, err := pool.DB.NewSelect().Model((*model.Component)(nil)).Count(ctx)
			require.NoError(t, err)
			assert.Equal(t, len(tc.initial), total)
			for _, desired := range tc.desired {
				id := idsByName[desired.name]
				got, err := (&model.Component{ID: id}).GetIncludingDeleted(ctx, pool.DB)
				require.NoError(t, err)
				assert.Nil(t, got.DeletedAt)
				assert.Equal(t, desired.manufacturer, got.Manufacturer)
				assert.Equal(t, desired.serial, got.SerialNumber)
			}
		})
	}
}

func TestMirrorComponents_ClaimsNaturalKeyAcrossComponentTypes(t *testing.T) {
	for _, tc := range []struct {
		name              string
		existingRecipient bool
	}{
		{name: "update", existingRecipient: true},
		{name: "insert"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			owner := model.Component{
				Name:         "switch-owner",
				Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
				Manufacturer: "Mfg",
				SerialNumber: "CROSS-TYPE-PAIR",
			}
			require.NoError(t, owner.Create(ctx, pool.DB))

			recipientMAC := "aa:bb:cc:dd:ee:81"
			var recipientID uuid.UUID
			if tc.existingRecipient {
				recipient := model.Component{
					Name:         "compute-recipient",
					Type:         compType(),
					Manufacturer: "Mfg",
					SerialNumber: "OLD-COMPUTE-PAIR",
				}
				require.NoError(t, recipient.Create(ctx, pool.DB))
				recipientID = recipient.ID
				_, err := pool.DB.NewInsert().Model(&model.BMC{
					MacAddress:  recipientMAC,
					Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
					ComponentID: recipient.ID,
				}).Exec(ctx)
				require.NoError(t, err)
			}

			spec := expectedComponentSpec{
				Type:         compType(),
				Name:         "compute-recipient",
				Manufacturer: "Mfg",
				SerialNumber: "CROSS-TYPE-PAIR",
				BMC:          expectedBMCSpec{MACAddress: recipientMAC},
			}
			mirrorExpectedComponents(ctx, pool, compType(), []expectedComponentSpec{spec}, map[string]uuid.UUID{})

			var recipient model.Component
			require.NoError(t, pool.DB.NewSelect().Model(&recipient).Where("name = ?", spec.Name).Scan(ctx))
			if tc.existingRecipient {
				assert.Equal(t, recipientID, recipient.ID, "an update must keep the recipient UUID")
			}
			assert.Equal(t, spec.Manufacturer, recipient.Manufacturer)
			assert.Equal(t, spec.SerialNumber, recipient.SerialNumber)

			gotOwner, err := (&model.Component{ID: owner.ID}).GetIncludingDeleted(ctx, pool.DB)
			require.NoError(t, err)
			assert.Empty(t, gotOwner.Manufacturer)
			assert.Empty(t, gotOwner.SerialNumber)
		})
	}
}

// Swapping a BMC board changes the MAC Core reports. The natural key adopts the
// existing row and the BMC is repointed, so the component keeps its UUID.
func TestMirrorComponents_BMCBoardSwapAdoptsByNaturalKey(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	c := model.Component{Type: compType(), Manufacturer: "Mfg", SerialNumber: "C-SWAP"}
	require.NoError(t, c.Create(ctx, pool.DB))
	oldBMC := model.BMC{
		MacAddress:  "aa:bb:cc:dd:ee:61",
		Type:        devicetypes.BMCTypeToString(devicetypes.BMCTypeHost),
		ComponentID: c.ID,
	}
	_, err := pool.DB.NewInsert().Model(&oldBMC).Exec(ctx)
	require.NoError(t, err)

	mirrorExpectedComponents(ctx, pool, compType(),
		[]expectedComponentSpec{computeSpec("Mfg", "C-SWAP", "aa:bb:cc:dd:ee:62")},
		map[string]uuid.UUID{})

	total, err := pool.DB.NewSelect().Model((*model.Component)(nil)).Count(ctx)
	require.NoError(t, err)
	assert.Equal(t, 1, total, "the natural key must adopt the existing row, not insert a second component")

	got, err := (&model.Component{ID: c.ID}).GetIncludingDeleted(ctx, pool.DB)
	require.NoError(t, err)
	require.Len(t, got.BMCs, 1)
	assert.Equal(t, "aa:bb:cc:dd:ee:62", got.BMCs[0].MacAddress, "the host BMC must be repointed to Core's new MAC")
}

// A Flow component with neither a host BMC nor a complete chassis pair cannot
// join the successful authoritative snapshot, so reconciliation removes it
// rather than retaining stale live inventory.
func TestMirrorComponents_UnmatchableRowSoftDeleted(t *testing.T) {
	ctx, pool := mirrorTestPool(t)

	components := []model.Component{
		{Type: compType(), Name: "orphan"},
		{Type: compType(), Name: "manufacturer-only-orphan", Manufacturer: "Mfg"},
		{Type: compType(), Name: "serial-only-orphan", SerialNumber: "C-ORPHAN"},
	}
	for i := range components {
		require.NoError(t, components[i].Create(ctx, pool.DB))
	}

	result := mirrorExpectedComponents(ctx, pool, compType(), nil, map[string]uuid.UUID{})
	assert.Equal(t, 3, result.softDeleted, "summary count must reflect the three rows actually soft-deleted")

	for _, component := range components {
		got, err := (&model.Component{ID: component.ID}).GetIncludingDeleted(ctx, pool.DB)
		require.NoError(t, err)
		assert.NotNil(t, got.DeletedAt, "a component without a complete expected-inventory identity must be soft-deleted")
	}
}
