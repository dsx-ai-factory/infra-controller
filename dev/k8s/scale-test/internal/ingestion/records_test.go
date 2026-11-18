// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"reflect"
	"strings"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
)

func is(t *testing.T, got, want any, what string) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%s:\n got %#v\nwant %#v", what, got, want)
	}
}

const profileID = "GB200_NVL72R1_C2G4_WIWYNN_NO_POWERSHELF"

// rackIntent is a rack declaring its own group, with the profile nico-api derives from that group.
func rackIntent(id string) inventory.RackIntent {
	return inventory.RackIntent{RackID: id, RackProfileID: profileID, Group: inventory.RackGroup{RackGroupID: "g-" + id, Topology: "gb200_nvl72r1_c2g4",
		Protocol: "NVLINK_V5", Members: []inventory.Member{{Type: "Compute", Manufacturer: "WIWYNN", ID: id + "-c1"}, {Type: "Switch", Manufacturer: "NVIDIA", ID: id + "-s1"}}}}
}

func inv(racks []inventory.RackIntent, devices ...inventory.Device) *inventory.SiteInventory {
	return &inventory.SiteInventory{Pods: []inventory.MatPod{{Records: inventory.Response{Racks: racks, Devices: devices}}}}
}

// fixture is two racks, a host on r1 with NICs and a DPU policy, a host with no rack, a switch on r2 with two NVOS MACs and a power shelf on r1.
func fixture() *inventory.SiteInventory {
	r1, r2, manage := "r1", "r2", inventory.DpuManage
	return inv([]inventory.RackIntent{rackIntent("r1"), rackIntent("r2")},
		inventory.Device{Kind: inventory.KindMachine, BmcMac: "AA:00", Serial: "m001", RackID: &r1, DpuPolicy: &manage, DpfEnabled: true,
			Interfaces: []inventory.Nic{{MacAddress: "a1", Primary: true}, {MacAddress: "a2"}}},
		inventory.Device{Kind: inventory.KindMachine, BmcMac: "aa:01", Serial: "m002"},
		inventory.Device{Kind: inventory.KindSwitch, BmcMac: "bb:00", Serial: "s1", RackID: &r2, NvosMacs: []string{"n1", "n2"}},
		inventory.Device{Kind: inventory.KindPowerShelf, BmcMac: "cc:00", Serial: "p1", RackID: &r1})
}

func TestNewPlan(t *testing.T) {
	t.Run("keys, order and unit keys", func(t *testing.T) {
		r1 := "r1"
		plan, err := NewPlan(inv([]inventory.RackIntent{{RackID: "r1"}},
			inventory.Device{Kind: inventory.KindMachine, BmcMac: "AA", RackID: &r1},
			inventory.Device{Kind: inventory.KindSwitch, BmcMac: "BB"},
			inventory.Device{Kind: inventory.KindPowerShelf, BmcMac: "CC"}))
		is(t, err, nil, "no error")
		is(t, plan.Racks, []RackWrites{{RackID: "r1", Group: "rackGroup/r1", Rack: "rack/r1"}}, "racks")
		is(t, plan.Devices[Host], []DeviceWrite{{RackID: "r1", Key: "host/aa"}}, "host")
		is(t, plan.Devices[Switch], []DeviceWrite{{Key: "switch/bb"}}, "switch")
		is(t, plan.Devices[PowerShelf], []DeviceWrite{{Key: "powerShelf/cc"}}, "power shelf")
		is(t, plan.UnitKeys(), []WriteKey{"rack/r1", "host/aa", "switch/bb", "powerShelf/cc"}, "unit keys")
	})
	t.Run("a dpu device, an unknown kind, a duplicate key and a rack planned twice are errors naming indices, never a value", func(t *testing.T) {
		twice := &inventory.SiteInventory{Pods: []inventory.MatPod{
			{Records: inventory.Response{Racks: []inventory.RackIntent{{RackID: secret}}}},
			{Records: inventory.Response{Racks: []inventory.RackIntent{{RackID: secret}}}},
		}}
		sameGroup := inv([]inventory.RackIntent{{RackID: "r1", Group: inventory.RackGroup{RackGroupID: secret}}, {RackID: "r2", Group: inventory.RackGroup{RackGroupID: secret}}})
		crossPod := &inventory.SiteInventory{Pods: []inventory.MatPod{
			{Records: inventory.Response{Racks: []inventory.RackIntent{{RackID: "r1", Group: inventory.RackGroup{RackGroupID: secret}}}}},
			{Records: inventory.Response{Racks: []inventory.RackIntent{{RackID: "r2", Group: inventory.RackGroup{RackGroupID: secret}}}}},
		}}
		for _, tc := range []struct {
			name string
			inv  *inventory.SiteInventory
			want string
		}{
			{"dpu", inv(nil, inventory.Device{Kind: inventory.KindDPU, BmcMac: secret}), "a DPU device has no expected record (pod 0, device 0)"},
			{"unknown", inv(nil, inventory.Device{Kind: "gpu", BmcMac: secret}), "a device has an unknown kind (pod 0, device 0)"},
			{"dup", inv(nil, inventory.Device{Kind: inventory.KindMachine, BmcMac: "aa"}, inventory.Device{Kind: inventory.KindMachine, BmcMac: secret},
				inventory.Device{Kind: inventory.KindMachine, BmcMac: strings.ToUpper(secret)}), "two devices map to the same write key (pod 0, device 2)"},
			{"group twice, across pods", crossPod, "two racks name the same group ID (pod 1, rack 0)"},
			{"group twice", sameGroup, "two racks name the same group ID (pod 0, rack 1)"},
			{"rack twice", twice, "two racks map to the same write key (pod 1, rack 0)"},
		} {
			_, err := NewPlan(tc.inv)
			is(t, fmt.Sprint(err), tc.want, tc.name)
		}
	})
}

