// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package ingestion sends the expected inventory machine-a-tron serves to nico-api: it plans one keyed write per record, builds the body machine-a-tron itself would
// send, dispatches the writes through the journaled writer in rack-then-device order and accounts each key's outcome.
package ingestion

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"regexp"
	"slices"
	"strings"
	"unicode/utf8"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"golang.org/x/text/cases"
	"golang.org/x/text/language"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/inventory"
)

// The factory credentials the simulated BMCs and switch NVOS accept until NICo rotates them (bmc-mock DUMMY_FACTORY_USERNAME, DUMMY_FACTORY_PASSWORD and
// DUMMY_NVOS_USERNAME).
const (
	bmcUsername     = "root"
	nvosUsername    = "admin"
	factoryPassword = "factory_password"
)

// chassisSerial is nico-api's expected-machine serial rule (CHASSIS_SERIAL_REGEX, crates/api-core/src/handlers/expected_machine.rs:43, enforced on add at :112).
var chassisSerial = regexp.MustCompile(`^[A-Za-z0-9_-]{4,32}$`)

var dpuModes = map[inventory.DpuPolicy]corev1.DpuMode{
	inventory.DpuManage: corev1.DpuMode_DPU_MODE,
	inventory.DpuNic:    corev1.DpuMode_NIC_MODE,
	inventory.DpuIgnore: corev1.DpuMode_NO_DPU,
}

// Record is one planned write: its nico-api body, its journal kind and the request digest every attempt of the key shares.
type Record struct {
	Kind   string
	Body   proto.Message
	Digest string
}

// Records plans the served inventory's writes and builds, for every planned key, the body machine-a-tron's own registration sends: a rack declares its own group
// before the rack, and each device carries the constant factory credentials. Every planned key has exactly one body and every body one planned key. An error
// names indices, never an inventory value.
func Records(inv *inventory.SiteInventory) (WritePlan, map[WriteKey]Record, error) {
	if inv == nil {
		return WritePlan{}, nil, errors.New("no inventory to plan writes for")
	}
	plan, err := NewPlan(inv)
	if err != nil {
		return WritePlan{}, nil, err
	}
	bodies := map[WriteKey]proto.Message{}
	for i := range inv.Pods {
		r := &inv.Pods[i].Records
		for j := range r.Racks {
			rack := &r.Racks[j]
			// stored_expected_rack_group's identifier rules: nico-api would refuse these only after other records were dispatched.
			if strings.TrimSpace(rack.RackID) == "" || strings.TrimSpace(rack.Group.RackGroupID) == "" || utf8.RuneCountInString(rack.Group.RackGroupID) > 128 {
				return WritePlan{}, nil, fmt.Errorf("a rack ID or group ID is blank, or the group ID is over 128 characters (pod %d, rack %d)", i, j)
			}
			// nico-api stores the profile it derives from the group in place of the one sent, so a mismatch is caught here or not at all, as machine-a-tron's
			// own registration does (ensure_derived_profile).
			profile, err := rackProfile(rack.Group)
			if err == nil && profile != rack.RackProfileID {
				err = errors.New("the configured rack profile is not the one nico-api derives from the rack's group")
				// The derived profile holds the group's topology, inventory text: it is named only when made of profile characters.
				if strings.Trim(profile, "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-") == "" {
					err = fmt.Errorf("the configured rack profile is not %s, the one nico-api derives from the rack's group", profile)
				}
			}
			if err != nil {
				return WritePlan{}, nil, fmt.Errorf("%w (pod %d, rack %d)", err, i, j)
			}
			bodies[GroupKey(rack.RackID)] = rackGroup(rack)
			bodies[RackKey(rack.RackID)] = &corev1.ExpectedRack{RackId: &corev1.RackId{Id: rack.RackID}, RackProfileId: &corev1.RackProfileId{Id: rack.RackProfileID}}
		}
		for j := range r.Devices {
			stream, body, err := deviceBody(&r.Devices[j])
			if err != nil {
				return WritePlan{}, nil, fmt.Errorf("%w (pod %d, device %d)", err, i, j)
			}
			bodies[DeviceKey(stream, r.Devices[j].BmcMac)] = body
		}
	}
	keys := plan.keys()
	if len(keys) == 0 {
		return WritePlan{}, nil, errors.New("the inventory plans no write")
	}
	recs := make(map[WriteKey]Record, len(keys))
	for n, k := range keys {
		body, ok := bodies[k]
		if !ok {
			return WritePlan{}, nil, fmt.Errorf("a planned write has no body (write %d)", n)
		}
		digest, err := digestOf(body)
		if err != nil {
			return WritePlan{}, nil, fmt.Errorf("%w (write %d)", err, n)
		}
		recs[k] = Record{Kind: kindOf(body), Body: body, Digest: digest}
	}
	if len(bodies) != len(recs) {
		return WritePlan{}, nil, errors.New("a body has no planned write")
	}
	return plan, recs, nil
}

