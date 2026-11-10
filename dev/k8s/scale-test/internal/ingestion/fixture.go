// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"net/netip"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
	"github.com/google/uuid"
)

type FixtureSettings struct {
	Name                   string                       `yaml:"name"`
	SiteID                 string                       `yaml:"siteId"`
	ProbeURL               string                       `yaml:"probeUrl"`
	MachineCapabilities    []probeapi.MachineCapability `yaml:"machineCapabilities"`
	IPBlockCIDR            string                       `yaml:"ipBlockCIDR"`
	AllocationPrefixLength int                          `yaml:"allocationPrefixLength"`
	SubnetPrefixLength     int                          `yaml:"subnetPrefixLength"`
	FabricPrefixCIDRs      []string                     `yaml:"fabricPrefixCIDRs"`
	Teardown               bool                         `yaml:"teardown"`
	SetupSeconds           uint64                       `yaml:"setupSeconds"`
}
type fixtureProbe interface {
	probe.Probe
	Fixture(context.Context, string) (probeapi.FixtureResponse, error)
}
type fixtureRecord struct {
	Kind     string         `json:"kind"`
	ID       string         `json:"id"`
	Path     string         `json:"path"`
	Provider bool           `json:"provider"`
	Owned    bool           `json:"owned"`
	Spec     map[string]any `json:"spec"`
	Proof    uint64         `json:"op,omitempty"`
	RunID    string         `json:"runId,omitempty"`
}
type fixtureLedger struct {
	Name, Site, Org, Provider, Origin, RunID string
	Records                                  []fixtureRecord
	Cohort                                   map[string]string
	TenantClient                             string `json:"tenantClient"`
}
type fixtureRun struct {
	in                     *instanceRun
	p                      fixtureProbe
	end                    time.Time
	epoch                  float64
	ledger                 fixtureLedger
	previous               map[string]fixtureRecord
	tenant, kind, provider string
	sequence               int
	report                 map[string]any
	imported               *fixtureLedger
	cleanupHosts           map[string]bool
	observed               map[string]transition
	loaded                 bool
	checkpoints            uint64
}

func validateFixture(s InstanceSettings) error {
	f := s.Fixture
	if f.Name == "" {
		return nil
	}
	if len(f.Name) > 40 || strings.ContainsFunc(f.Name, func(r rune) bool { safe := r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-'; return !safe }) || uuid.Validate(f.SiteID) != nil || (f.SetupSeconds == 0 || f.SetupSeconds > 30*86400) || len(f.MachineCapabilities) == 0 {
		return instanceIssue("fixture needs a stable name, site UUID, capabilities and setup budget")
	}
	block, err := netip.ParsePrefix(f.IPBlockCIDR)
	if err != nil || !block.Addr().Is4() || block != block.Masked() || f.AllocationPrefixLength < block.Bits() || f.SubnetPrefixLength < f.AllocationPrefixLength || f.SubnetPrefixLength > 30 {
		return instanceIssue("fixture IP block or allocation/subnet prefix is invalid")
	}
	a := s.Rest.Auth
	if a.TokenURL == "" || a.ProviderDirectory == "" || a.CredentialsDir == "" || a.TenantClientID == "" || a.TenantDirectory == "" {
		return instanceIssue("fixture needs provider credentials, tenant credentials, and a canonical token URL")
	}
	u, e := url.Parse(a.TokenURL)
	if e != nil || u.Scheme != "https" || u.Host == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return instanceIssue("token URL must be canonical HTTPS without credentials or query")
	}
	return nil
}

