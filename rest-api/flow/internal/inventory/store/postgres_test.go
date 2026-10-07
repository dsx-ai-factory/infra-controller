// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"context"
	"os"
	"strings"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"github.com/uptrace/bun"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	commonutils "github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/converter/protobuf"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	dbquery "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/query"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestUniqueRackByName(t *testing.T) {
	tests := []struct {
		name      string
		racks     []model.Rack
		wantName  string
		wantCode  codes.Code
		wantError string
	}{
		{
			name:      "no match",
			wantCode:  codes.NotFound,
			wantError: "rack shared",
		},
		{
			name:     "one match",
			racks:    []model.Rack{{Name: "shared"}},
			wantName: "shared",
		},
		{
			name:      "ambiguous match",
			racks:     []model.Rack{{Name: "shared"}, {Name: "shared"}},
			wantCode:  codes.InvalidArgument,
			wantError: `rack name "shared" matches multiple racks; use rack id`,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got, err := uniqueRackByName(tc.racks, "shared")
			if tc.wantCode != codes.OK {
				require.Error(t, err)
				assert.Equal(t, tc.wantCode, status.Code(err))
				assert.ErrorContains(t, err, tc.wantError)
				return
			}
			require.NoError(t, err)
			require.NotNil(t, got)
			assert.Equal(t, tc.wantName, got.Name)
		})
	}
}

func TestPostgresStore_GetRackByID(t *testing.T) {
	ctx := context.Background()
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := commonutils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)
	store := NewPostgres(pool)

	rackDAO := model.Rack{Name: "aggregate-rack", ExternalID: stringPtr("rack-01"), RackProfileID: stringPtr("GB200_NVL72R1_C2G4_NVIDIA")}
	require.NoError(t, rackDAO.Create(ctx, pool.DB))
	components := []model.Component{
		{
			Name:   "ready",
			Type:   devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
			RackID: rackDAO.ID,
			Status: &types.ComponentOperationStatus{Phase: types.PhaseReady},
		},
		{
			Name:   "initializing",
			Type:   devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
			RackID: rackDAO.ID,
			Status: &types.ComponentOperationStatus{Phase: types.PhaseInitializing},
		},
	}
	for i := range components {
		require.NoError(t, components[i].Create(ctx, pool.DB))
	}

	byID, err := store.GetRackByID(ctx, rackDAO.ID, false)
	require.NoError(t, err)
	assert.Equal(t, rackDAO.RackProfileID, byID.RackProfileID)
	assert.Equal(t, *rackDAO.RackProfileID, protobuf.RackTo(byID).GetRackProfileId())
	assert.Empty(t, byID.Components)
	assert.Equal(t, types.PhaseInitializing, byID.OperationStatus)

	byIDWithComponents, err := store.GetRackByID(ctx, rackDAO.ID, true)
	require.NoError(t, err)
	require.Len(t, byIDWithComponents.Components, 2)
	assert.Equal(t, byID.OperationStatus, byIDWithComponents.OperationStatus)

	listed, total, err := store.GetListOfRacks(
		ctx,
		dbquery.StringQueryInfo{},
		nil,
		nil,
		nil,
		nil,
		false,
		false,
	)
	require.NoError(t, err)
	require.Equal(t, int32(1), total)
	require.Len(t, listed, 1)
	assert.Empty(t, listed[0].Components)
	assert.Equal(t, byID.OperationStatus, listed[0].OperationStatus)
}

func TestPostgresStore_GetListOfComponents(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}

	ctx := t.Context()
	dbConf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := commonutils.UnitTestDB(ctx, t, dbConf)
	require.NoError(t, err)
	store := NewPostgres(pool)

	domain := model.NVLDomain{Name: "group-domain", ExternalID: stringPtr("group-01")}
	require.NoError(t, domain.Create(ctx, pool.DB))
	rack := model.Rack{Name: "rack", ExternalID: stringPtr("rack-01"), NVLDomainID: domain.ID}
	require.NoError(t, rack.Create(ctx, pool.DB))
	component := model.Component{
		Name:        "compute",
		Type:        devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute),
		RackID:      rack.ID,
		ComponentID: stringPtr("compute-01"),
	}
	require.NoError(t, component.Create(ctx, pool.DB))

	components, total, err := store.GetListOfComponents(
		ctx,
		dbquery.StringQueryInfo{},
		nil,
		nil,
		nil,
		nil,
		nil,
	)
	require.NoError(t, err)
	require.Equal(t, int32(1), total)
	require.Len(t, components, 1)
	assert.Equal(t, rack.ID, components[0].RackID)
	assert.Equal(t, "rack-01", components[0].RackExternalID)
	assert.Equal(t, domain.ID, components[0].NVLDomainID)
	assert.Equal(t, domain.ExternalID, components[0].NVLDomainExternalID)
	assert.Equal(t, domain.ExternalID, protobuf.ComponentTo(components[0]).NvlDomainExternalId)
}