func TestRecordsConvertTheServedInventory(t *testing.T) {
	plan, recs, err := Records(fixture())
	is(t, err, nil, "no error")
	is(t, plan.Devices[Host], []DeviceWrite{{RackID: "r1", Key: "host/aa:00"}, {Key: "host/aa:01"}}, "hosts")
	group := func(id string) *corev1.ExpectedRackGroup {
		return &corev1.ExpectedRackGroup{RackGroupId: &corev1.RackGroupId{Id: "g-" + id}, Topology: "gb200_nvl72r1_c2g4", Protocol: "NVLINK_V5",
			Racks: []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: id}, Members: []*corev1.ExpectedRackGroupMember{
				{Type: "Compute", Manufacturer: "WIWYNN", Id: id + "-c1"}, {Type: "Switch", Manufacturer: "NVIDIA", Id: id + "-s1"}}}}}
	}
	rack := func(id string) *corev1.ExpectedRack {
		return &corev1.ExpectedRack{RackId: &corev1.RackId{Id: id}, RackProfileId: &corev1.RackProfileId{Id: profileID}}
	}
	nic := func(mac string, primary bool) *corev1.ExpectedHostNic {
		return &corev1.ExpectedHostNic{MacAddress: proto.String(mac), Primary: proto.Bool(primary), NetworkSegmentType: corev1.NetworkSegmentType_HOST_INBAND.Enum()}
	}
	m1 := &corev1.ExpectedMachine{BmcMacAddress: "AA:00", BmcUsername: "root", BmcPassword: "factory_password", ChassisSerialNumber: "m001",
		HostNics: []*corev1.ExpectedHostNic{nic("a1", true), nic("a2", false)}, RackId: &corev1.RackId{Id: "r1"}, IsDpfEnabled: proto.Bool(true),
		DpuMode: corev1.DpuMode_DPU_MODE.Enum()}
	m1.DpfEnabled = true //nolint:staticcheck // the deprecated field machine-a-tron still sets
	want := map[WriteKey]Record{
		"rackGroup/r1": {Kind: "expectedRackGroup", Body: group("r1")},
		"rack/r1":      {Kind: "expectedRack", Body: rack("r1")},
		"rackGroup/r2": {Kind: "expectedRackGroup", Body: group("r2")},
		"rack/r2":      {Kind: "expectedRack", Body: rack("r2")},
		"host/aa:00":   {Kind: "expectedMachine", Body: m1},
		"host/aa:01": {Kind: "expectedMachine", Body: &corev1.ExpectedMachine{BmcMacAddress: "aa:01", BmcUsername: "root", BmcPassword: "factory_password",
			ChassisSerialNumber: "m002", IsDpfEnabled: proto.Bool(false)}},
		"switch/bb:00": {Kind: "expectedSwitch", Body: &corev1.ExpectedSwitch{BmcMacAddress: "bb:00", BmcUsername: "root", BmcPassword: "factory_password",
			SwitchSerialNumber: "s1", RackId: &corev1.RackId{Id: "r2"}, NvosUsername: proto.String("admin"), NvosPassword: proto.String("factory_password"),
			NvosInterfaces: []*corev1.ExpectedNvosInterface{{MacAddress: "n1", IsPrimary: true}, {MacAddress: "n2"}}}},
		"powerShelf/cc:00": {Kind: "expectedPowerShelf", Body: &corev1.ExpectedPowerShelf{BmcMacAddress: "cc:00", BmcUsername: "root",
			BmcPassword: "factory_password", ShelfSerialNumber: "p1", RackId: &corev1.RackId{Id: "r1"}, BmcRetainCredentials: proto.Bool(true)}},
	}
	is(t, len(recs), len(want), "one record per planned key")
	for k, w := range want {
		got, ok := recs[k]
		if !ok || got.Kind != w.Kind || !proto.Equal(got.Body, w.Body) {
			t.Errorf("%s:\n got %s %v\nwant %s %v", k, got.Kind, got.Body, w.Kind, w.Body)
		}
	}

	t.Run("each DPU policy maps to its mode, and none leaves the mode unset", func(t *testing.T) {
		for policy, mode := range map[inventory.DpuPolicy]*corev1.DpuMode{
			inventory.DpuManage: corev1.DpuMode_DPU_MODE.Enum(), inventory.DpuNic: corev1.DpuMode_NIC_MODE.Enum(), inventory.DpuIgnore: corev1.DpuMode_NO_DPU.Enum(),
			"": nil,
		} {
			d := inventory.Device{Kind: inventory.KindMachine, BmcMac: "aa", Serial: "m001"}
			if policy != "" {
				d.DpuPolicy = &policy
			}
			_, body, err := deviceBody(&d)
			is(t, []any{err, body.(*corev1.ExpectedMachine).DpuMode}, []any{nil, mode}, string(policy))
		}
	})

	t.Run("the digest is the SHA-256 of the deterministic encoding: stable per body, distinct across bodies", func(t *testing.T) {
		_, again, _ := Records(fixture())
		changed := fixture()
		changed.Pods[0].Records.Devices[0].Serial = "m001-other"
		_, other, _ := Records(changed)
		b, _ := proto.MarshalOptions{Deterministic: true}.Marshal(recs["rack/r1"].Body)
		sum := sha256.Sum256(b)
		is(t, recs["rack/r1"].Digest, hex.EncodeToString(sum[:]), "sha256 of the encoding")
		for k, r := range recs {
			is(t, again[k].Digest, r.Digest, string(k)+" stable")
			is(t, other[k].Digest == r.Digest, k != "host/aa:00", string(k)+" changes only with its body")
		}
	})

	t.Run("a topology Rust uppercases by expansion and a 128-character group ID are accepted", func(t *testing.T) {
		s := fixture()
		r := &s.Pods[0].Records.Racks[1]
		r.Group.Topology, r.RackProfileID, r.Group.RackGroupID = "straße", "STRASSE_WIWYNN_NO_POWERSHELF", strings.Repeat("é", 128)
		_, recs, err := Records(s)
		is(t, []any{err, recs != nil}, []any{nil, true}, "accepted")
	})

	t.Run("host serials of 4 and 32 characters, with every allowed character, are accepted", func(t *testing.T) {
		for _, serial := range []string{"a-_9", strings.Repeat("Z", 32)} {
			s := fixture()
			s.Pods[0].Records.Devices[1].Serial = serial
			_, recs, err := Records(s)
			is(t, []any{err, recs["host/aa:01"].Body.(*corev1.ExpectedMachine).ChassisSerialNumber}, []any{nil, serial}, serial)
		}
	})

	// nico-api would store an empty rack reference for a blank one: a nil RackID is rackless (the fixture's second host), a present blank one is refused. A host
	// serial is nico-api's CHASSIS_SERIAL_REGEX with no normalization, and an error never holds the value.
	t.Run("a blank rack ID on any device kind and a host serial outside the rule are refused, naming indices", func(t *testing.T) {
		with := func(d inventory.Device) *inventory.SiteInventory {
			s := fixture()
			s.Pods[0].Records.Devices = append(s.Pods[0].Records.Devices, d)
			return s
		}
		for _, kind := range []inventory.DeviceKind{inventory.KindMachine, inventory.KindSwitch, inventory.KindPowerShelf} {
			for _, id := range []string{"", " ", "\t"} {
				_, recs, err := Records(with(inventory.Device{Kind: kind, BmcMac: secret, Serial: "m005", RackID: &id}))
				is(t, []any{fmt.Sprint(err), recs == nil}, []any{"a device names a blank rack ID (pod 0, device 4)", true}, string(kind)+" "+id)
			}
		}
		for _, serial := range []string{"", "abc", strings.Repeat("a", 33), "ab cd", "ab.cd", "ábcd", "abcd\n", secret} {
			_, recs, err := Records(with(inventory.Device{Kind: inventory.KindMachine, BmcMac: "dd:00", Serial: serial}))
			is(t, []any{fmt.Sprint(err), recs == nil}, []any{"a host's serial is not 4 to 32 letters, digits, hyphens or underscores (pod 0, device 4)", true}, serial)
		}
	})

	t.Run("bad input is an error naming indices, never a value or a panic", func(t *testing.T) {
		// rack2 is the fixture with its second rack changed by f.
		rack2 := func(f func(r *inventory.RackIntent)) *inventory.SiteInventory {
			s := fixture()
			f(&s.Pods[0].Records.Racks[1])
			return s
		}
		bad := inventory.DpuPolicy("other")
		// withPod is the fixture plus a second pod holding the rack r3, changed by f.
		withPod := func(f func(r *inventory.RackIntent)) *inventory.SiteInventory {
			s := fixture()
			s.Pods = append(s.Pods, inventory.MatPod{Records: inventory.Response{Racks: []inventory.RackIntent{rackIntent("r3")}}})
			f(&s.Pods[1].Records.Racks[0])
			return s
		}
		const badID = "a rack ID or group ID is blank, or the group ID is over 128 characters (pod 0, rack 1)"
		for _, tc := range []struct {
			name string
			inv  *inventory.SiteInventory
			want string
		}{
			{"nil", nil, "no inventory to plan writes for"},
			{"empty", &inventory.SiteInventory{}, "the inventory plans no write"},
			{"no records", inv(nil), "the inventory plans no write"},
			{"dpu", inv(nil, inventory.Device{Kind: inventory.KindDPU, BmcMac: secret}), "a DPU device has no expected record (pod 0, device 0)"},
			{"unknown dpu policy", inv(nil, inventory.Device{Kind: inventory.KindMachine, BmcMac: secret, Serial: "m001", DpuPolicy: &bad}),
				"a device has an unknown DPU policy (pod 0, device 0)"},
			{"a body that does not encode", inv(nil, inventory.Device{Kind: inventory.KindMachine, BmcMac: secret + "\xff", Serial: "m001"}), "the body does not encode (write 0)"},
			// nico-api would store its own profile, so only the configured one changed fails here, before any dispatch.
			{"only the configured profile changed", rack2(func(r *inventory.RackIntent) { r.RackProfileID = secret }),
				"the configured rack profile is not " + profileID + ", the one nico-api derives from the rack's group (pod 0, rack 1)"},
			{"a derived profile holding inventory text is not named", rack2(func(r *inventory.RackIntent) { r.Group.Topology = secret }),
				"the configured rack profile is not the one nico-api derives from the rack's group (pod 0, rack 1)"},
			// stored_expected_rack_group's identifiers, checked before any body is returned: a group ID of 129 characters, not bytes, is over the limit.
			{"a blank group ID", rack2(func(r *inventory.RackIntent) { r.Group.RackGroupID = " " }), badID},
			{"a 129-character group ID", rack2(func(r *inventory.RackIntent) { r.Group.RackGroupID = secret + strings.Repeat("é", 129-len(secret)) }), badID},
			{"a blank rack ID", rack2(func(r *inventory.RackIntent) { r.RackID = "\t" }), badID},
			// A group is written under its own ID: two racks naming one, in a pod or across pods, are refused before any write.
			{"two racks naming one group", rack2(func(r *inventory.RackIntent) { r.Group.RackGroupID = "g-r1" }), "two racks name the same group ID (pod 0, rack 1)"},
			{"two pods' racks naming one group", withPod(func(r *inventory.RackIntent) { r.Group.RackGroupID = "g-r1" }), "two racks name the same group ID (pod 1, rack 0)"},
			{"an invalid group", rack2(func(r *inventory.RackIntent) { r.Group.Members[1].Type = secret }),
				"a rack group member is blank, of no known type or listed twice (pod 0, rack 1)"},
		} {
			_, recs, err := Records(tc.inv)
			is(t, []any{fmt.Sprint(err), recs == nil}, []any{tc.want, true}, tc.name)
		}
	})
}