// keys is every planned key: each rack's group, then the unit keys.
func (p *WritePlan) keys() []WriteKey {
	keys := make([]WriteKey, 0, 2*len(p.Racks))
	for _, r := range p.Racks {
		keys = append(keys, r.Group)
	}
	return append(keys, p.UnitKeys()...)
}

// rackProfile is the rack profile nico-api derives from a rack's own group, as machine-a-tron checks it: the group is first validated as a stored group
// (stored_expected_rack_group), then derive_rack_profile_id. An error is fixed text.
func rackProfile(g inventory.RackGroup) (string, error) {
	invalid := func(s string) bool { return strings.TrimSpace(s) == "" || utf8.RuneCountInString(s) > 128 }
	// add_expected_rack_group takes an empty protocol as none and refuses a blank one (crates/rpc/src/model/expected_rack_group.rs:82).
	if invalid(g.Topology) || g.Protocol != "" && invalid(g.Protocol) {
		return "", errors.New("the rack's group topology or protocol is blank or over 128 characters")
	}
	seen, kinds := map[inventory.Member]bool{}, map[string]bool{}
	for _, m := range g.Members {
		if strings.TrimSpace(m.Manufacturer) == "" || strings.TrimSpace(m.ID) == "" || !slices.Contains([]string{"Compute", "Switch", "PowerShelf"}, m.Type) || seen[m] {
			return "", errors.New("a rack group member is blank, of no known type or listed twice")
		}
		seen[m], kinds[m.Type] = true, true
	}
	if !kinds["Compute"] || !kinds["Switch"] {
		return "", errors.New("the rack's group has no Compute or no Switch member")
	}
	power := "_NO_POWERSHELF"
	if kinds["PowerShelf"] {
		power = ""
	}
	// The first vendor any member's manufacturer names, in ASCII case only (eq_ignore_ascii_case: the vendor is ASCII, so equal lengths rule out a non-ASCII fold).
	fold := func(m, vendor string) bool { return len(m) == len(vendor) && strings.EqualFold(m, vendor) }
	vendor := "NVIDIA"
	for _, v := range []string{"WIWYNN", "LENOVO", "SMC"} {
		if slices.ContainsFunc(g.Members, func(m inventory.Member) bool {
			return fold(m.Manufacturer, v) || v == "SMC" && fold(m.Manufacturer, "Supermicro")
		}) {
			vendor = v
			break
		}
	}
	// NVLINK_V6 names a gb<digits>_<suffix> topology's profile vr_<suffix>.
	topology := g.Topology
	if platform, suffix, ok := strings.Cut(topology, "_"); ok && g.Protocol == "NVLINK_V6" {
		gen, gb := strings.CutPrefix(platform, "gb")
		if !gb {
			gen, gb = strings.CutPrefix(platform, "GB")
		}
		if gb && gen != "" && strings.Trim(gen, "0123456789") == "" {
			topology = "vr_" + suffix
		}
	}
	// Full Unicode uppercase, as Rust's to_uppercase: some characters expand ("straße" is "STRASSE"), which strings.ToUpper's rune-by-rune mapping does not do.
	return cases.Upper(language.Und).String(topology) + "_" + vendor + power, nil
}

func rackGroup(rack *inventory.RackIntent) *corev1.ExpectedRackGroup {
	members := make([]*corev1.ExpectedRackGroupMember, 0, len(rack.Group.Members))
	for _, m := range rack.Group.Members {
		members = append(members, &corev1.ExpectedRackGroupMember{Type: m.Type, Manufacturer: m.Manufacturer, Id: m.ID})
	}
	return &corev1.ExpectedRackGroup{
		RackGroupId: &corev1.RackGroupId{Id: rack.Group.RackGroupID},
		Topology:    rack.Group.Topology,
		Protocol:    rack.Group.Protocol,
		Racks:       []*corev1.ExpectedRackGroupRack{{RackId: &corev1.RackId{Id: rack.RackID}, Members: members}},
	}
}

