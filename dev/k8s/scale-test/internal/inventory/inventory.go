// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package inventory reads the expected inventory every machine-a-tron pod serves on /expected-inventory/records, over the pod IP. Fleet-up proves it and ingestion sends
// it, so N per type comes from here. The MAT-record to nico-api body conversion and the request digest belong to the ingestion change; this package reads the records and
// checks their structural links.
package inventory

import (
	"context"
	"fmt"
	"net/netip"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"sync"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// RecordsPath is the MAT control route serving the expected records.
const RecordsPath = "/expected-inventory/records"

// maxReadWorkers bounds how many pod records reads run at once.
const maxReadWorkers = 16

// PodLister lists the MAT pods. cluster.Cluster satisfies it.
type PodLister interface {
	ListPods(ctx context.Context, namespace, selector string) ([]cluster.PodInfo, error)
}

// RecordsReader reads one pod's records at url.
type RecordsReader interface {
	Read(ctx context.Context, url string) (Response, error)
}

// Response is the /expected-inventory/records reply.
type Response struct {
	InventoryID              string       `json:"inventoryId"`
	RegisterExpectedMachines bool         `json:"registerExpectedMachines"`
	CarbideAPIURL            string       `json:"carbideApiUrl"`
	Racks                    []RackIntent `json:"racks"`
	Devices                  []Device     `json:"devices"`
	DPUs                     []DPU        `json:"dpus"`
}

type RackIntent struct {
	RackID        string    `json:"rackId"`
	RackProfileID string    `json:"rackProfileId"`
	Group         RackGroup `json:"group"`
}

type RackGroup struct {
	RackGroupID string   `json:"rackGroupId"`
	Topology    string   `json:"topology"`
	Protocol    string   `json:"protocol"`
	Members     []Member `json:"members"`
}

type Member struct {
	Type         string `json:"type"`
	Manufacturer string `json:"manufacturer"`
	ID           string `json:"id"`
}

// DeviceKind is a served device's kind, on the wire in serde snake_case.
type DeviceKind string

const (
	KindMachine    DeviceKind = "machine"
	KindDPU        DeviceKind = "dpu"
	KindSwitch     DeviceKind = "switch"
	KindPowerShelf DeviceKind = "power_shelf"
)

// DpuPolicy is the operator's DPU policy for a host, preserved so the ingestion conversion can rebuild the write. Optional: absent means site default.
type DpuPolicy string

const (
	DpuManage DpuPolicy = "manage"
	DpuNic    DpuPolicy = "nic"
	DpuIgnore DpuPolicy = "ignore"
)

func init() {
	jsonx.Enum(KindMachine, KindDPU, KindSwitch, KindPowerShelf)
	jsonx.Enum(DpuManage, DpuNic, DpuIgnore)
}

type Device struct {
	Kind         DeviceKind `json:"kind"`
	HardwareType string     `json:"hardwareType"`
	BmcMac       string     `json:"bmcMac"`
	Serial       string     `json:"serial"`
	RackID       *string    `json:"rackId"` // RackID and DpuPolicy are serde Options: absent or null reads as nil

	DpuPolicy  *DpuPolicy `json:"dpuPolicy"`
	DpfEnabled bool       `json:"dpfEnabled"`
	Interfaces []Nic      `json:"interfaces"`
	NvosMacs   []string   `json:"nvosMacs"`
}

type Nic struct {
	MacAddress string `json:"macAddress"`
	Primary    bool   `json:"primary"`
}

type DPU struct {
	BmcMac       string `json:"bmcMac"`
	Serial       string `json:"serial"`
	ParentBmcMac string `json:"parentBmcMac"`
}

// RecordsURL is the records URL of the MAT pod at ip. IPv6 is bracketed.
func RecordsURL(ip netip.Addr, port uint16) string {
	return "https://" + netip.AddrPortFrom(ip, port).String() + RecordsPath
}

// CheckedAPIURL is why raw cannot be the nico-api URL, or "": HTTPS to a host on a usable port, with no credentials, query, fragment or path (the client dials host and
// port only). The reason is fixed text, never the scheme, port or any part of the URL, which may hold credentials.
func CheckedAPIURL(raw string) string {
	u, err := url.Parse(raw)
	switch {
	case err != nil:
		return "the nico-api URL does not parse"
	case u.Scheme != "https":
		return "the nico-api URL is not https"
	case u.User != nil:
		return "the nico-api URL carries credentials"
	case u.Hostname() == "":
		return "the nico-api URL has no host"
	case strings.Contains(raw, "?") || u.RawQuery != "" || u.ForceQuery:
		return "the nico-api URL has a query"
	case strings.Contains(raw, "#") || u.Fragment != "":
		return "the nico-api URL has a fragment"
	case u.Path != "" && u.Path != "/":
		return "the nico-api URL has a path"
	case badPort(u.Port()):
		return "the nico-api URL has an invalid port"
	}
	return ""
}

// badPort reports a port that is present but not 1 to 65535. The client dials it, so port 0 and an out-of-range port are unusable.
func badPort(port string) bool {
	if port == "" {
		return false
	}
	n, err := strconv.Atoi(port)
	return err != nil || n < 1 || n > 65535
}

// TypeCounts is units per type, as the stages count them.
type TypeCounts struct {
	RackGroup  uint64 `json:"rackGroup"`
	Rack       uint64 `json:"rack"`
	Host       uint64 `json:"host"`
	DPU        uint64 `json:"dpu"`
	Switch     uint64 `json:"switch"`
	PowerShelf uint64 `json:"powerShelf"`
}

func (c *TypeCounts) add(r *Response) {
	c.RackGroup += uint64(len(r.Racks)) // one group per rack
	c.Rack += uint64(len(r.Racks))
	c.DPU += uint64(len(r.DPUs))
	for _, d := range r.Devices {
		switch d.Kind {
		case KindMachine:
			c.Host++
		case KindSwitch:
			c.Switch++
		case KindPowerShelf:
			c.PowerShelf++
		}
	}
}

// BmcKey is the kind of a MAT BMC Service: its machine type and, for a host Service, the hardware type device status reports.
func BmcKey(machineType, hardwareType string) string {
	if hardwareType == "" {
		return machineType
	}
	return machineType + "/" + hardwareType
}

// MatPod is one MAT pod and the records it served.
type MatPod struct {
	Name    string
	IP      netip.Addr
	Records Response
}

// SiteInventory is the served inventory of every MAT pod; a non-empty Problems fails matInventory.
type SiteInventory struct {
	Pods      []MatPod
	N         TypeCounts
	BmcCounts map[string]uint64
	Problems  []string
}

// OK reports no problem.
func (s *SiteInventory) OK() bool { return len(s.Problems) == 0 }

// CarbideAPIURL is the nico-api URL every pod loaded, when they agree.
func (s *SiteInventory) CarbideAPIURL() (string, bool) {
	if len(s.Pods) == 0 {
		return "", false
	}
	first := s.Pods[0].Records.CarbideAPIURL
	for _, p := range s.Pods[1:] {
		if p.Records.CarbideAPIURL != first {
			return "", false
		}
	}
	return first, true
}

// Read reads the inventory of every MAT pod matching selector in namespace, in parallel. The returned inventory holds what was read; Problems carries faults.
func Read(ctx context.Context, lister PodLister, namespace string, reader RecordsReader, selector string, port uint16) SiteInventory {
	inv := SiteInventory{BmcCounts: map[string]uint64{}}
	pods, err := lister.ListPods(ctx, namespace, selector)
	switch {
	case err != nil:
		inv.Problems = append(inv.Problems, fmt.Sprintf("listing MAT pods %s in %s: %s", selector, namespace, err))
		return inv
	case len(pods) == 0:
		inv.Problems = append(inv.Problems, fmt.Sprintf("no MAT pod matches %s in %s", selector, namespace))
		return inv
	}

	type result struct {
		pod     MatPod
		problem string
	}
	// The list permits up to 100,000 pods and each reply up to 64 MiB, so the reads run through a small fixed pool, not one goroutine per pod.
	results := make([]result, len(pods))
	jobs := make(chan int)
	var wg sync.WaitGroup
	for range min(maxReadWorkers, len(pods)) {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for i := range jobs {
				mp, problem := readPod(ctx, reader, pods[i], port)
				results[i] = result{mp, problem}
			}
		}()
	}
	sent := len(pods)
dispatch:
	for i := range pods {
		// Stop feeding once stopped, before the select, so a ready worker is never chosen over an already-elapsed stop.
		if callerStop(ctx) != nil {
			sent = i
			break dispatch
		}
		select {
		case <-ctx.Done():
			sent = i
			break dispatch
		case jobs <- i:
		}
	}
	close(jobs)
	wg.Wait()
	for i := sent; i < len(pods); i++ {
		results[i] = result{problem: fmt.Sprintf("pod %s: not read: %s", pods[i].Name, callerStop(ctx))}
	}
	for _, r := range results {
		if r.problem != "" {
			inv.Problems = append(inv.Problems, r.problem)
		} else {
			inv.Pods = append(inv.Pods, r.pod)
		}
	}

	inv.Problems = append(inv.Problems, checkPods(inv.Pods)...)
	for i := range inv.Pods {
		r := &inv.Pods[i].Records
		inv.N.add(r)
		for _, d := range r.Devices {
			inv.BmcCounts[BmcKey("host", d.HardwareType)]++
		}
		inv.BmcCounts[BmcKey("dpu", "")] += uint64(len(r.DPUs))
	}
	return inv
}

