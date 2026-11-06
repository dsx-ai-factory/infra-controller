// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventory

import (
	"context"
	"fmt"
	"net/netip"
	"os"
	"reflect"
	"strings"
	"testing"

	"go.uber.org/goleak"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

var bg = context.Background()

func TestMain(m *testing.M) {
	code := m.Run()
	if err := goleak.Find(); err != nil && code == 0 {
		fmt.Println(err)
		code = 1
	}
	os.Exit(code)
}

func is(t *testing.T, got, want any, what string) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%s:\n got %#v\nwant %#v", what, got, want)
	}
}

type fakeLister struct {
	pods []cluster.PodInfo
	err  error
}

func (f fakeLister) ListPods(context.Context, string, string) ([]cluster.PodInfo, error) {
	return f.pods, f.err
}

// fakeReader answers by URL.
type fakeReader map[string]struct {
	resp Response
	err  error
}

func (f fakeReader) Read(_ context.Context, url string) (Response, error) {
	r := f[url]
	return r.resp, r.err
}

func readyPod(name, ip string) cluster.PodInfo {
	return cluster.PodInfo{Name: name, IP: ip, Ready: true, Phase: "Running"}
}

// host makes a host device record. An empty rack means the device names none.
func host(mac, hw, rack string, nics ...string) Device {
	d := Device{Kind: KindMachine, HardwareType: hw, BmcMac: mac}
	if rack != "" {
		d.RackID = &rack
	}
	for _, n := range nics {
		d.Interfaces = append(d.Interfaces, Nic{MacAddress: n})
	}
	return d
}

func TestURLRules(t *testing.T) {
	t.Run("records url brackets ipv6", func(t *testing.T) {
		is(t, RecordsURL(netip.MustParseAddr("10.0.0.5"), 1266),
			"https://10.0.0.5:1266/expected-inventory/records", "v4")
		is(t, RecordsURL(netip.MustParseAddr("fd00::1"), 1266),
			"https://[fd00::1]:1266/expected-inventory/records", "v6")
	})
	t.Run("api url rules, reason never repeats the url", func(t *testing.T) {
		for _, tc := range []struct{ raw, want string }{
			{"https://nico.example:443", ""},
			{"https://nico.example", ""},
			{"https://nico.example/", ""},
			{"://bad", "the nico-api URL does not parse"},
			{"http://nico.example", "the nico-api URL is not https"},
			{"https://user:pw@nico.example", "the nico-api URL carries credentials"},
			{"https:///path", "the nico-api URL has no host"},
			{"https://nico.example?q=1", "the nico-api URL has a query"},
			{"https://nico.example#frag", "the nico-api URL has a fragment"},
			{"https://nico.example/v2", "the nico-api URL has a path"},
			{"https://nico.example:0", "the nico-api URL has an invalid port"},
			{"https://nico.example:99999", "the nico-api URL has an invalid port"},
		} {
			is(t, CheckedAPIURL(tc.raw), tc.want, tc.raw)
		}
		// A planted marker in the scheme, port or path never reaches the reason.
		for _, raw := range []string{"ftpMARKER://h", "https://h:MARKER", "https://h/MARKER"} {
			if r := CheckedAPIURL(raw); strings.Contains(r, "MARKER") {
				t.Errorf("%q leaked into %q", raw, r)
			}
		}
	})
}

func TestRead(t *testing.T) {
	t.Run("a list error and no pods are problems", func(t *testing.T) {
		inv := Read(bg, fakeLister{err: cluster.Error{Kind: cluster.Forbidden, Text: "Forbidden"}}, "ns", fakeReader{}, "app=mat", 1266)
		is(t, inv.Problems, []string{"listing MAT pods app=mat in ns: forbidden: Forbidden"}, "list error")
		inv = Read(bg, fakeLister{}, "ns", fakeReader{}, "app=mat", 1266)
		is(t, inv.Problems, []string{"no MAT pod matches app=mat in ns"}, "no pods")
	})

	t.Run("a pod that is not ready, has no IP, or fails to read is a problem", func(t *testing.T) {
		pods := []cluster.PodInfo{
			{Name: "down", Ready: false, Phase: "Pending"},
			{Name: "noip", Ready: true},
			{Name: "badip", Ready: true, IP: "nope"},
			readyPod("read", "10.0.0.9"),
		}
		reader := fakeReader{RecordsURL(netip.MustParseAddr("10.0.0.9"), 1266): {err: fmt.Errorf("timed out")}}
		inv := Read(bg, fakeLister{pods: pods}, "ns", reader, "app=mat", 1266)
		is(t, inv.Problems, []string{
			"pod down: not Ready (phase Pending)",
			"pod noip: has no IP",
			"pod badip: IP \"nope\" is not an IP address",
			"pod read: reading its records: timed out",
		}, "per-pod problems")
	})

	t.Run("inventory sums pods and counts bmcs by kind", func(t *testing.T) {
		pod1 := Response{
			CarbideAPIURL: "https://nico",
			Racks:         []RackIntent{{RackID: "r1"}},
			Devices:       []Device{host("aa", "gb200", "r1"), {Kind: KindSwitch, HardwareType: "n5700", BmcMac: "bb"}},
			DPUs:          []DPU{{BmcMac: "dd", ParentBmcMac: "aa"}},
		}
		pod2 := Response{
			CarbideAPIURL: "https://nico",
			Racks:         []RackIntent{{RackID: "r2"}},
			Devices:       []Device{host("cc", "gb200", "r2"), {Kind: KindPowerShelf, HardwareType: "delta", BmcMac: "ee"}},
		}
		pods := []cluster.PodInfo{readyPod("p1", "10.0.0.1"), readyPod("p2", "10.0.0.2")}
		reader := fakeReader{
			RecordsURL(netip.MustParseAddr("10.0.0.1"), 1266): {resp: pod1},
			RecordsURL(netip.MustParseAddr("10.0.0.2"), 1266): {resp: pod2},
		}
		inv := Read(bg, fakeLister{pods: pods}, "ns", reader, "app=mat", 1266)
		is(t, inv.OK(), true, "no problem")
		is(t, inv.N, TypeCounts{RackGroup: 2, Rack: 2, Host: 2, DPU: 1, Switch: 1, PowerShelf: 1}, "counts")
		is(t, inv.BmcCounts, map[string]uint64{"host/gb200": 2, "host/n5700": 1, "host/delta": 1, "dpu": 1}, "bmc counts")
		url, ok := inv.CarbideAPIURL()
		is(t, []any{url, ok}, []any{"https://nico", true}, "pods agree on the url")
	})
}