// Discovery has normalized dynamic specifications. Keep integer precision and
// validate the wire encoding before decoding their map values.
func decodeFixture(raw []byte, v any) error {
	var checked json.RawMessage
	if err := jsonx.Decode(raw, &checked); err != nil {
		return err
	}
	decoder := json.NewDecoder(bytes.NewReader(checked))
	decoder.UseNumber()
	decoder.DisallowUnknownFields()
	return decoder.Decode(v)
}
func object(raw json.RawMessage) (map[string]any, error) {
	var v map[string]any
	err := decodeFixture(raw, &v)
	return v, err
}
func text(v map[string]any, key string) string { s, _ := v[key].(string); return s }
func normalized(body map[string]any) map[string]any {
	b, _ := json.Marshal(body)
	v, _ := object(b)
	if script, ok := v["ipxeScript"].(string); ok {
		delete(v, "ipxeScript")
		v["ipxeScriptHash"] = digest([]byte(script))
	}
	delete(v, "labels")
	delete(v, "description")
	return v
}
func containsSpec(want, got any) bool {
	switch w := want.(type) {
	case map[string]any:
		g, ok := got.(map[string]any)
		if !ok {
			return false
		}
		for k, v := range w {
			if !containsSpec(v, g[k]) {
				return false
			}
		}
		return true
	case []any:
		g, ok := got.([]any)
		if !ok || len(w) != len(g) {
			return false
		}
		for i, v := range w {
			if !containsSpec(v, g[i]) {
				return false
			}
		}
		return true
	default:
		a, _ := json.Marshal(want)
		b, _ := json.Marshal(got)
		return string(a) == string(b)
	}
}
func (f *fixtureRun) lookup(kind string) ([]map[string]any, error) {
	ctx, cancel := context.WithDeadline(f.in.ctx, f.end)
	defer cancel()
	reply, err := f.p.Fixture(ctx, kind)
	if err != nil {
		return nil, instanceObservationError(err)
	}
	if reply.Kind != kind || reply.SiteID != f.in.t.s.Fixture.SiteID || reply.Org != f.in.t.s.Rest.Org || f.epoch != 0 && reply.ProcessStartSeconds != f.epoch || time.Since(reply.ReadStart.Time) < 0 || time.Since(reply.ReadStart.Time) > 30*time.Second || reply.ReadAt.Before(reply.ReadStart.Time) || reply.ReadAt.After(time.Now()) {
		return nil, instanceIssue("fixture discovery scope, epoch or freshness changed")
	}
	f.epoch = reply.ProcessStartSeconds
	raw, e := json.Marshal(reply)
	if e != nil {
		return nil, e
	}
	var evidence any = reply
	if len(raw) > int(journal.MaxLineBytes)/2 {
		f.sequence++
		name := fmt.Sprintf("%s.fixture-read-%d.json", f.in.t.cfg.RunID, f.sequence)
		hash, x := writeOnce(f.in.t.cfg.Journal.Dir, name, reply)
		if x != nil {
			return nil, x
		}
		evidence = map[string]string{"path": name, "sha256": hash}
	}
	if e = f.in.w.Record(f.in.ctx, "fixtureRead", evidence); e != nil {
		return nil, e
	}
	out := []map[string]any{}
	ids := map[string]bool{}
	for _, raw := range reply.Objects {
		v, e := object(raw)
		if e != nil || uuid.Validate(text(v, "id")) != nil || ids[text(v, "id")] {
			return nil, instanceIssue("fixture discovery has invalid or duplicate identities")
		}
		ids[text(v, "id")] = true
		out = append(out, v)
	}
	if kind != "blocks" && kind != "providerBlocks" && len(out) > 1 {
		return nil, instanceIssue("fixture discovery has duplicate names")
	}
	return out, nil
}
func (f *fixtureRun) wait(kind string, check func([]map[string]any) bool) ([]map[string]any, error) {
	for {
		rows, err := f.lookup(kind)
		if err != nil {
			return nil, err
		}
		if kind != "blocks" && len(rows) == 1 && text(rows[0], "status") == "Error" {
			return nil, instanceIssue("fixture entered Error: " + kind)
		}
		if check(rows) {
			return rows, nil
		}
		if !time.Now().Before(f.end) {
			return nil, instanceIssue("fixture did not settle within its setup budget")
		}
		ctx, cancel := context.WithDeadline(f.in.ctx, minTime(f.end, time.Now().Add(10*time.Second)))
		err = waitInstance(ctx, 10, f.in.healthStop)
		cancel()
		if err != nil && time.Now().Before(f.end) && !errors.Is(err, context.DeadlineExceeded) {
			return nil, err
		}
	}
}
func (f *fixtureRun) mutate(method, path string, body map[string]any, provider bool, count int, before any, guards ...func() error) (writer.Outcome, error) {
	var out writer.Outcome
	if !time.Now().Before(f.end) {
		return out, instanceIssue("fixture setup or teardown deadline passed")
	}
	if err := f.beforeWrite(); err != nil {
		return out, err
	}
	raw, err := json.Marshal(body)
	if err != nil {
		return out, err
	}
	if bytes.Equal(raw, []byte("null")) {
		raw = nil
	}

	var until time.Time
	for pass := 0; ; pass++ {
		if !f.in.rc.DryRun {
			ctx, cancel := context.WithDeadline(f.in.ctx, f.end)
			err = f.in.t.rest.authorize(ctx, provider)
			until = f.in.t.rest.until
			cancel()
			if err != nil {
				return out, err
			}
		}
		if err := f.in.safety(f.end); err != nil {
			return out, err
		}
		for _, guard := range guards {
			if err := guard(); err != nil {
				return out, err
			}
		}
		err = f.in.dispatchEvidence()
		if pass == 0 && errors.Is(err, errSafetyStop) {
			continue
		}
		if err != nil {
			return out, err
		}
		break
	}

	f.sequence++
	if err := f.in.w.Record(f.in.ctx, "fixtureIntent", map[string]any{"path": path, "method": method, "provider": provider, "spec": normalized(body), "before": before}); err != nil {
		return out, err
	}
	call := writer.Call{WriteCall: journal.WriteCall{Scope: "fixture", Kind: method, Key: fmt.Sprintf("%s/fixture/%d/%s", f.in.t.cfg.RunID, f.sequence, path), RequestDigest: digest(append([]byte(method+" "+path+"\n"), raw...))}, Retry: writer.RetryPolicy{MaxAttempts: 1, AttemptTimeout: min(300*time.Second, time.Until(f.end))}, Cutoff: f.end}
	call.BeforeDispatch = func() error {
		if err := validToken(until); err != nil {
			return err
		}
		return f.in.dispatchEvidence()
	}
	if f.in.rc.DryRun {
		return writer.Plan(f.in.ctx, f.in.w, call)
	}
	out, err = f.in.write(call, func(ctx context.Context) writer.Attempt {
		return f.in.t.rest.attempt(ctx, method, path, raw, count, provider)
	})
	if err == nil && out.Kind != writer.Accepted {
		err = instanceIssue("fixture mutation rejected or uncertain; read back and never replay")
	}
	return out, err
}