func readPod(ctx context.Context, reader RecordsReader, pod cluster.PodInfo, port uint16) (MatPod, string) {
	if !pod.Ready {
		return MatPod{}, fmt.Sprintf("pod %s: not Ready (phase %s)", pod.Name, pod.Phase)
	}
	if pod.IP == "" {
		return MatPod{}, fmt.Sprintf("pod %s: has no IP", pod.Name)
	}
	ip, err := netip.ParseAddr(pod.IP)
	if err != nil {
		return MatPod{}, fmt.Sprintf("pod %s: IP %q is not an IP address", pod.Name, pod.IP)
	}
	if s := callerStop(ctx); s != nil { // checked immediately before the read
		return MatPod{}, fmt.Sprintf("pod %s: not read: %s", pod.Name, s)
	}
	records, err := reader.Read(ctx, RecordsURL(ip, port))
	if err != nil {
		return MatPod{}, fmt.Sprintf("pod %s: reading its records: %s", pod.Name, err)
	}
	return MatPod{Name: pod.Name, IP: ip, Records: records}, ""
}

// owners keeps the owners of a key in first-seen order; the caller sorts.
type owners struct {
	keys map[string][]string
	seen []string
}

func (o *owners) add(key, owner string) {
	if o.keys == nil {
		o.keys = map[string][]string{}
	}
	if _, ok := o.keys[key]; !ok {
		o.seen = append(o.seen, key)
	}
	o.keys[key] = append(o.keys[key], owner)
}

