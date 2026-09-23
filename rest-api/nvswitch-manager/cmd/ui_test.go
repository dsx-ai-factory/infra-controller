// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"bytes"
	"html/template"
	"regexp"
	"strings"
	"testing"

	pb "github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/internal/proto/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestSwitchesTableEndpointCells(t *testing.T) {
	tmpl, err := template.New("").Funcs(template.FuncMap{
		"formatEndpoint": formatEndpoint,
	}).ParseFS(uiTemplates, "ui_templates/switches.html")
	require.NoError(t, err)
	endpointCells := regexp.MustCompile(`(?s)<td class="mono">\s*(.*?)\s*</td>`)

	tests := []struct {
		name     string
		bmc      *pb.BMCInfo
		nvos     *pb.NVOSInfo
		wantBMC  string
		wantNVOS string
	}{
		{
			name:     "IPv6 with ports",
			bmc:      &pb.BMCInfo{IpAddress: "2001:db8::1", Port: 443},
			nvos:     &pb.NVOSInfo{IpAddress: "2001:db8::2", Port: 22},
			wantBMC:  "[2001:db8::1]:443",
			wantNVOS: "[2001:db8::2]:22",
		},
		{
			name:     "IPv4 with ports",
			bmc:      &pb.BMCInfo{IpAddress: "192.0.2.1", Port: 8443},
			nvos:     &pb.NVOSInfo{IpAddress: "192.0.2.2", Port: 2222},
			wantBMC:  "192.0.2.1:8443",
			wantNVOS: "192.0.2.2:2222",
		},
		{
			name:     "addresses without ports",
			bmc:      &pb.BMCInfo{IpAddress: "2001:db8::1"},
			nvos:     &pb.NVOSInfo{IpAddress: "192.0.2.2"},
			wantBMC:  "2001:db8::1",
			wantNVOS: "192.0.2.2",
		},
		{
			name:     "absent endpoints",
			wantBMC:  "-",
			wantNVOS: "-",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data := map[string]any{
				"Switches": []SwitchWithStatus{{
					Switch:       &pb.NVSwitchTray{Uuid: "switch-1", Bmc: tt.bmc, Nvos: tt.nvos},
					UpdateStatus: &UpdateStatusSummary{},
				}},
			}
			var rendered bytes.Buffer
			err := tmpl.ExecuteTemplate(&rendered, "switches.html", data)
			require.NoError(t, err)
			assert.Contains(t, rendered.String(), ">BMC IP</th>")
			assert.Contains(t, rendered.String(), ">NVOS IP</th>")
			cells := endpointCells.FindAllStringSubmatch(rendered.String(), -1)
			require.Len(t, cells, 2)
			assert.Equal(t, tt.wantBMC, strings.TrimSpace(cells[0][1]))
			assert.Equal(t, tt.wantNVOS, strings.TrimSpace(cells[1][1]))
		})
	}
}