var fixturePaths = map[string]string{"account": "tenant/account", "type": "instance/type", "compute": "allocation", "ipallocation": "allocation", "ipblock": "ipblock", "vpc": "vpc", "subnet": "subnet", "os": "operating-system"}

func (f *fixtureRun) ensure(kind string, body map[string]any, status ...string) (map[string]any, error) {
	rows, err := f.lookup(kind)
	if err != nil {
		return nil, err
	}
	provider := kind == "account" || kind == "type" || kind == "compute" || kind == "ipallocation" || kind == "ipblock"
	want := normalized(body)
	expectedID := ""
	if len(rows) > 0 {
		expectedID = text(rows[0], "id")
		if err = f.scope(kind, rows[0]); err != nil {
			return nil, err
		}
	}
	var receipt writer.Outcome
	owned := false
	if len(rows) == 0 {
		receipt, err = f.mutate(http.MethodPost, fixturePaths[kind], body, provider, -1, nil)
		if err != nil {
			return nil, f.readUncertain(kind, receipt, err)
		}
		if f.in.rc.DryRun {
			return map[string]any{"id": "planned-" + kind}, nil
		}
		if len(receipt.IDs) != 1 {
			return nil, instanceIssue("fixture creation receipt has no unique ID")
		}
		expectedID, owned = receipt.IDs[0], true
	} else {
		if f.previous[kind].Owned && f.previous[kind].ID != text(rows[0], "id") {
			return nil, instanceIssue("owned fixture resource was replaced")
		}
		owned = f.previous[kind].Owned && f.previous[kind].ID == text(rows[0], "id")
		if !fixtureSpec(kind, want, rows[0]) {
			if kind != "compute" || !owned {
				return nil, instanceIssue("borrowed or incompatible fixture specification: " + kind)
			}
			constraints, _ := rows[0]["allocationConstraints"].([]any)
			desired, _ := want["allocationConstraints"].([]any)
			if len(constraints) != 1 || len(desired) != 1 {
				return nil, instanceIssue("owned allocation has ambiguous constraints")
			}
			old, _ := constraints[0].(map[string]any)
			next, _ := desired[0].(map[string]any)
			a, aErr := strconv.ParseUint(fmt.Sprint(old["constraintValue"]), 10, 64)
			b, bErr := strconv.ParseUint(fmt.Sprint(next["constraintValue"]), 10, 64)
			proof := maps.Clone(next)
			delete(proof, "constraintValue")
			if aErr != nil || bErr != nil || !containsSpec(proof, old) || a >= b || text(old, "id") == "" {
				return nil, instanceIssue("allocation specification cannot be adopted or reduced")
			}
			receipt, err = f.mutate(http.MethodPatch, "allocation/"+text(rows[0], "id")+"/constraint/"+text(old, "id"), map[string]any{"constraintValue": next["constraintValue"]}, true, 0, rows[0], f.unchanged(kind, rows[0]))
			if err != nil {
				return nil, err
			}
		}
	}
	proof := receipt.Op
	runID := f.in.t.journalID()
	if proof == 0 {
		proof = f.previous[kind].Proof
		runID = f.previous[kind].RunID
	}
	record := fixtureRecord{Kind: kind, ID: expectedID, Path: fixturePaths[kind] + "/" + expectedID, Provider: provider, Owned: owned, Spec: want, Proof: proof, RunID: runID}
	if err = f.record(record); err != nil {
		return nil, err
	}
	rows, err = f.wait(kind, func(rows []map[string]any) bool {
		return len(rows) == 1 && f.scope(kind, rows[0]) == nil && fixtureSpec(kind, want, rows[0]) && (len(status) == 0 || slices.Contains(status, text(rows[0], "status")))
	})
	if err != nil {
		return nil, err
	}
	if text(rows[0], "id") != expectedID {
		return nil, instanceIssue("fixture receipt disagrees with discovery")
	}
	return rows[0], nil
}