// TestRackProfileIsTheOneNicoAPIDerives: the vectors of derive_rack_profile_id and the stored group rules before it. A member is type:manufacturer[:id], its id
// otherwise its index.
func TestRackProfileIsTheOneNicoAPIDerives(t *testing.T) {
	const t72, v5, v6, cs = "gb200_nvl72r1_c2g4", "NVLINK_V5", "NVLINK_V6", "Compute:NVIDIA Switch:NVIDIA"
	const nv, badGroup, badMember, noKind = "_NVIDIA_NO_POWERSHELF", "the rack's group topology or protocol is blank or over 128 characters",
		"a rack group member is blank, of no known type or listed twice", "the rack's group has no Compute or no Switch member"
	long := strings.Repeat("a", 129)
	for _, tc := range []struct{ topology, protocol, members, want string }{
		{t72, v5, cs + " PowerShelf:NVIDIA", "GB200_NVL72R1_C2G4_NVIDIA"},
		{t72, v5, "Compute:NVIDIA Compute:NVIDIA Switch:NVIDIA", "GB200_NVL72R1_C2G4" + nv},
		// The first of WIWYNN, LENOVO and SMC (or Supermicro) any member names, in any ASCII case, else NVIDIA.
		{t72, v5, "Compute:SMC Switch:LENOVO PowerShelf:wiwynn", "GB200_NVL72R1_C2G4_WIWYNN"},
		{t72, v5, "Compute:Supermicro Switch:lenovo", "GB200_NVL72R1_C2G4_LENOVO_NO_POWERSHELF"},
		{t72, v5, "Compute:NVIDIA Switch:SuperMicro", "GB200_NVL72R1_C2G4_SMC_NO_POWERSHELF"},
		{t72, v5, "Compute:other-vendor Switch:NVIDIA Switch:\u017fmc", "GB200_NVL72R1_C2G4" + nv},
		// NVLINK_V6 makes a gb<digits>_ platform vr_; any other protocol, platform or topology stays.
		{"gb300_nvl72r1_c2g4", v6, cs, "VR_NVL72R1_C2G4" + nv},
		{"GB3_x_y", v6, cs, "VR_X_Y" + nv},
		{"gb300_nvl72r1_c2g4", v5, cs, "GB300_NVL72R1_C2G4" + nv},
		{"gb300_nvl72r1_c2g4", "nvlink_v6", cs, "GB300_NVL72R1_C2G4" + nv},
		{"Gb300_x", v6, cs, "GB300_X" + nv},
		{"gb_x", v6, cs, "GB_X" + nv},
		{"gb3a_x", v6, cs, "GB3A_X" + nv},
		{"gb300", v6, cs, "GB300" + nv},
		// An empty protocol is none, a blank one is refused; a length is in characters.
		{t72, "", cs, "GB200_NVL72R1_C2G4" + nv}, {t72, " ", cs, badGroup}, {t72, strings.Repeat("é", 128), cs, "GB200_NVL72R1_C2G4" + nv},
		// Uppercasing expands some characters, as Rust's to_uppercase does.
		{"straße_ǆ_ﬁ", v5, cs, "STRASSE_Ǆ_FI" + nv},
		{strings.Repeat("é", 128), v5, cs, strings.Repeat("É", 128) + nv},
		{" ", v5, cs, badGroup}, {long, v5, cs, badGroup}, {t72, long, cs, badGroup},
		{t72, v5, "Switch:NVIDIA", noKind}, {t72, v5, "Compute:NVIDIA PowerShelf:NVIDIA", noKind},
		{t72, v5, "compute:NVIDIA Switch:NVIDIA", badMember}, {t72, v5, "Compute: Switch:NVIDIA", badMember},
		{t72, v5, "Compute:NVIDIA: Switch:NVIDIA", badMember}, {t72, v5, "Compute:NVIDIA:x Compute:NVIDIA:x Switch:NVIDIA", badMember},
	} {
		g := inventory.RackGroup{Topology: tc.topology, Protocol: tc.protocol}
		for i, f := range strings.Fields(tc.members) {
			typ, rest, _ := strings.Cut(f, ":")
			manufacturer, id, ok := strings.Cut(rest, ":")
			if !ok {
				id = fmt.Sprint(i)
			}
			g.Members = append(g.Members, inventory.Member{Type: typ, Manufacturer: manufacturer, ID: id})
		}
		got, err := rackProfile(g)
		if err != nil {
			got = err.Error()
		}
		is(t, got, tc.want, tc.topology+" "+tc.protocol+" "+tc.members)
	}
}