// checkPods finds the problems across the served records: registration still on, pods that disagree on the nico-api URL, a BMC MAC or rack served twice, a device naming
// an unserved rack, a DPU whose parent is no served host, an interface or NVOS MAC on two devices. MACs compare in any case; the record-conversion fault is found by the
// ingestion conversion.
func checkPods(pods []MatPod) []string {
	var problems []string
	apiURLs := owners{}
	bmcMacs := owners{}
	racks := owners{}
	for _, pod := range pods {
		r := &pod.Records
		if r.RegisterExpectedMachines {
			problems = append(problems, fmt.Sprintf(
				"pod %s registers its own expected records (register_expected_machines is true)", pod.Name))
		}
		if e := CheckedAPIURL(r.CarbideAPIURL); e != "" {
			problems = append(problems, fmt.Sprintf("pod %s: %s", pod.Name, e))
		}
		apiURLs.add(r.CarbideAPIURL, pod.Name)
		for _, d := range r.Devices {
			bmcMacs.add(strings.ToLower(d.BmcMac), pod.Name)
		}
		for _, d := range r.DPUs {
			bmcMacs.add(strings.ToLower(d.BmcMac), pod.Name)
		}
		for _, rack := range r.Racks {
			racks.add(rack.RackID, pod.Name)
		}
	}
	// Pods grouped by the URL they loaded, never the URL itself (credentials).
	if len(apiURLs.seen) > 1 {
		groups := make([]string, 0, len(apiURLs.seen))
		for _, u := range apiURLs.seen {
			groups = append(groups, "["+strings.Join(apiURLs.keys[u], ", ")+"]")
		}
		slices.Sort(groups)
		problems = append(problems, "pods disagree on carbide_api_url, by URL: "+strings.Join(groups, ", "))
	}
	for _, mac := range sorted(bmcMacs.seen) {
		if o := bmcMacs.keys[mac]; len(o) > 1 {
			problems = append(problems, fmt.Sprintf("BMC MAC %s is served %d times, by %s", mac, len(o), strings.Join(o, ", ")))
		}
	}
	for _, rack := range sorted(racks.seen) {
		if o := racks.keys[rack]; len(o) > 1 {
			problems = append(problems, fmt.Sprintf("rack %s is served %d times, by %s", rack, len(o), strings.Join(o, ", ")))
		}
	}
	hosts := map[string]bool{} // host BMC MACs on any pod (a DPU's parent may be elsewhere)
	for _, pod := range pods {
		for _, d := range pod.Records.Devices {
			if d.Kind == KindMachine {
				hosts[strings.ToLower(d.BmcMac)] = true
			}
		}
	}
	nicMacs := owners{}
	nicSeen := map[string]map[string]bool{}
	for _, pod := range pods {
		for _, d := range pod.Records.DPUs {
			if !hosts[strings.ToLower(d.ParentBmcMac)] {
				problems = append(problems, fmt.Sprintf(
					"pod %s DPU %s names parent %s, which is no served host", pod.Name, d.BmcMac, d.ParentBmcMac))
			}
		}
		for _, d := range pod.Records.Devices {
			owner := fmt.Sprintf("%s device %s", pod.Name, d.BmcMac)
			macs := make([]string, 0, len(d.Interfaces)+len(d.NvosMacs))
			for _, nic := range d.Interfaces {
				macs = append(macs, nic.MacAddress)
			}
			macs = append(macs, d.NvosMacs...)
			for _, raw := range macs {
				mac := strings.ToLower(raw)
				if nicSeen[mac] == nil {
					nicSeen[mac] = map[string]bool{}
				}
				if !nicSeen[mac][owner] {
					nicSeen[mac][owner] = true
					nicMacs.add(mac, owner)
				}
			}
		}
	}
	for _, mac := range sorted(nicMacs.seen) {
		if o := nicMacs.keys[mac]; len(o) > 1 {
			slices.Sort(o)
			problems = append(problems, fmt.Sprintf("MAC %s is served by %d devices: %s", mac, len(o), strings.Join(o, ", ")))
		}
	}
	served := map[string]bool{}
	for _, rack := range racks.seen {
		served[rack] = true
	}
	for _, pod := range pods {
		for _, d := range pod.Records.Devices {
			if d.RackID != nil && !served[*d.RackID] {
				problems = append(problems, fmt.Sprintf(
					"pod %s device %s names rack %s, which no pod serves", pod.Name, d.BmcMac, *d.RackID))
			}
		}
	}
	return problems
}

func sorted(s []string) []string {
	out := slices.Clone(s)
	slices.Sort(out)
	return out
}