func (f *fixtureRun) record(record fixtureRecord) error {
	if i := slices.IndexFunc(f.ledger.Records, func(r fixtureRecord) bool { return r.Kind == record.Kind }); i >= 0 {
		f.ledger.Records[i] = record
	} else {
		f.ledger.Records = append(f.ledger.Records, record)
	}
	if err := f.in.w.Record(f.in.ctx, "fixtureResource", record); err != nil {
		return err
	}
	return f.checkpoint()
}
func (f *fixtureRun) unchanged(kind string, row map[string]any) func() error {
	before := normalized(row)
	return func() error {
		rows, err := f.lookup(kind)
		if err != nil {
			return err
		}
		if len(rows) != 1 || f.scope(kind, rows[0]) != nil || !containsSpec(before, rows[0]) || !containsSpec(normalized(rows[0]), before) {
			return instanceIssue("fixture resource changed before dispatch: " + kind)
		}
		return nil
	}
}

func (f *fixtureRun) coveringBlock() (map[string]any, error) {
	wanted, _ := netip.ParsePrefix(f.in.t.s.Fixture.IPBlockCIDR)
	rows, err := f.lookup("providerBlocks")
	if err != nil {
		return nil, err
	}
	var root map[string]any
	for _, row := range rows {
		if row["tenantId"] != nil {
			continue
		}
		p, err := netip.ParsePrefix(text(row, "prefix") + "/" + fmt.Sprint(row["prefixLength"]))
		if err != nil || p != p.Masked() || f.scope("ipblock", row) != nil {
			return nil, instanceIssue("provider IP block scope or prefix is invalid")
		}
		if !p.Overlaps(wanted) {
			continue
		}
		if p.Bits() > wanted.Bits() {
			return nil, instanceIssue("provider IP block only partially overlaps fixture CIDR")
		}
		if text(row, "name") == f.in.t.s.Fixture.Name+"-ipblock" {
			continue
		}
		if root != nil || text(row, "status") != "Ready" || text(row, "protocolVersion") != "IPv4" {
			return nil, instanceIssue("covering provider IP block is ambiguous or not Ready IPv4")
		}
		root = row
	}
	if root == nil || f.previous["ipblock"].Owned {
		return nil, nil
	}
	id := text(root, "id")
	return root, f.record(fixtureRecord{Kind: "ipblock", ID: id, Path: "ipblock/" + id, Provider: true, Spec: normalized(root)})
}
func borrowedAccount(row map[string]any, owned bool) error {
	if !owned && (uuid.Validate(text(row, "tenantId")) != nil || text(row, "status") != "Ready" || row["targetedInstanceCreation"] != true) {
		return instanceIssue("borrowed account must already be linked, Ready and targeted")
	}
	return nil
}
func (f *fixtureRun) setup() error {
	s := f.in.t.s
	name := s.Fixture.Name
	if err := f.load(); err != nil {
		return err
	}
	ip, err := f.coveringBlock()
	if err != nil {
		return err
	}
	if f.in.rc.DryRun {
		return f.planOnly(ip != nil)
	}
	accounts, err := f.lookup("account")
	if err != nil {
		return err
	}
	if len(accounts) == 1 {
		prior := f.previous["account"]
		if prior.Owned && prior.ID != text(accounts[0], "id") {
			return instanceIssue("owned account was replaced")
		}
		if err := borrowedAccount(accounts[0], prior.Owned); err != nil {
			return err
		}
	}
	if err := f.tenantIdentity(); err != nil {
		return err
	}
	account, err := f.ensure("account", map[string]any{"infrastructureProviderId": f.provider, "tenantOrg": s.Rest.Org}, "Invited", "Ready")
	if err != nil {
		return err
	}
	accountID := text(account, "id")
	sameAccount := func(row map[string]any) bool {
		return text(row, "id") == accountID && text(row, "tenantOrg") == s.Rest.Org && f.scope("account", row) == nil
	}
	ownedAccount := false
	for _, record := range f.ledger.Records {
		ownedAccount = ownedAccount || record.Kind == "account" && record.ID == text(account, "id") && record.Owned
	}
	if err := borrowedAccount(account, ownedAccount); err != nil {
		return err
	}
	if text(account, "tenantId") == "" {
		// This GET mutates tenant linkage. Its body is deliberately discarded.
		if _, err = f.mutate(http.MethodGet, "tenant/current", nil, false, 0, account, f.unchanged("account", account)); err != nil {
			_, readErr := f.lookup("account")
			return errors.Join(err, readErr)
		}
		rows, e := f.wait("account", func(rows []map[string]any) bool { return len(rows) == 1 && text(rows[0], "tenantId") != "" })
		if e != nil {
			return e
		}
		if !sameAccount(rows[0]) {
			return instanceIssue("owned account identity or scope changed")
		}
		account = rows[0]
	}
	f.tenant = text(account, "tenantId")
	if uuid.Validate(f.tenant) != nil {
		return instanceIssue("provider account has no valid linked tenant")
	}
	if text(account, "status") == "Invited" {
		if _, err = f.mutate(http.MethodPatch, "tenant/account/"+text(account, "id"), map[string]any{}, false, 0, account, f.unchanged("account", account)); err != nil {
			return err
		}
	}
	accounts, err = f.wait("account", func(rows []map[string]any) bool {
		return len(rows) == 1 && text(rows[0], "tenantId") == f.tenant && text(rows[0], "status") == "Ready"
	})
	if err != nil {
		return err
	}
	if !sameAccount(accounts[0]) {
		return instanceIssue("owned account identity or scope changed")
	}
	if err = f.targetedAccount(accounts[0]); err != nil {
		return err
	}
	kind, err := f.ensure("type", map[string]any{"name": name + "-type", "siteId": s.Fixture.SiteID, "machineCapabilities": s.Fixture.MachineCapabilities}, "Ready")
	if err != nil {
		return err
	}
	f.kind = text(kind, "id")
	if err = f.associate(); err != nil {
		return err
	}
	constraint := func(kind, id string, value any) []any {
		return []any{map[string]any{"resourceType": kind, "resourceTypeId": id, "constraintType": "Reserved", "constraintValue": value}}
	}
	if _, err = f.ensure("compute", map[string]any{"name": name + "-compute", "siteId": s.Fixture.SiteID, "tenantId": f.tenant, "allocationConstraints": constraint("InstanceType", f.kind, s.Instance.N+uint64(len(f.in.latest.rows[probeapi.FamilyInstance])))}, "Registered"); err != nil {
		return err
	}
	block, _ := netip.ParsePrefix(s.Fixture.IPBlockCIDR)
	if ip == nil {
		ip, err = f.ensure("ipblock", map[string]any{"name": name + "-ipblock", "siteId": s.Fixture.SiteID, "prefix": block.Addr().String(), "prefixLength": block.Bits(), "routingType": "DatacenterOnly", "protocolVersion": "IPv4"}, "Ready")
		if err != nil {
			return err
		}
	}
	// REST chooses the allocation's child prefix within the selected parent.
	block, _ = netip.ParsePrefix(text(ip, "prefix") + "/" + fmt.Sprint(ip["prefixLength"]))
	allocationSpec := map[string]any{"name": name + "-ipallocation", "siteId": s.Fixture.SiteID, "tenantId": f.tenant, "allocationConstraints": constraint("IPBlock", text(ip, "id"), s.Fixture.AllocationPrefixLength)}
	allocation, err := f.ensure("ipallocation", allocationSpec, "Registered")
	if err != nil {
		return err
	}
	derived := func(v map[string]any) string {
		a, _ := v["allocationConstraints"].([]any)
		if len(a) != 1 {
			return ""
		}
		c, _ := a[0].(map[string]any)
		return text(c, "derivedResourceId")
	}
	if derived(allocation) == "" {
		var changed error
		rows, e := f.wait("ipallocation", func(rows []map[string]any) bool {
			if len(rows) != 1 {
				return false
			}
			if text(rows[0], "id") != text(allocation, "id") || f.scope("ipallocation", rows[0]) != nil || !fixtureSpec("ipallocation", normalized(allocationSpec), rows[0]) || text(rows[0], "status") != "Registered" {
				changed = instanceIssue("fixture IP allocation identity, specification, scope or status changed")
			}
			return changed != nil || derived(rows[0]) != ""
		})
		if e != nil || changed != nil {
			return errors.Join(e, changed)
		}
		allocation = rows[0]
	}
	child := derived(allocation)
	var childPrefix netip.Prefix
	if _, err = f.wait("blocks", func(rows []map[string]any) bool {
		for _, r := range rows {
			if text(r, "id") == child && text(r, "tenantId") == f.tenant && text(r, "status") == "Ready" {
				p, e := netip.ParsePrefix(text(r, "prefix") + "/" + fmt.Sprint(r["prefixLength"]))
				childPrefix = p
				return e == nil && p == p.Masked() && p.Bits() == s.Fixture.AllocationPrefixLength && block.Contains(p.Addr())
			}
		}
		return false
	}); err != nil {
		return err
	}
	attestation := s.Fixture
	attestation.IPBlockCIDR = childPrefix.String()
	if err = fabricPrerequisite(attestation); err != nil {
		return err
	}
	vpc, err := f.ensure("vpc", map[string]any{"name": name + "-vpc", "siteId": s.Fixture.SiteID, "networkVirtualizationType": "ETHERNET_VIRTUALIZER"}, "Ready")
	if err != nil {
		return err
	}
	subnet, err := f.ensure("subnet", map[string]any{"name": name + "-subnet", "vpcId": text(vpc, "id"), "ipv4BlockId": child, "prefixLength": s.Fixture.SubnetPrefixLength}, "Ready")
	if err != nil {
		return err
	}
	script, err := readScript(s.Instance.IPXEFile)
	if err != nil {
		return err
	}
	os, err := f.ensure("os", map[string]any{"name": name + "-os", "ipxeScript": script, "phoneHomeEnabled": false}, "Ready")
	if err != nil {
		return err
	}
	f.in.t.s.Instance.TenantID, f.in.t.s.Instance.InstanceTypeID, f.in.t.s.Instance.VPCID, f.in.t.s.Instance.SubnetID, f.in.t.s.Instance.OperatingSystemID = f.tenant, f.kind, text(vpc, "id"), text(subnet, "id"), text(os, "id")
	return nil
}