func stringPtr(value string) *string {
	return &value
}

func TestPostgresStore_GetComponentByBMCMAC(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}
	ctx := context.Background()
	tests := []struct {
		name          string
		componentType devicetypes.ComponentType
		bmcType       devicetypes.BMCType
		storedMAC     string
		duplicate     string
		otherOwner    bool
		missing       bool
		wantCode      codes.Code
	}{
		{name: "uppercase Host", componentType: devicetypes.ComponentTypeCompute, bmcType: devicetypes.BMCTypeHost, storedMAC: "D8:AB:CD:EF:00:01"},
		{name: "lowercase DPU", componentType: devicetypes.ComponentTypeCompute, bmcType: devicetypes.BMCTypeDPU, storedMAC: "d8:ab:cd:ef:00:02"},
		{name: "mixed case NVSwitch", componentType: devicetypes.ComponentTypeNVSwitch, bmcType: devicetypes.BMCTypeHost, storedMAC: "D8:ab:Cd:eF:00:03"},
		{name: "uppercase PowerShelf", componentType: devicetypes.ComponentTypePowerShelf, bmcType: devicetypes.BMCTypeHost, storedMAC: "D8:AB:CD:EF:00:04"},
		{name: "case variants with same owner", componentType: devicetypes.ComponentTypeCompute, bmcType: devicetypes.BMCTypeHost, storedMAC: "D8:AB:CD:EF:00:05", duplicate: "d8:ab:cd:ef:00:05"},
		{name: "case variants with different owners", componentType: devicetypes.ComponentTypeCompute, bmcType: devicetypes.BMCTypeHost, storedMAC: "D8:AB:CD:EF:00:06", duplicate: "d8:ab:cd:ef:00:06", otherOwner: true, wantCode: codes.FailedPrecondition},
		{name: "unknown MAC", componentType: devicetypes.ComponentTypeCompute, bmcType: devicetypes.BMCTypeHost, storedMAC: "D8:AB:CD:EF:00:07", missing: true, wantCode: codes.NotFound},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			dbConf, err := cdb.ConfigFromEnv()
			require.NoError(t, err)
			pool, err := commonutils.UnitTestDB(ctx, t, dbConf)
			require.NoError(t, err)
			t.Cleanup(pool.Close)
			store := NewPostgres(pool)
			rack := model.Rack{Name: "MAC lookup rack", ExternalID: stringPtr("rack-mac")}
			require.NoError(t, rack.Create(ctx, pool.DB))
			owner := model.Component{Name: tc.name, Type: devicetypes.ComponentTypeToString(tc.componentType), RackID: rack.ID, ComponentID: stringPtr("component-mac")}
			require.NoError(t, owner.Create(ctx, pool.DB))
			bmcs := []model.BMC{
				{MacAddress: tc.storedMAC, Type: devicetypes.BMCTypeToString(tc.bmcType), ComponentID: owner.ID},
				{MacAddress: "00:11:22:33:44:55", Type: devicetypes.BMCTypeToString(tc.bmcType), ComponentID: owner.ID},
			}
			if tc.duplicate != "" {
				duplicateOwner := owner.ID
				if tc.otherOwner {
					other := model.Component{Name: "other owner", Type: owner.Type, RackID: rack.ID}
					require.NoError(t, other.Create(ctx, pool.DB))
					duplicateOwner = other.ID
				}
				bmcs = append(bmcs, model.BMC{MacAddress: tc.duplicate, Type: bmcs[0].Type, ComponentID: duplicateOwner})
			}
			_, err = pool.DB.NewInsert().Model(&bmcs).Exec(ctx)
			require.NoError(t, err)

			// Use the serialized response MAC, not a separately normalized test input.
			byID, err := store.GetComponentByID(ctx, owner.ID)
			require.NoError(t, err)
			wire := protobuf.ComponentTo(byID)
			var responseMAC string
			for _, controller := range wire.GetBmcs() {
				if strings.EqualFold(controller.GetMacAddress(), tc.storedMAC) {
					responseMAC = controller.GetMacAddress()
				}
			}
			require.Equal(t, strings.ToLower(tc.storedMAC), responseMAC)
			if tc.missing {
				responseMAC = "ff:ff:ff:ff:ff:ff"
			}
			got, err := store.GetComponentByBMCMAC(ctx, strings.ToUpper(responseMAC))
			require.Equal(t, tc.wantCode, status.Code(err))
			resolved, resolveErr := resolver.ResolveComponentIdentifier(ctx, store, responseMAC, tc.componentType)
			require.Equal(t, tc.wantCode, status.Code(resolveErr))
			if tc.wantCode != codes.OK {
				assert.Nil(t, got)
				assert.Nil(t, resolved)
				return
			}
			require.NotNil(t, got)
			require.NotNil(t, resolved)
			assert.Equal(t, owner.ID, got.Info.ID)
			assert.Equal(t, owner.ID, resolved.Info.ID)
			assert.Equal(t, "component-mac", resolved.ComponentID)
			assert.Equal(t, rack.ID, resolved.RackID)
			assert.Equal(t, "rack-mac", resolved.RackExternalID)
			assert.Equal(t, tc.componentType, resolved.Type)
			assert.Len(t, resolved.BmcsByType[tc.bmcType], len(bmcs), "lookup must retain companion controllers")
			var persisted []model.BMC
			err = pool.DB.NewSelect().Model(&persisted).Order("mac_address").Scan(ctx)
			require.NoError(t, err)
			assert.Len(t, persisted, len(bmcs))
			assert.Contains(t, persisted, bmcs[0], "lookup must not rewrite persisted identities")
		})
	}
}

