// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventorysync

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

type failSwitchNvosPortsClient struct{ nicoapi.Client }

func (c *failSwitchNvosPortsClient) FindSwitchNvosPorts(context.Context, []string) (map[string][]nicoapi.SwitchNvosPort, error) {
	return nil, errors.New("injected NVOS lookup failure")
}

func TestSyncSwitchNvosPorts(t *testing.T) {
	const portsJSON = `[
		{"mac":"00:11:22:33:44:55","servicePort":22,"addresses":[
			{"addressFamily":"ADDRESS_FAMILY_V4","address":"192.0.2.20"},
			{"addressFamily":"ADDRESS_FAMILY_V6","address":"2001:db8::20"}
		],"isPrimary":true},
		{"mac":"00:11:22:33:44:56","servicePort":null,"addresses":[],"isPrimary":false}
	]`
	tests := []struct {
		name            string
		before          string
		ports           string
		fetchError      bool
		concurrentWrite bool
		want            string
	}{
		{
			name:   "initialize description with full ports",
			before: `null`,
			ports:  portsJSON,
			want:   `{"nvos_ports":` + portsJSON + `}`,
		},
		{
			name:            "migrate legacy key and preserve concurrent metadata",
			before:          `{"nvos_ip":"192.0.2.1","operator":"keep"}`,
			ports:           portsJSON,
			concurrentWrite: true,
			want:            `{"nvos_ports":` + portsJSON + `,"operator":"updated"}`,
		},
		{
			name:   "empty snapshot removes stale ports and scalar IP",
			before: `{"nvos_ip":"192.0.2.1","nvos_ports":` + portsJSON + `,"operator":"keep"}`,
			ports:  `[]`,
			want:   `{"nvos_ports":[],"operator":"keep"}`,
		},
		{
			name:   "missing NVOS information preserves stored ports",
			before: `{"nvos_ports":` + portsJSON + `,"operator":"keep"}`,
			want:   `{"nvos_ports":` + portsJSON + `,"operator":"keep"}`,
		},
		{
			name:       "failed fetch preserves legacy metadata",
			before:     `{"nvos_ip":"192.0.2.1","operator":"keep"}`,
			fetchError: true,
			want:       `{"nvos_ip":"192.0.2.1","operator":"keep"}`,
		},
	}

	ctx, pool := mirrorTestPool(t)
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			component := model.Component{
				Name:         "before",
				SerialNumber: tt.name,
				Manufacturer: "TestMfg",
				Type:         devicetypes.ComponentTypeToString(devicetypes.ComponentTypeNVSwitch),
			}
			require.NoError(t, json.Unmarshal([]byte(tt.before), &component.Description))
			require.NoError(t, component.Create(ctx, pool.DB))

			const switchID = "core-switch-id"
			var client nicoapi.Client = nicoapi.NewMockClient()
			if tt.ports != "" {
				var ports []nicoapi.SwitchNvosPort
				require.NoError(t, json.Unmarshal([]byte(tt.ports), &ports))
				client.SetSwitchNvosPorts(switchID, ports)
			}
			if tt.fetchError {
				client = &failSwitchNvosPortsClient{Client: client}
			}

			wantName := component.Name
			if tt.concurrentWrite {
				// Write after the sync's component snapshot was read. Updating
				// NVOS metadata must preserve these newer, unrelated values.
				_, err := pool.DB.NewUpdate().Model((*model.Component)(nil)).
					Set("name = ?", "after").
					Set("description = jsonb_set(description, '{operator}', '\"updated\"'::jsonb)").
					Where("id = ?", component.ID).Exec(ctx)
				require.NoError(t, err)
				wantName = "after"
			}

			components := map[string]*model.Component{switchID: &component}
			syncSwitchNvosPorts(ctx, pool, client, components)

			var stored model.Component
			require.NoError(t, pool.DB.NewSelect().Model(&stored).Where("id = ?", component.ID).Scan(ctx))
			data, err := json.Marshal(stored.Description)
			require.NoError(t, err)
			assert.JSONEq(t, tt.want, string(data))
			assert.Equal(t, wantName, stored.Name)

			// JSONB numbers decode into json.Number. A repeated sync must
			// still recognize equal port data and avoid rewriting the row.
			var beforeVersion, afterVersion string
			require.NoError(t, pool.DB.NewSelect().Table("component").ColumnExpr("xmin::text").Where("id = ?", component.ID).Scan(ctx, &beforeVersion))
			syncSwitchNvosPorts(ctx, pool, client, map[string]*model.Component{switchID: &stored})
			require.NoError(t, pool.DB.NewSelect().Table("component").ColumnExpr("xmin::text").Where("id = ?", component.ID).Scan(ctx, &afterVersion))
			assert.Equal(t, beforeVersion, afterVersion, "unchanged inventory must not rewrite the component")
		})
	}
}