// A larger compatible reservation can be borrowed. Only journal-owned quota
// is ever grown, and existing reservations are never reduced.
func fixtureSpec(kind string, want, got map[string]any) bool {
	if kind != "compute" {
		return containsSpec(want, got)
	}
	a, _ := want["allocationConstraints"].([]any)
	b, _ := got["allocationConstraints"].([]any)
	if len(a) != 1 || len(b) != 1 {
		return false
	}
	wa, _ := a[0].(map[string]any)
	ga, _ := b[0].(map[string]any)
	n, e := strconv.ParseUint(fmt.Sprint(wa["constraintValue"]), 10, 64)
	m, x := strconv.ParseUint(fmt.Sprint(ga["constraintValue"]), 10, 64)
	if e != nil || x != nil || m < n {
		return false
	}
	c := maps.Clone(wa)
	c["constraintValue"] = ga["constraintValue"]
	w := maps.Clone(want)
	w["allocationConstraints"] = []any{c}
	return containsSpec(w, got)
}
func (f *fixtureRun) planOnly(borrowedBlock bool) error {
	for _, kind := range []string{"account", "type", "compute", "ipblock", "ipallocation", "vpc", "subnet", "os"} {
		if kind == "ipblock" && borrowedBlock {
			continue
		}
		if _, err := f.mutate(http.MethodPost, fixturePaths[kind], map[string]any{"name": f.in.t.s.Fixture.Name + "-" + kind, "plannedDependencies": true}, kind != "vpc" && kind != "subnet" && kind != "os", 0, nil); err != nil {
			return err
		}
	}
	f.report["setup"] = map[string]string{"verdict": "planned"}
	f.report["measurement"] = map[string]string{"verdict": "skipped"}
	return nil
}