// deviceBody is a device's stream and body. A DPU has no record of its own.
func deviceBody(d *inventory.Device) (Stream, proto.Message, error) {
	var rackID *corev1.RackId
	if d.RackID != nil {
		// A rack-less device omits the rack; a named one must name a rack, or nico-api would store an empty reference.
		if strings.TrimSpace(*d.RackID) == "" {
			return "", nil, errors.New("a device names a blank rack ID")
		}
		rackID = &corev1.RackId{Id: *d.RackID}
	}
	switch d.Kind {
	case inventory.KindMachine:
		var mode *corev1.DpuMode
		if d.DpuPolicy != nil {
			m, ok := dpuModes[*d.DpuPolicy]
			if !ok {
				return "", nil, errors.New("a device has an unknown DPU policy")
			}
			mode = &m
		}
		if !chassisSerial.MatchString(d.Serial) {
			return "", nil, errors.New("a host's serial is not 4 to 32 letters, digits, hyphens or underscores")
		}
		nics := make([]*corev1.ExpectedHostNic, 0, len(d.Interfaces))
		for _, n := range d.Interfaces {
			nics = append(nics, &corev1.ExpectedHostNic{MacAddress: proto.String(n.MacAddress), Primary: proto.Bool(n.Primary),
				NetworkSegmentType: corev1.NetworkSegmentType_HOST_INBAND.Enum()})
		}
		m := &corev1.ExpectedMachine{
			BmcMacAddress: d.BmcMac, BmcUsername: bmcUsername, BmcPassword: factoryPassword, ChassisSerialNumber: d.Serial, HostNics: nics, RackId: rackID,
			IsDpfEnabled: proto.Bool(d.DpfEnabled), DpuMode: mode,
		}
		m.DpfEnabled = d.DpfEnabled //nolint:staticcheck // deprecated, but machine-a-tron still sets it beside is_dpf_enabled
		return Host, m, nil
	case inventory.KindSwitch:
		// The first NVOS MAC is the primary port.
		nvos := make([]*corev1.ExpectedNvosInterface, 0, len(d.NvosMacs))
		for i, mac := range d.NvosMacs {
			nvos = append(nvos, &corev1.ExpectedNvosInterface{MacAddress: mac, IsPrimary: i == 0})
		}
		return Switch, &corev1.ExpectedSwitch{
			BmcMacAddress: d.BmcMac, BmcUsername: bmcUsername, BmcPassword: factoryPassword, SwitchSerialNumber: d.Serial, RackId: rackID,
			NvosUsername: proto.String(nvosUsername), NvosPassword: proto.String(factoryPassword), NvosInterfaces: nvos,
		}, nil
	case inventory.KindPowerShelf:
		return PowerShelf, &corev1.ExpectedPowerShelf{
			BmcMacAddress: d.BmcMac, BmcUsername: bmcUsername, BmcPassword: factoryPassword, ShelfSerialNumber: d.Serial, RackId: rackID,
			BmcRetainCredentials: proto.Bool(true),
		}, nil
	}
	return "", nil, errors.New("a device has no expected record")
}

// kindOf is the journal kind of a body, or "" for a body that is no expected-inventory write.
func kindOf(body proto.Message) string {
	switch body.(type) {
	case *corev1.ExpectedRackGroup:
		return "expectedRackGroup"
	case *corev1.ExpectedRack:
		return "expectedRack"
	case *corev1.ExpectedMachine:
		return "expectedMachine"
	case *corev1.ExpectedSwitch:
		return "expectedSwitch"
	case *corev1.ExpectedPowerShelf:
		return "expectedPowerShelf"
	}
	return ""
}

// digestOf is the SHA-256, in hex, of the body's deterministic protobuf encoding.
func digestOf(body proto.Message) (string, error) {
	b, err := proto.MarshalOptions{Deterministic: true}.Marshal(body)
	if err != nil {
		return "", errors.New("the body does not encode")
	}
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:]), nil
}