func TestProblemsAreNamed(t *testing.T) {
	p1 := MatPod{Name: "p1", Records: Response{
		RegisterExpectedMachines: true,
		CarbideAPIURL:            "https://user:pw@nico",
		Racks:                    []RackIntent{{RackID: "r1"}},
		Devices: []Device{
			host("AA", "gb200", "r1", "11:22"),
			{Kind: KindSwitch, HardwareType: "n5700", BmcMac: "bb", NvosMacs: []string{"11:22"}},
			host("ff", "gb200", "missing"),
		},
		DPUs: []DPU{{BmcMac: "dd", ParentBmcMac: "zz"}},
	}}
	p2 := MatPod{Name: "p2", Records: Response{
		CarbideAPIURL: "https://other",
		Racks:         []RackIntent{{RackID: "r1"}},
		Devices:       []Device{host("aa", "gb200", "r1")},
	}}
	want := []string{
		"pod p1 registers its own expected records (register_expected_machines is true)",
		"pod p1: the nico-api URL carries credentials",
		"pods disagree on carbide_api_url, by URL: [p1], [p2]",
		"BMC MAC aa is served 2 times, by p1, p2",
		"rack r1 is served 2 times, by p1, p2",
		"pod p1 DPU dd names parent zz, which is no served host",
		"MAC 11:22 is served by 2 devices: p1 device AA, p1 device bb",
		"pod p1 device ff names rack missing, which no pod serves",
	}
	is(t, checkPods([]MatPod{p1, p2}), want, "problems")
}

func TestNewPlan(t *testing.T) {
	t.Run("keys, order and unit keys", func(t *testing.T) {
		inv := SiteInventory{Pods: []MatPod{{Records: Response{
			Racks: []RackIntent{{RackID: "r1"}},
			Devices: []Device{
				host("AA", "gb200", "r1"),
				{Kind: KindSwitch, BmcMac: "BB"},
				{Kind: KindPowerShelf, BmcMac: "CC"},
			},
		}}}}
		plan, err := NewPlan(&inv)
		is(t, err, nil, "no error")
		is(t, plan.Racks, []RackWrites{{RackID: "r1", Group: "rackGroup/r1", Rack: "rack/r1"}}, "racks")
		is(t, plan.Devices[Host], []DeviceWrite{{RackID: "r1", Key: "host/aa"}}, "host")
		is(t, plan.Devices[Switch], []DeviceWrite{{Key: "switch/bb"}}, "switch")
		is(t, plan.Devices[PowerShelf], []DeviceWrite{{Key: "powerShelf/cc"}}, "power shelf")
		is(t, plan.UnitKeys(), []WriteKey{"rack/r1", "host/aa", "switch/bb", "powerShelf/cc"}, "unit keys")
	})
	t.Run("a dpu device, an unknown kind and a duplicate key are errors", func(t *testing.T) {
		for _, tc := range []struct {
			name string
			devs []Device
			want string
		}{
			{"dpu", []Device{{Kind: KindDPU, BmcMac: "dd"}}, "device dd is a DPU"},
			{"unknown", []Device{{Kind: "gpu", BmcMac: "gg"}}, "device gg has an unknown kind"},
			{"dup", []Device{host("aa", "", ""), host("AA", "", "")}, "write host/aa is planned twice"},
		} {
			_, err := NewPlan(&SiteInventory{Pods: []MatPod{{Records: Response{Devices: tc.devs}}}})
			is(t, fmt.Sprint(err), tc.want, tc.name)
		}
	})
	t.Run("a rack planned twice is an error", func(t *testing.T) {
		inv := SiteInventory{Pods: []MatPod{
			{Records: Response{Racks: []RackIntent{{RackID: "r1"}}}},
			{Records: Response{Racks: []RackIntent{{RackID: "r1"}}}},
		}}
		_, err := NewPlan(&inv)
		is(t, fmt.Sprint(err), "write rackGroup/r1 is planned twice", "dup rack")
	})
}