func (f *fixtureRun) scope(kind string, row map[string]any) error {
	if kind != "os" && kind != "account" && text(row, "siteId") != f.in.t.s.Fixture.SiteID {
		return instanceIssue("fixture resource site changed")
	}
	if kind == "compute" || kind == "ipallocation" || kind == "vpc" || kind == "os" {
		if text(row, "tenantId") != f.tenant {
			return instanceIssue("fixture resource tenant changed")
		}
	}
	if kind == "type" || kind == "ipblock" || kind == "account" {
		if text(row, "infrastructureProviderId") != f.provider {
			return instanceIssue("fixture resource provider changed")
		}
	}
	return nil
}

func fabricPrerequisite(s FixtureSettings) error {
	block, err := netip.ParsePrefix(s.IPBlockCIDR)
	if err != nil {
		return instanceIssue("fixture IP block invalid")
	}
	for _, raw := range s.FabricPrefixCIDRs {
		p, e := netip.ParsePrefix(raw)
		if e == nil && p == p.Masked() && p.Bits() <= block.Bits() && p.Contains(block.Addr()) {
			return nil
		}
	}
	return instanceIssue("site fabric-prefix prerequisite: operator attestation must cover the fixture IP block")
}
func (f *fixtureRun) readUncertain(kind string, out writer.Outcome, cause error) error {
	var op *writer.OpError
	if out.Kind != writer.Unclear && (!errors.As(cause, &op) || !op.Dispatched) {
		return cause
	}
	until := minTime(f.end, time.Now().Add(300*time.Second))
	for {
		rows, err := f.lookup(kind)
		if err != nil {
			return errors.Join(cause, err)
		}
		if len(rows) > 0 || !time.Now().Before(until) {
			return cause
		}
		ctx, cancel := context.WithDeadline(f.in.ctx, minTime(f.end, time.Now().Add(10*time.Second)))
		err = waitInstance(ctx, 10)
		cancel()
		if err != nil && time.Now().Before(f.end) && !errors.Is(err, context.DeadlineExceeded) {
			return errors.Join(cause, err)
		}
	}
}

