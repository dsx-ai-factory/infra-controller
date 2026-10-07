// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
)

func TestReadRackJSONData(t *testing.T) {
	testCases := map[string]struct {
		file        string
		jsonStr     string
		expectError bool
		errContains string
		expectBytes []byte
	}{
		"inline json string": {
			jsonStr:     `{"info":{"name":"R1"}}`,
			expectBytes: []byte(`{"info":{"name":"R1"}}`),
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got, err := readRackJSONData(tc.file, tc.jsonStr)
			if tc.expectError {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
			} else {
				require.NoError(t, err)
				assert.Equal(t, tc.expectBytes, got)
			}
		})
	}
}

func TestReadRackJSONDataFromFile(t *testing.T) {
	content := []byte(`{"info":{"name":"R1"}}`)
	dir := t.TempDir()
	path := filepath.Join(dir, "rack.json")
	require.NoError(t, os.WriteFile(path, content, 0600))

	got, err := readRackJSONData(path, "")
	require.NoError(t, err)
	assert.Equal(t, content, got)
}

func TestReadRackJSONDataFileNotFound(t *testing.T) {
	_, err := readRackJSONData("/nonexistent/path/rack.json", "")
	require.Error(t, err)
}

func TestParseRackJSON(t *testing.T) {
	validUUID := "a1b2c3d4-0001-4000-8000-000000000001"
	compUUID := "a1b2c3d4-0002-4000-8000-000000000001"
	domainUUID := "a1b2c3d4-0003-4000-8000-000000000001"

	testCases := map[string]struct {
		json        string
		expectError bool
		errContains string
		validate    func(t *testing.T, rack *types.Rack)
	}{
		"invalid json": {
			json:        `not json`,
			expectError: true,
			errContains: "failed to parse JSON",
		},
		"minimal rack without id generates uuid": {
			json: `{
				"info": {"name": "R1", "manufacturer": "NVIDIA",
				         "serial_number": "SN001"},
				"location": {"region": "us-west", "datacenter": "DC1"}
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				assert.Equal(t, "R1", rack.Info.Name)
				assert.Equal(t, "NVIDIA", rack.Info.Manufacturer)
				assert.Equal(t, "SN001", rack.Info.SerialNumber)
				assert.Equal(t, "us-west", rack.Location.Region)
				assert.Equal(t, "DC1", rack.Location.Datacenter)
				assert.NotEqual(
					t, [16]byte{}, [16]byte(rack.Info.ID),
					"UUID should be generated",
				)
				assert.Empty(t, rack.Components)
			},
		},
		"rack with explicit uuid": {
			json: `{"info": {"id": "` + validUUID + `", "name": "R1"}}`,
			validate: func(t *testing.T, rack *types.Rack) {
				assert.Equal(t, validUUID, rack.Info.ID.String())
			},
		},
		"rack with invalid uuid": {
			json:        `{"info": {"id": "not-a-uuid"}}`,
			expectError: true,
			errContains: "invalid rack UUID",
		},
		"rack with NVLink domain IDs": {
			json: `{
				"info": {"name": "R1"},
				"nvl_domain_ids": ["` + domainUUID + `"]
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				require.Len(t, rack.NVLDomainIDs, 1)
				assert.Equal(t, domainUUID, rack.NVLDomainIDs[0].String())
			},
		},
		"rack with invalid NVLink domain UUID": {
			json:        `{"nvl_domain_ids":["not-a-uuid"]}`,
			expectError: true,
			errContains: "invalid NVLink domain UUID",
		},
		"rack with all location fields": {
			json: `{
				"info": {},
				"location": {
					"region": "us-east",
					"datacenter": "SJC01",
					"room": "Room-A",
					"position": "Row-1"
				}
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				assert.Equal(t, "us-east", rack.Location.Region)
				assert.Equal(t, "SJC01", rack.Location.Datacenter)
				assert.Equal(t, "Room-A", rack.Location.Room)
				assert.Equal(t, "Row-1", rack.Location.Position)
			},
		},
		"rack with compute component": {
			json: `{
				"info": {"name": "R1"},
				"components": [{
					"type": "compute",
					"info": {
						"id": "` + compUUID + `",
						"name": "gpu1",
						"manufacturer": "NVIDIA",
						"model": "GB200",
						"serial_number": "SN-GPU-001"
					},
					"firmware_version": "1.2.3",
					"component_id": "ext-id-001",
					"position": {"slot_id": 11, "tray_index": 0, "host_id": 1}
				}]
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				require.Len(t, rack.Components, 1)
				c := rack.Components[0]
				assert.Equal(t, types.ComponentTypeCompute, c.Type)
				assert.Equal(t, compUUID, c.Info.ID.String())
				assert.Equal(t, "gpu1", c.Info.Name)
				assert.Equal(t, "NVIDIA", c.Info.Manufacturer)
				assert.Equal(t, "GB200", c.Info.Model)
				assert.Equal(t, "SN-GPU-001", c.Info.SerialNumber)
				assert.Equal(t, "1.2.3", c.FirmwareVersion)
				assert.Equal(t, "ext-id-001", c.ComponentID)
				assert.Equal(t, 11, c.Position.SlotID)
				assert.Equal(t, 0, c.Position.TrayIndex)
				assert.Equal(t, 1, c.Position.HostID)
			},
		},
		"component without id generates uuid": {
			json: `{
				"info": {},
				"components": [{"type": "nvswitch", "info": {"name": "sw1"}}]
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				require.Len(t, rack.Components, 1)
				assert.NotEqual(
					t, [16]byte{}, [16]byte(rack.Components[0].Info.ID),
				)
			},
		},
		"component with invalid type": {
			json: `{
				"info": {},
				"components": [{"type": "bogus", "info": {}}]
			}`,
			expectError: true,
			errContains: "invalid component type",
		},
		"component with invalid uuid": {
			json: `{
				"info": {},
				"components": [{
					"type": "compute",
					"info": {"id": "bad-uuid"}
				}]
			}`,
			expectError: true,
			errContains: "invalid component UUID",
		},
		"component with host bmc": {
			json: `{
				"info": {},
				"components": [{
					"type": "compute",
					"info": {},
					"bmcs": [{
						"type": "host",
						"mac": "aa:bb:cc:dd:ee:ff",
						"ip": "192.168.1.1"
					}]
				}]
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				require.Len(t, rack.Components[0].BMCs, 1)
				bmc := rack.Components[0].BMCs[0]
				assert.Equal(t, types.BMCTypeHost, bmc.Type)
				assert.Equal(
					t, "aa:bb:cc:dd:ee:ff", bmc.MAC.String(),
				)
				assert.Equal(
					t, "192.168.1.1", bmc.IP.String(),
				)
			},
		},
		"component with dpu bmc and no ip": {
			json: `{
				"info": {},
				"components": [{
					"type": "compute",
					"info": {},
					"bmcs": [{"type": "dpu", "mac": "11:22:33:44:55:66"}]
				}]
			}`,
			validate: func(t *testing.T, rack *types.Rack) {
				bmc := rack.Components[0].BMCs[0]
				assert.Equal(t, types.BMCTypeDPU, bmc.Type)
				assert.Nil(t, bmc.IP)
			},
		},
		"bmc with invalid mac": {
			json: `{
				"info": {},
				"components": [{
					"type": "compute",
					"info": {},
					"bmcs": [{"type": "host", "mac": "not-a-mac"}]
				}]
			}`,
			expectError: true,
			errContains: "invalid BMC MAC address",
		},
		"bmc with invalid ip": {
			json: `{
				"info": {},
				"components": [{
					"type": "compute",
					"info": {},
					"bmcs": [{"type": "host", "ip": "999.999.999.999"}]
				}]
			}`,
			expectError: true,
			errContains: "invalid BMC IP address",
		},
		"bmc with invalid type": {
			json: `{
				"info": {},
				"components": [{
					"type": "compute",
					"info": {},
					"bmcs": [{"type": "unknown-type"}]
				}]
			}`,
			expectError: true,
			errContains: "invalid BMC type",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			rack, err := parseRackJSON([]byte(tc.json))
			if tc.expectError {
				require.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
			} else {
				require.NoError(t, err)
				require.NotNil(t, rack)
				if tc.validate != nil {
					tc.validate(t, rack)
				}
			}
		})
	}
}

func TestParseBMCTypeToTypes(t *testing.T) {
	testCases := map[string]struct {
		input    string
		expected types.BMCType
	}{
		"host lowercase":  {input: "host", expected: types.BMCTypeHost},
		"host uppercase":  {input: "HOST", expected: types.BMCTypeHost},
		"host mixed case": {input: "Host", expected: types.BMCTypeHost},
		"empty string":    {input: "", expected: types.BMCTypeHost},
		"dpu lowercase":   {input: "dpu", expected: types.BMCTypeDPU},
		"dpu uppercase":   {input: "DPU", expected: types.BMCTypeDPU},
		"invalid type":    {input: "bmc", expected: types.BMCTypeUnknown},
		"unknown keyword": {input: "unknown", expected: types.BMCTypeUnknown},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			got := parseBMCTypeToTypes(tc.input)
			assert.Equal(t, tc.expected, got)
		})
	}
}
