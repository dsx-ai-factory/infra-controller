// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"errors"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"testing"
)

type failDomainMembershipClient struct{ nicoapi.Client }

func (c *failDomainMembershipClient) GetObservedNVLinkDomainMemberships(_ context.Context) ([]nicoapi.NVLinkDomainMembership, error) {
	return nil, errors.New("domain snapshot unavailable")
}
func TestPullObservedNVLinkDomainMemberships(t *testing.T) {
	rows, ok := pullObservedNVLinkDomainMemberships(t.Context(), &failDomainMembershipClient{})
	assert.False(t, ok)
	assert.Nil(t, rows)
}

func TestBuildDomainTopologySnapshot(t *testing.T) {
	a, b, c := uuid.New(), uuid.New(), uuid.New()
	x, y := uuid.New(), uuid.New()
	racks := map[string]uuid.UUID{"a": a, "b": b, "c": c}
	groups := map[string]string{"a": "group-ab", "b": "group-ab", "c": "group-c"}
	for _, tc := range []struct {
		name    string
		rows    []nicoapi.NVLinkDomainMembership
		invalid bool
		cluster *uuid.UUID
	}{
		{name: "group without switches"},
		{name: "duplicates and multiple racks", rows: []nicoapi.NVLinkDomainMembership{{RackID: "a", DomainID: x.String()}, {RackID: "a", DomainID: x.String()}, {RackID: "b", DomainID: x.String()}}, cluster: &x},
		{name: "unknown invalid observation is skipped", rows: []nicoapi.NVLinkDomainMembership{{RackID: "unknown", DomainID: "invalid"}, {RackID: "a", DomainID: x.String()}}, cluster: &x},
		{name: "invalid known observation", rows: []nicoapi.NVLinkDomainMembership{{RackID: "a", DomainID: "invalid"}}, invalid: true},
		{name: "conflicting group observations", rows: []nicoapi.NVLinkDomainMembership{{RackID: "a", DomainID: x.String()}, {RackID: "b", DomainID: y.String()}}, invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rows := append(append([]nicoapi.NVLinkDomainMembership{}, tc.rows...), nicoapi.NVLinkDomainMembership{RackID: "c", DomainID: y.String()})
			got := buildDomainTopologySnapshot(rows, racks, groups)
			assert.Equal(t, map[uuid.UUID]string{a: "group-ab", b: "group-ab", c: "group-c"}, got.groupByRack)
			assert.Equal(t, tc.invalid, got.invalidGroups["group-ab"])
			assert.Equal(t, tc.cluster, got.clusterByGroup["group-ab"])
			assert.Equal(t, &y, got.clusterByGroup["group-c"])
		})
	}
}

func TestMirrorObservedNVLinkDomainMemberships(t *testing.T) {
	for _, scenario := range []string{"lifecycle", "rollback"} {
		t.Run(scenario, func(t *testing.T) {
			ctx, pool := mirrorTestPool(t)
			legacy := model.NVLDomain{ID: uuid.New(), Name: "legacy"}
			require.NoError(t, legacy.Create(ctx, pool.DB))
			a := model.Rack{Name: "a", ExternalID: strPtr("a"), NVLDomainID: legacy.ID}
			b := model.Rack{Name: "b", ExternalID: strPtr("b")}
			require.NoError(t, a.Create(ctx, pool.DB))
			require.NoError(t, b.Create(ctx, pool.DB))
			racks := map[string]uuid.UUID{"a": a.ID, "b": b.ID}
			groups := map[string]string{"a": "group-ab", "b": "group-ab"}
			x, y := uuid.New(), uuid.New()
			rows := []nicoapi.NVLinkDomainMembership{{RackID: "a", DomainID: x.String()}}
			if scenario == "rollback" {
				_, err := pool.DB.ExecContext(ctx, "ALTER TABLE nvldomain ADD CONSTRAINT reject_group CHECK (external_id <> 'group-ab')")
				require.NoError(t, err)
				result, err := mirrorObservedNVLinkDomainMemberships(ctx, pool, rows, racks, groups)
				require.Error(t, err)
				assert.Equal(t, domainMirrorResult{pulled: 1}, result)
				stored, err := a.Get(ctx, pool.DB, false)
				require.NoError(t, err)
				assert.Equal(t, legacy.ID, stored.NVLDomainID)
				assert.Nil(t, stored.RackGroupID)
				return
			}
			result, err := mirrorObservedNVLinkDomainMemberships(ctx, pool, rows, racks, groups)
			require.NoError(t, err)
			assert.Equal(t, 1, result.domainsInserted)
			assert.Equal(t, 2, result.membershipsAssigned)
			assert.Equal(t, 1, result.domainsSoftDeleted)
			domain, err := (&model.NVLDomain{ExternalID: strPtr("group-ab")}).Get(ctx, pool.DB)
			require.NoError(t, err)
			assert.NotEqual(t, x, domain.ID)
			assert.Empty(t, domain.Name)
			var unnamed bool
			require.NoError(t, pool.DB.NewRaw("SELECT name IS NULL FROM nvldomain WHERE id = ?", domain.ID).Scan(ctx, &unnamed))
			assert.True(t, unnamed)
			assert.Equal(t, &x, domain.NMXCClusterID)
			for _, rack := range []*model.Rack{&a, &b} {
				stored, err := rack.Get(ctx, pool.DB, false)
				require.NoError(t, err)
				assert.Equal(t, domain.ID, stored.NVLDomainID)
				assert.Equal(t, strPtr("group-ab"), stored.RackGroupID)
			}
			_, err = pool.DB.NewUpdate().Model(domain).Set("name = ?", "operator-name").Where("id = ?", domain.ID).Exec(ctx)
			require.NoError(t, err)
			result, err = mirrorObservedNVLinkDomainMemberships(ctx, pool, rows, racks, groups)
			require.NoError(t, err)
			assert.Equal(t, domainMirrorResult{pulled: 1}, result)
			rows = append(rows, nicoapi.NVLinkDomainMembership{RackID: "b", DomainID: y.String()})
			_, err = mirrorObservedNVLinkDomainMemberships(ctx, pool, rows, racks, groups)
			require.NoError(t, err)
			domain, err = domain.Get(ctx, pool.DB)
			require.NoError(t, err)
			assert.Equal(t, &x, domain.NMXCClusterID)
			// Missing observations clear the cluster but not the rack-group domain.
			_, err = mirrorObservedNVLinkDomainMemberships(ctx, pool, nil, racks, groups)
			require.NoError(t, err)
			domain, err = domain.Get(ctx, pool.DB)
			require.NoError(t, err)
			assert.Nil(t, domain.NMXCClusterID)
			_, err = pool.DB.NewDelete().Model(domain).Where("id = ?", domain.ID).Exec(ctx)
			require.NoError(t, err)
			result, err = mirrorObservedNVLinkDomainMemberships(ctx, pool, nil, racks, groups)
			require.NoError(t, err)
			assert.Equal(t, 1, result.domainsResurrected)
			restored, err := domain.Get(ctx, pool.DB)
			require.NoError(t, err)
			assert.Equal(t, domain.ID, restored.ID)
			assert.Equal(t, "operator-name", restored.Name)
		})
	}
}