func list(v any) []any { out, _ := v.([]any); return out }

// Replace only the targeted capability, preserving defaults and other sites.
func (f *fixtureRun) targetedAccount(row map[string]any) error {
	if enabled, ok := row["targetedInstanceCreation"].(bool); ok && enabled {
		return nil
	}
	owned := false
	for _, r := range f.ledger.Records {
		owned = owned || r.Kind == "account" && r.ID == text(row, "id") && r.Owned
	}
	if !owned {
		return instanceIssue("borrowed account lacks targeted instance creation for the site")
	}
	caps := list(row["siteCapabilities"])
	if len(caps) == 0 {
		return instanceIssue("account capability evidence unavailable")
	}
	updates := []any{}
	for _, raw := range caps {
		cap, _ := raw.(map[string]any)
		ids := []string{}
		for _, rawID := range list(cap["siteIds"]) {
			if id, _ := rawID.(string); id != f.in.t.s.Fixture.SiteID {
				ids = append(ids, id)
			}
		}
		if len(ids) > 0 || len(list(cap["siteIds"])) == 0 {
			updates = append(updates, map[string]any{"siteIds": ids, "targetedInstanceCreation": cap["targetedInstanceCreation"]})
		}
	}
	updates = append(updates, map[string]any{"siteIds": []string{f.in.t.s.Fixture.SiteID}, "targetedInstanceCreation": true})
	if _, err := f.mutate(http.MethodPatch, "tenant/account/"+text(row, "id"), map[string]any{"siteCapabilities": updates}, true, 0, row, f.unchanged("account", row)); err != nil {
		return err
	}
	_, err := f.wait("account", func(rows []map[string]any) bool {
		return len(rows) == 1 && text(rows[0], "id") == text(row, "id") && f.scope("account", rows[0]) == nil && text(rows[0], "tenantOrg") == f.in.t.s.Rest.Org && rows[0]["targetedInstanceCreation"] == true
	})
	return err
}