type domainPageQueryCounter struct{ queries int }

func (c *domainPageQueryCounter) BeforeQuery(ctx context.Context, _ *bun.QueryEvent) context.Context {
	c.queries++
	return ctx
}

func (*domainPageQueryCounter) AfterQuery(context.Context, *bun.QueryEvent) {}

func TestPostgresStore_GetRacksForNVLDomains(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}
	ctx := t.Context()
	conf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := commonutils.UnitTestDB(ctx, t, conf)
	require.NoError(t, err)
	store := NewPostgres(pool)
	ids := make([]uuid.UUID, 0, 2)
	for _, name := range []string{"group-a", "group-b"} {
		domain := model.NVLDomain{Name: name, ExternalID: &name}
		require.NoError(t, domain.Create(ctx, pool.DB))
		member := model.Rack{Name: name, NVLDomainID: domain.ID}
		require.NoError(t, member.Create(ctx, pool.DB))
		comp := model.Component{Name: name, Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), RackID: member.ID}
		require.NoError(t, comp.Create(ctx, pool.DB))
		ids = append(ids, domain.ID)
	}
	counter := &domainPageQueryCounter{}
	pool.DB.AddQueryHook(counter)
	for _, tc := range []struct {
		name       string
		ids        []uuid.UUID
		components bool
	}{
		{name: "empty page"},
		{name: "single domain", ids: ids[:1]},
		{name: "multiple domains with components", ids: ids, components: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			counter.queries = 0
			got, err := store.GetRacksForNVLDomains(ctx, tc.ids, tc.components)
			require.NoError(t, err)
			require.Len(t, got, len(tc.ids))
			if len(tc.ids) == 0 {
				assert.Zero(t, counter.queries)
			} else {
				assert.LessOrEqual(t, counter.queries, 7, "page reads must not query each domain separately")
			}
			for _, id := range tc.ids {
				require.Len(t, got[id], 1)
				assert.Equal(t, id, got[id][0].NVLDomainID)
				if tc.components {
					require.Len(t, got[id][0].Components, 1)
					assert.Equal(t, got[id][0].Info.ID, got[id][0].Components[0].RackID)
				} else {
					assert.Empty(t, got[id][0].Components)
				}
			}
		})
	}
}

