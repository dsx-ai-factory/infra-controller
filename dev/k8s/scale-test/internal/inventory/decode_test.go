// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventory

import (
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// completeRecord is one MAT pod's records with every required field present.
const completeRecord = `{
  "inventoryId": "inv-1",
  "registerExpectedMachines": false,
  "carbideApiUrl": "https://nico",
  "racks": [{"rackId":"r1","rackProfileId":"p1","group":{"rackGroupId":"g1","topology":"t","protocol":"pr","members":[{"type":"compute","manufacturer":"nv","id":"m1"}]}}],
  "devices": [{"kind":"machine","hardwareType":"gb200","bmcMac":"AA","serial":"s1","rackId":"r1","dpuPolicy":"nic","dpfEnabled":true,"interfaces":[{"macAddress":"11:22","primary":true}],"nvosMacs":[]}],
  "dpus": [{"bmcMac":"dd","serial":"s2","parentBmcMac":"AA"}]
}`

func ptr[T any](v T) *T { return &v }

func TestDecodeRecords(t *testing.T) {
	var full Response
	if err := jsonx.Decode([]byte(completeRecord), &full); err != nil {
		t.Fatalf("the complete record did not decode: %v", err)
	}
	is(t, full.Devices[0].DpuPolicy, ptr(DpuNic), "dpuPolicy nic preserved")
	is(t, full.Devices[0].RackID, ptr("r1"), "rackId preserved")
	is(t, full.Devices[0].Interfaces, []Nic{{MacAddress: "11:22", Primary: true}}, "interfaces")

	t.Run("optional fields: null, nic and ignore", func(t *testing.T) {
		device := func(frag string) string {
			return `{"inventoryId":"i","registerExpectedMachines":false,"carbideApiUrl":"https://n","racks":[],` +
				`"devices":[{"kind":"machine","hardwareType":"h","bmcMac":"a","serial":"s","dpfEnabled":false,"interfaces":[],"nvosMacs":[]` + frag + `}],"dpus":[]}`
		}
		for _, tc := range []struct {
			name, frag string
			rack       *string
			policy     *DpuPolicy
		}{
			{"absent", "", nil, nil},
			{"null", `,"rackId":null,"dpuPolicy":null`, nil, nil},
			{"nic", `,"rackId":"r1","dpuPolicy":"nic"`, ptr("r1"), ptr(DpuNic)},
			{"ignore", `,"dpuPolicy":"ignore"`, nil, ptr(DpuIgnore)},
		} {
			var r Response
			err := jsonx.Decode([]byte(device(tc.frag)), &r)
			is(t, err, nil, tc.name+" decodes")
			is(t, []any{r.Devices[0].RackID, r.Devices[0].DpuPolicy}, []any{tc.rack, tc.policy}, tc.name)
		}
	})

	t.Run("required fields, duplicates, case and enums are refused", func(t *testing.T) {
		base := `"inventoryId":"i","carbideApiUrl":"https://n","racks":[],"devices":[],"dpus":[]`
		dev := func(d string) string {
			return `{"inventoryId":"i","registerExpectedMachines":false,"carbideApiUrl":"https://n","racks":[],"devices":[` + d + `],"dpus":[]}`
		}
		host := `"kind":"machine","hardwareType":"h","bmcMac":"a","serial":"s","dpfEnabled":false,"interfaces":[],"nvosMacs":[]`
		for _, tc := range []struct{ name, in string }{
			{"missing registration", `{` + base + `}`},
			{"null registration", `{"registerExpectedMachines":null,` + base + `}`},
			{"missing array", `{"registerExpectedMachines":false,"inventoryId":"i","carbideApiUrl":"https://n","devices":[],"dpus":[]}`},
			{"null array", `{"registerExpectedMachines":false,"inventoryId":"i","carbideApiUrl":"https://n","racks":null,"devices":[],"dpus":[]}`},
			{"duplicate registration", `{"registerExpectedMachines":false,"registerExpectedMachines":true,` + base + `}`},
			{"wrong-case key only", `{"RegisterExpectedMachines":false,` + base + `}`},
			{"unknown kind", dev(`{"kind":"gpu","hardwareType":"h","bmcMac":"a","serial":"s","dpfEnabled":false,"interfaces":[],"nvosMacs":[]}`)},
			{"unknown dpuPolicy", dev(`{` + host + `,"dpuPolicy":"bogus"}`)},
		} {
			var r Response
			err := jsonx.Decode([]byte(tc.in), &r)
			if err == nil {
				t.Errorf("%s: want an error, got none", tc.name)
			}
		}
	})

	t.Run("unknown fields are allowed and the canonical field wins", func(t *testing.T) {
		in := `{"registerExpectedMachines":true,"RegisterExpectedMachines":false,"extra":{"x":1},` +
			`"inventoryId":"i","carbideApiUrl":"https://n","racks":[],"devices":[],"dpus":[]}`
		var r Response
		is(t, jsonx.Decode([]byte(in), &r), nil, "decodes with unknown fields")
		is(t, r.RegisterExpectedMachines, true, "canonical registration wins")
	})

	t.Run("a decode error holds no input", func(t *testing.T) {
		var r Response
		err := jsonx.Decode([]byte(`{"registerExpectedMachines":"SECRETVALUE","inventoryId":"i","carbideApiUrl":"https://n","racks":[],"devices":[],"dpus":[]}`), &r)
		if err == nil || strings.Contains(err.Error(), "SECRETVALUE") {
			t.Errorf("decode error leaked input or was nil: %v", err)
		}
	})
}