func TestPostgresStore_GetRacksForNVLDomain(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}
	ctx := context.Background()
	conf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := commonutils.UnitTestDB(ctx, t, conf)
	require.NoError(t, err)
	store := NewPostgres(pool)
	domain := model.NVLDomain{Name: "legacy", ExternalID: stringPtr("group-legacy")}
	require.NoError(t, domain.Create(ctx, pool.DB))
	legacy := model.Rack{Name: "legacy-rack", ExternalID: stringPtr("legacy-rack"), NVLDomainID: domain.ID}
	require.NoError(t, legacy.Create(ctx, pool.DB))
	collisionDomain := model.NVLDomain{Name: "collision", ExternalID: stringPtr("group-collision")}
	require.NoError(t, collisionDomain.Create(ctx, pool.DB))
	preferred := model.Rack{Name: "preferred", ExternalID: stringPtr("group-collision")}
	require.NoError(t, preferred.Create(ctx, pool.DB))
	comp := model.Component{Name: "compute", Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), RackID: preferred.ID}
	require.NoError(t, comp.Create(ctx, pool.DB))

	legacyComp := model.Component{Name: "legacy-compute", Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), RackID: legacy.ID}
	require.NoError(t, legacyComp.Create(ctx, pool.DB))
	tray, err := store.GetComponentByID(ctx, legacyComp.ID)
	require.NoError(t, err)
	assert.Equal(t, domain.ExternalID, tray.NVLDomainExternalID)
	memberIDs := make([]uuid.UUID, 0, 2)
	for _, name := range []string{"member-a", "member-b"} {
		member := model.Rack{Name: name, ExternalID: stringPtr(name), NVLDomainID: collisionDomain.ID}
		require.NoError(t, member.Create(ctx, pool.DB))
		memberComp := model.Component{Name: name + "-compute", Type: devicetypes.ComponentTypeToString(devicetypes.ComponentTypeCompute), RackID: member.ID}
		require.NoError(t, memberComp.Create(ctx, pool.DB))
		memberIDs = append(memberIDs, member.ID)
	}

	tests := []struct {
		name              string
		id                identifier.Identifier
		want              uuid.UUID
		wantMembers       []uuid.UUID
		code              codes.Code
		cancel            bool
		withoutComponents bool
	}{
		{name: "external group ID", id: identifier.Identifier{ExternalID: "group-legacy"}, want: legacy.ID},
		{name: "no legacy UUID fallback", id: identifier.Identifier{ExternalID: domain.ID.String()}, code: codes.NotFound},
		{name: "domain wins rack external ID collision", id: identifier.Identifier{ExternalID: "group-collision"}, wantMembers: memberIDs},
		{name: "external group without components", id: identifier.Identifier{ExternalID: "group-legacy"}, want: legacy.ID, withoutComponents: true},
		{name: "typed domain UUID expands all domain members despite rack collision", id: identifier.Identifier{ID: collisionDomain.ID}, wantMembers: memberIDs},
		{name: "domain name remains supported", id: identifier.Identifier{Name: "legacy"}, want: legacy.ID},
		{name: "unknown external ID", id: identifier.Identifier{ExternalID: "missing"}, code: codes.NotFound},
		{name: "unknown UUID", id: identifier.Identifier{ExternalID: uuid.NewString()}, code: codes.NotFound},
		{name: "cancelled lookup does not fall back", id: identifier.Identifier{ExternalID: domain.ID.String()}, cancel: true},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			callCtx, cancel := context.WithCancel(ctx)
			defer cancel()
			if tc.cancel {
				cancel()
			}
			got, err := store.GetRacksForNVLDomain(callCtx, tc.id, !tc.withoutComponents)
			if tc.cancel {
				require.Error(t, err)
				assert.Empty(t, got)
				return
			}
			if tc.code != codes.OK {
				require.Error(t, err)
				assert.Equal(t, tc.code, status.Code(err))
				return
			}
			require.NoError(t, err)
			if tc.wantMembers != nil {
				gotIDs := make([]uuid.UUID, 0, len(got))
				for _, r := range got {
					gotIDs = append(gotIDs, r.Info.ID)
				}
				assert.ElementsMatch(t, tc.wantMembers, gotIDs)
				return
			}
			require.Len(t, got, 1)
			assert.Equal(t, tc.want, got[0].Info.ID)
			assert.Equal(t, domain.ExternalID, got[0].NVLDomainExternalID)
			if tc.withoutComponents {
				assert.Empty(t, got[0].Components)
			} else {
				require.Len(t, got[0].Components, 1)
				assert.Equal(t, domain.ExternalID, got[0].Components[0].NVLDomainExternalID)
			}
		})
	}
}

func TestPostgresStore_GetListOfRacks(t *testing.T) {
	if os.Getenv("DB_PORT") == "" {
		t.Skip("Skipping integration test: no DB environment specified")
	}
	ctx := t.Context()
	conf, err := cdb.ConfigFromEnv()
	require.NoError(t, err)
	pool, err := commonutils.UnitTestDB(ctx, t, conf)
	require.NoError(t, err)
	store := NewPostgres(pool)
	rows := []model.Rack{
		{Name: "a", ExternalID: nil},
		{Name: "b", ExternalID: stringPtr("")},
		{Name: "c", ExternalID: stringPtr("rack-c")},
		{Name: "d", ExternalID: stringPtr("rack-d")},
	}
	for i := range rows {
		require.NoError(t, rows[i].Create(ctx, pool.DB))
	}
	for _, tc := range []struct {
		name         string
		offset       int
		externalOnly bool
		total        int32
		want         string
	}{
		{name: "existing rack list", total: 4, want: ""},
		{name: "first domain page", externalOnly: true, total: 2, want: "rack-c"},
		{name: "second domain page", offset: 1, externalOnly: true, total: 2, want: "rack-d"},
		{name: "past final domain page", offset: 2, externalOnly: true, total: 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, total, err := store.GetListOfRacks(ctx, dbquery.StringQueryInfo{}, nil, nil, &dbquery.Pagination{Offset: tc.offset, Limit: 1}, nil, false, tc.externalOnly)
			require.NoError(t, err)
			assert.Equal(t, tc.total, total)
			if tc.offset == 2 {
				assert.Empty(t, got)
				return
			}
			require.Len(t, got, 1)
			assert.Equal(t, tc.want, got[0].ExternalID)
		})
	}
}
