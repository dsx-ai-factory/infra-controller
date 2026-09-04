// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"

	cli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

// LoginFunc is a callback to perform login and return a new token.
type LoginFunc func() (string, error)

// Scope holds the current filter context for the interactive session.
type Scope struct {
	SiteID       string
	SiteName     string
	VpcID        string
	VpcName      string
	LabelFilters map[string]string
}

// Session holds the shared state for an interactive TUI session.
type Session struct {
	Client     *cli.Client
	ConfigPath string
	Org        string
	Token      string
	Scope      Scope
	Cache      *Cache
	Resolver   *Resolver
	LoginFn    LoginFunc
}

// apiPath builds an org-scoped API path using the client's configured API name
// instead of a hardcoded "nico" segment. resource is everything after the
// API name, e.g. "site", "site/{id}", "vpc/{id}/virtualization".
func apiPath(s *Session, resource string) string {
	return fmt.Sprintf("/v2/org/{org}/%s/%s", s.Client.APIName, resource)
}

// PromptString returns the prompt showing org and current scope.
func (s *Session) PromptString() string {
	parts := []string{s.Org}
	if s.Scope.SiteName != "" {
		parts = append(parts, s.Scope.SiteName)
	}
	if s.Scope.VpcName != "" {
		parts = append(parts, s.Scope.VpcName)
	}
	return Cyan("nico:"+strings.Join(parts, "/")) + "> "
}

// RefreshClient updates the session with a new token.
func (s *Session) RefreshClient(token string) {
	s.Token = token
	s.Client.Token = token
}

// NewSession creates a new interactive session.
func NewSession(client *cli.Client, org, configPath string) *Session {
	cache := NewCache()
	resolver := NewResolver(cache)
	s := &Session{
		Client:     client,
		ConfigPath: configPath,
		Org:        org,
		Cache:      cache,
		Resolver:   resolver,
	}
	s.registerFetchers()
	return s
}

func (s *Session) registerFetchers() {
	s.Resolver.RegisterFetcher("site", s.fetchSites)
	s.Resolver.RegisterFetcher("domain", s.fetchDomains)
	s.Resolver.RegisterFetcher("vpc", s.fetchVPCs)
	s.Resolver.RegisterFetcher("subnet", s.fetchSubnets)
	s.Resolver.RegisterFetcher("instance", s.fetchInstances)
	s.Resolver.RegisterFetcher("operating-system", s.fetchOperatingSystems)
	s.Resolver.RegisterFetcher("machine", s.fetchMachines)
	s.Resolver.RegisterFetcher("dpu-machine", s.fetchDPUMachines)
	s.Resolver.RegisterFetcher("ip-block", s.fetchIPBlocks)
	s.Resolver.RegisterFetcher("network-security-group", s.fetchNSGs)
	s.Resolver.RegisterFetcher("audit", s.fetchAudits)
	s.Resolver.RegisterFetcher("ssh-key", s.fetchSSHKeys)
	s.Resolver.RegisterFetcher("ssh-key-group", s.fetchSSHKeyGroups)
	s.Resolver.RegisterFetcher("sku", s.fetchSKUs)
	s.Resolver.RegisterFetcher("rack", s.fetchRacks)
	s.Resolver.RegisterFetcher("vpc-prefix", s.fetchVPCPrefixes)
	s.Resolver.RegisterFetcher("tenant-account", s.fetchTenantAccounts)
	s.Resolver.RegisterFetcher("allocation", s.fetchAllocations)
	s.Resolver.RegisterFetcher("expected-machine", s.fetchExpectedMachines)
	s.Resolver.RegisterFetcher("expected-rack", s.fetchExpectedRacks)
	s.Resolver.RegisterFetcher("expected-switch", s.fetchExpectedSwitches)
	s.Resolver.RegisterFetcher("expected-power-shelf", s.fetchExpectedPowerShelves)
	s.Resolver.RegisterFetcher("infiniband-partition", s.fetchInfiniBandPartitions)
	s.Resolver.RegisterFetcher("nvlink-logical-partition", s.fetchNVLinkLogicalPartitions)
	s.Resolver.RegisterFetcher("instance-type", s.fetchInstanceTypes)
	s.Resolver.RegisterFetcher("dpu-extension-service", s.fetchDPUExtensionServices)
	s.Resolver.RegisterFetcher("tray", s.fetchTrays)
	s.Resolver.RegisterFetcher("ipxe-template", s.fetchIPXETemplates)
	s.Resolver.RegisterFetcher("rule", s.fetchRules)
	s.Resolver.RegisterFetcher("task-run", s.fetchRuns)
	s.Resolver.RegisterFetcher("vpc-peering", s.fetchVPCPeerings)
	s.Resolver.RegisterFetcher("tenant", s.fetchTenants)
	s.Resolver.RegisterFetcher("tray-component", s.fetchTrayComponents)
}

// fetchAll fetches all pages from a list endpoint and returns raw JSON objects.
func (s *Session) fetchAll(path string, extraQuery map[string]string) ([]map[string]interface{}, error) {
	q := map[string]string{"pageSize": "100"}
	for k, v := range extraQuery {
		q[k] = v
	}
	var all []map[string]interface{}
	for page := 1; page <= 1000; page++ {
		q["pageNumber"] = strconv.Itoa(page)
		body, hdrs, err := s.Client.Do("GET", path, nil, q, nil)
		if err != nil {
			return nil, err
		}
		var items []map[string]interface{}
		if err := json.Unmarshal(body, &items); err != nil {
			return all, nil
		}
		all = append(all, items...)
		if pag := hdrs.Get("X-Pagination"); pag != "" {
			var ph struct {
				Total int `json:"total"`
			}
			if json.Unmarshal([]byte(pag), &ph) == nil && ph.Total > 0 && len(all) >= ph.Total {
				break
			}
		}
		if len(items) < 100 {
			break
		}
	}
	return all, nil
}

// extractLabels pulls a map[string]string from the "labels" field of a raw JSON object.
func extractLabels(m map[string]interface{}) map[string]string {
	raw, ok := m["labels"].(map[string]interface{})
	if !ok || len(raw) == 0 {
		return nil
	}
	labels := make(map[string]string, len(raw))
	for k, v := range raw {
		if s, ok := v.(string); ok {
			labels[k] = s
		}
	}
	return labels
}

// str extracts a string field from a raw JSON object.
func str(m map[string]interface{}, key string) string {
	if v, ok := m[key]; ok {
		if s, ok := v.(string); ok {
			return s
		}
	}
	return ""
}

// buildMachineVPCNames fetches instances and returns a map of machineId → comma-separated VPC names.
func (s *Session) buildMachineVPCNames(ctx context.Context) map[string]string {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	if s.Scope.VpcID != "" {
		q["vpcId"] = s.Scope.VpcID
	}
	instances, err := s.fetchAll(apiPath(s, "instance"), q)
	if err != nil {
		return map[string]string{}
	}

	vpcSetByMachine := make(map[string]map[string]struct{})
	for _, inst := range instances {
		machineID := strings.TrimSpace(str(inst, "machineId"))
		vpcID := strings.TrimSpace(str(inst, "vpcId"))
		if machineID == "" || vpcID == "" {
			continue
		}
		if vpcSetByMachine[machineID] == nil {
			vpcSetByMachine[machineID] = make(map[string]struct{})
		}
		vpcSetByMachine[machineID][vpcID] = struct{}{}
	}

	result := make(map[string]string, len(vpcSetByMachine))
	for machineID, vpcIDs := range vpcSetByMachine {
		names := make([]string, 0, len(vpcIDs))
		for vpcID := range vpcIDs {
			name := strings.TrimSpace(s.Resolver.ResolveID("vpc", vpcID))
			if name == "" {
				name = vpcID
			}
			names = append(names, name)
		}
		result[machineID] = strings.Join(names, ",")
	}
	return result
}

// getTenantID returns the current tenant ID, caching it for the session.
func (s *Session) getTenantID(_ context.Context) (string, error) {
	if cached := s.Cache.LookupByName("_tenant", s.Org); cached != nil {
		return cached.ID, nil
	}
	body, _, err := s.Client.Do("GET", apiPath(s, "tenant/current"), nil, nil, nil)
	if err != nil {
		return "", fmt.Errorf("fetching tenant: %w", err)
	}
	var t map[string]interface{}
	if err := json.Unmarshal(body, &t); err != nil {
		return "", fmt.Errorf("parsing tenant: %w", err)
	}
	id := str(t, "id")
	if id == "" {
		return "", fmt.Errorf("tenant has no id")
	}
	s.Cache.Set("_tenant", []NamedItem{{Name: s.Org, ID: id}})
	return id, nil
}

// tenantHasTargetedInstanceCreationAtSite resolves the current Tenant's
// effective targeted-instance-creation capability for one Site. The API
// enforces this capability using a Ready Tenant Account for the Site's
// Infrastructure Provider plus any site-specific override, so the TUI mirrors
// that resolution before offering specific Machines.
func (s *Session) tenantHasTargetedInstanceCreationAtSite(ctx context.Context, siteID string) (bool, error) {
	isSiteContext := s.Scope.VpcID != ""
	if siteID == "" {
		message := "site ID is missing"
		if isSiteContext {
			message = "selected VPC has no site ID"
		}
		return false, errors.New(message)
	}

	tenantID, err := s.getTenantID(ctx)
	if err != nil {
		return false, err
	}

	body, _, err := s.Client.Do(
		"GET",
		apiPath(s, "site/{id}"),
		map[string]string{
			"id": siteID,
		},
		nil,
		nil,
	)
	var siteString string
	if isSiteContext {
		siteString = "selected VPC site"
	} else {
		siteString = "site"
	}
	if err != nil {
		return false, fmt.Errorf("fetching %s: %w", siteString, err)
	}
	var site map[string]interface{}
	err = json.Unmarshal(body, &site)
	if err != nil {
		return false, fmt.Errorf("parsing %s: %w", siteString, err)
	}
	providerID := str(site, "infrastructureProviderId")
	if providerID == "" {
		return false, fmt.Errorf("%s has no infrastructure provider ID", siteString)
	}

	accounts, err := s.fetchAll(
		apiPath(s, "tenant/account"),
		map[string]string{
			"infrastructureProviderId": providerID,
			"tenantId":                 tenantID,
		},
	)
	if err != nil {
		return false, fmt.Errorf("fetching tenant accounts for %s: %w", siteString, err)
	}
	for _, account := range accounts {
		status := str(account, "status")
		if !strings.EqualFold(status, "Ready") {
			continue
		}
		accountProviderID := str(account, "infrastructureProviderId")
		if accountProviderID != providerID {
			continue
		}
		accountTenantID := str(account, "tenantId")
		if accountTenantID != tenantID {
			continue
		}

		rawCapabilities, ok := account["siteCapabilities"].([]interface{})
		if !ok {
			return false, nil
		}

		defaultEnabled := false
		for _, rawCapability := range rawCapabilities {
			capability, ok := rawCapability.(map[string]interface{})
			if !ok {
				continue
			}
			enabled, ok := capability["targetedInstanceCreation"].(bool)
			if !ok {
				continue
			}
			siteIDs := stringSlice(capability["siteIds"])
			if len(siteIDs) == 0 {
				defaultEnabled = enabled
				continue
			}
			for _, capabilitySiteID := range siteIDs {
				if capabilitySiteID == siteID {
					return enabled, nil
				}
			}
		}
		return defaultEnabled, nil
	}
	return false, nil
}

// getInfrastructureProviderID returns the current infrastructure provider ID, caching it for the session.
func (s *Session) getInfrastructureProviderID(_ context.Context) (string, error) {
	if cached := s.Cache.LookupByName("_infra_provider", s.Org); cached != nil {
		return cached.ID, nil
	}
	body, _, err := s.Client.Do("GET", apiPath(s, "infrastructure-provider/current"), nil, nil, nil)
	if err != nil {
		return "", fmt.Errorf("fetching infrastructure provider: %w", err)
	}
	var p map[string]interface{}
	if err := json.Unmarshal(body, &p); err != nil {
		return "", fmt.Errorf("parsing infrastructure provider: %w", err)
	}
	id := str(p, "id")
	if id == "" {
		return "", fmt.Errorf("infrastructure provider has no id")
	}
	s.Cache.Set("_infra_provider", []NamedItem{{Name: s.Org, ID: id}})
	return id, nil
}

// addOwnerScopeFilter adds either an `infrastructureProviderId` or a
// `tenantId` query parameter to q so that list endpoints which require one
// of the two (when the caller holds both Provider Admin and Tenant Admin
// roles) succeed instead of returning 400. Provider scope is preferred
// because resources offered to allocation-create constraints (IP blocks,
// instance types) are owned by the provider; the tenant view of the same
// resources is what the provider has already allocated, which is not what
// the picker wants.
//
// Either filter is skipped silently if the caller already supplied one in
// q, or if both lookups fail (single-role users for whom neither identity
// resolves). The latter case preserves the legacy behavior where the API
// happily lists with no owner filter.
func addOwnerScopeFilter(s *Session, ctx context.Context, q map[string]string) {
	if _, hasProv := q["infrastructureProviderId"]; hasProv {
		return
	}
	if _, hasTen := q["tenantId"]; hasTen {
		return
	}
	if id, err := s.getInfrastructureProviderID(ctx); err == nil && id != "" {
		q["infrastructureProviderId"] = id
		return
	}
	if id, err := s.getTenantID(ctx); err == nil && id != "" {
		q["tenantId"] = id
	}
}

// -- Fetchers --

func (s *Session) fetchSites(_ context.Context) ([]NamedItem, error) {
	items, err := s.fetchAll(apiPath(s, "site"), nil)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"), Raw: m}
	}
	return result, nil
}

func (s *Session) fetchDomains(ctx context.Context) ([]NamedItem, error) {
	tenantID, err := s.getTenantID(ctx)
	if err != nil {
		return nil, err
	}
	query := map[string]string{"tenantId": tenantID}
	if s.Scope.SiteID != "" {
		query["siteId"] = s.Scope.SiteID
	}
	domains, err := s.fetchAll(apiPath(s, "domain"), query)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(domains))
	for i, domain := range domains {
		if str(domain, "tenantId") != tenantID {
			return nil, fmt.Errorf("domain %q is not owned by the current tenant", str(domain, "id"))
		}
		result[i] = NamedItem{
			Name: str(domain, "name"),
			ID:   str(domain, "id"),
			Extra: map[string]string{
				"siteId":   str(domain, "siteId"),
				"tenantId": str(domain, "tenantId"),
			},
			Raw: domain,
		}
	}
	return result, nil
}

func (s *Session) fetchVPCs(ctx context.Context) ([]NamedItem, error) {
	tenantID, err := s.getTenantID(ctx)
	if err != nil {
		return nil, err
	}
	q := map[string]string{"tenantId": tenantID}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "vpc"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		if str(m, "tenantId") != tenantID {
			return nil, fmt.Errorf("vpc %q is not owned by the current tenant", str(m, "id"))
		}
		result[i] = NamedItem{
			Name:   str(m, "name"),
			ID:     str(m, "id"),
			Status: str(m, "status"),
			Labels: extractLabels(m),
			Extra: map[string]string{
				"networkVirtualizationType": str(m, "networkVirtualizationType"),
				"siteId":                    str(m, "siteId"),
				"tenantId":                  str(m, "tenantId"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchSubnets(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{"includeUsageStats": "true"}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	if s.Scope.VpcID != "" {
		q["vpcId"] = s.Scope.VpcID
	}
	items, err := s.fetchAll(apiPath(s, "subnet"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		usageStats, _ := m["usageStats"].(map[string]interface{})
		acquiredIPs, _ := usageStats["acquiredIPs"].(float64)
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{
				"vpcId":       str(m, "vpcId"),
				"siteId":      str(m, "siteId"),
				"subdomainId": str(m, "subdomainId"),
				"acquiredIPs": strconv.FormatFloat(acquiredIPs, 'f', -1, 64),
			}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchInstances(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	if s.Scope.VpcID != "" {
		q["vpcId"] = s.Scope.VpcID
	}
	items, err := s.fetchAll(apiPath(s, "instance"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Labels: extractLabels(m),
			Extra:  map[string]string{"vpcId": str(m, "vpcId"), "siteId": str(m, "siteId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchMachines(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "machine"), q)
	if err != nil {
		return nil, err
	}
	if s.Scope.VpcID != "" {
		instanceQuery := map[string]string{"vpcId": s.Scope.VpcID}
		if s.Scope.SiteID != "" {
			instanceQuery["siteId"] = s.Scope.SiteID
		}
		instances, err := s.fetchAll(apiPath(s, "instance"), instanceQuery)
		if err != nil {
			return nil, err
		}
		allowedMachineIDs := make(map[string]struct{}, len(instances))
		for _, inst := range instances {
			if machineID := str(inst, "machineId"); machineID != "" {
				allowedMachineIDs[machineID] = struct{}{}
			}
		}
		filtered := make([]map[string]interface{}, 0, len(items))
		for _, m := range items {
			if _, ok := allowedMachineIDs[str(m, "id")]; ok {
				filtered = append(filtered, m)
			}
		}
		items = filtered
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := machineDisplayName(m)
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: str(m, "status"),
			Labels: extractLabels(m),
			Extra:  map[string]string{"siteId": str(m, "siteId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchDPUMachines(_ context.Context) ([]NamedItem, error) {
	siteID := strings.TrimSpace(s.Scope.SiteID)
	if siteID == "" {
		return nil, fmt.Errorf("select a site before resolving a DPU Machine")
	}
	items, err := s.fetchAll(apiPath(s, "dpu"), map[string]string{"siteId": siteID})
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: machineDisplayName(m), ID: str(m, "id"), Status: str(m, "state"),
			Labels: extractLabels(m),
			Extra: map[string]string{
				"siteId":        str(m, "siteId"),
				"hostMachineId": str(m, "hostMachineId"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func machineDisplayName(m map[string]interface{}) string {
	if labels, ok := m["labels"].(map[string]interface{}); ok {
		for _, key := range []string{"ServerName", "serverName", "hostname", "hostName"} {
			if v, ok := labels[key].(string); ok && strings.TrimSpace(v) != "" {
				return strings.TrimSpace(v)
			}
		}
	}
	if sn, ok := m["serialNumber"].(string); ok && strings.TrimSpace(sn) != "" {
		return strings.TrimSpace(sn)
	}
	if id := str(m, "id"); id != "" {
		return id
	}
	return "<unknown>"
}

func (s *Session) fetchOperatingSystems(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "operating-system"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"), Raw: m}
	}
	return result, nil
}

func (s *Session) fetchSSHKeyGroups(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "sshkeygroup"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"), Raw: m}
	}
	return result, nil
}

func (s *Session) fetchAllocations(ctx context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	addOwnerScopeFilter(s, ctx, q)
	items, err := s.fetchAll(apiPath(s, "allocation"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{"siteId": str(m, "siteId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchIPBlocks(ctx context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	addOwnerScopeFilter(s, ctx, q)
	items, err := s.fetchAll(apiPath(s, "ipblock"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{"siteId": str(m, "siteId"), "tenantId": str(m, "tenantId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchTenantIPBlocks(ctx context.Context) ([]NamedItem, string, error) {
	tenantID, err := s.getTenantID(ctx)
	if err != nil {
		return nil, "", err
	}
	q := map[string]string{"tenantId": tenantID}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "ipblock"), q)
	if err != nil {
		return nil, "", err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{
				"siteId":          str(m, "siteId"),
				"tenantId":        str(m, "tenantId"),
				"protocolVersion": str(m, "protocolVersion"),
			},
			Raw: m,
		}
	}
	return result, tenantID, nil
}

func (s *Session) fetchNSGs(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "network-security-group"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Labels: extractLabels(m), Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchAudits(_ context.Context) ([]NamedItem, error) {
	items, err := s.fetchAll(apiPath(s, "audit"), nil)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		method := str(m, "method")
		endpoint := str(m, "endpoint")
		if method == "" {
			method = "AUDIT"
		}
		name := strings.TrimSpace(method + " " + endpoint)
		if name == "" {
			name = str(m, "id")
		}
		statusCode := ""
		if sc, ok := m["statusCode"].(float64); ok {
			statusCode = strconv.Itoa(int(sc))
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: statusCode,
			Extra: map[string]string{"method": method, "endpoint": endpoint}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchSSHKeys(_ context.Context) ([]NamedItem, error) {
	items, err := s.fetchAll(apiPath(s, "sshkey"), nil)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name:  str(m, "name"),
			ID:    str(m, "id"),
			Extra: map[string]string{"fingerprint": str(m, "fingerprint")},
			Raw:   m,
		}
	}
	return result, nil
}

func (s *Session) fetchSKUs(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "sku"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		deviceType := str(m, "deviceType")
		name := deviceType
		if strings.TrimSpace(name) == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name:  name,
			ID:    str(m, "id"),
			Extra: map[string]string{"siteId": str(m, "siteId"), "deviceType": deviceType},
			Raw:   m,
		}
	}
	return result, nil
}

func (s *Session) fetchRacks(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "rack"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"),
			Extra: map[string]string{"manufacturer": str(m, "manufacturer"), "model": str(m, "model")},
			Raw:   m,
		}
	}
	return result, nil
}

func (s *Session) fetchVPCPrefixes(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	if s.Scope.VpcID != "" {
		q["vpcId"] = s.Scope.VpcID
	}
	items, err := s.fetchAll(apiPath(s, "vpc-prefix"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{"vpcId": str(m, "vpcId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchTenantAccounts(ctx context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if id, err := s.getInfrastructureProviderID(ctx); err == nil {
		q["infrastructureProviderId"] = id
	} else {
		tenantID, tenantErr := s.getTenantID(ctx)
		if tenantErr != nil {
			return nil, fmt.Errorf(
				"resolving tenant-account scope: infrastructure provider: %v; tenant: %w",
				err, tenantErr,
			)
		}
		q["tenantId"] = tenantID
	}
	items, err := s.fetchAll(apiPath(s, "tenant/account"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		tenantOrg := str(m, "tenantOrg")
		name := strings.TrimSpace(tenantOrg)
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{
				"infrastructureProviderId": str(m, "infrastructureProviderId"),
				"tenantId":                 str(m, "tenantId"),
				"tenantOrg":                str(m, "tenantOrg"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchExpectedMachines(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "expected-machine"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "bmcMacAddress"))
		if name == "" {
			name = strings.TrimSpace(str(m, "chassisSerialNumber"))
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"),
			Labels: extractLabels(m),
			Extra: map[string]string{
				"siteId":              str(m, "siteId"),
				"bmcMacAddress":       str(m, "bmcMacAddress"),
				"chassisSerialNumber": str(m, "chassisSerialNumber"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchExpectedRacks(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "expected-rack"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = strings.TrimSpace(str(m, "rackId"))
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"),
			Labels: extractLabels(m),
			Extra: map[string]string{
				"siteId":        str(m, "siteId"),
				"rackId":        str(m, "rackId"),
				"rackProfileId": str(m, "rackProfileId"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchExpectedSwitches(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "expected-switch"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = strings.TrimSpace(str(m, "switchSerialNumber"))
		}
		if name == "" {
			name = strings.TrimSpace(str(m, "bmcMacAddress"))
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"),
			Labels: extractLabels(m),
			Extra: map[string]string{
				"siteId":             str(m, "siteId"),
				"bmcMacAddress":      str(m, "bmcMacAddress"),
				"switchSerialNumber": str(m, "switchSerialNumber"),
				"rackId":             str(m, "rackId"),
				"manufacturer":       str(m, "manufacturer"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchExpectedPowerShelves(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "expected-power-shelf"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = strings.TrimSpace(str(m, "shelfSerialNumber"))
		}
		if name == "" {
			name = strings.TrimSpace(str(m, "bmcMacAddress"))
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"),
			Labels: extractLabels(m),
			Extra: map[string]string{
				"siteId":            str(m, "siteId"),
				"bmcMacAddress":     str(m, "bmcMacAddress"),
				"shelfSerialNumber": str(m, "shelfSerialNumber"),
				"rackId":            str(m, "rackId"),
				"manufacturer":      str(m, "manufacturer"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchInfiniBandPartitions(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "infiniband-partition"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Labels: extractLabels(m),
			Extra:  map[string]string{"siteId": str(m, "siteId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchNVLinkLogicalPartitions(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "nvlink-logical-partition"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{"siteId": str(m, "siteId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchInstanceTypes(ctx context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	addOwnerScopeFilter(s, ctx, q)
	items, err := s.fetchAll(apiPath(s, "instance/type"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"), Status: str(m, "status"),
			Labels: extractLabels(m),
			Extra:  map[string]string{"siteId": str(m, "siteId")}, Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchTrays(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "tray"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = strings.TrimSpace(str(m, "serialNumber"))
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: str(m, "powerState"),
			Extra: map[string]string{
				"componentId":     str(m, "componentId"),
				"type":            str(m, "type"),
				"manufacturer":    str(m, "manufacturer"),
				"model":           str(m, "model"),
				"serialNumber":    str(m, "serialNumber"),
				"firmwareVersion": str(m, "firmwareVersion"),
				"rackId":          str(m, "rackId"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchDPUExtensionServices(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "dpu-extension-service"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		result[i] = NamedItem{
			Name: str(m, "name"), ID: str(m, "id"),
			Extra: map[string]string{"siteId": str(m, "siteId"), "serviceType": str(m, "serviceType")},
			Raw:   m,
		}
	}
	return result, nil
}

func (s *Session) fetchIPXETemplates(_ context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	items, err := s.fetchAll(apiPath(s, "ipxe-template"), q)
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: str(m, "visibility"),
			Extra: map[string]string{"visibility": str(m, "visibility")},
			Raw:   m,
		}
	}
	return result, nil
}

func (s *Session) fetchRules(ctx context.Context) ([]NamedItem, error) {
	siteID := strings.TrimSpace(s.Scope.SiteID)
	if siteID == "" {
		return nil, fmt.Errorf("select a site before resolving an operation rule")
	}
	return s.fetchRulesForSite(ctx, siteID)
}

func (s *Session) fetchRulesForSite(_ context.Context, siteID string) ([]NamedItem, error) {
	items, err := s.fetchAll(apiPath(s, "task/rule"), map[string]string{"siteId": siteID})
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = strings.Trim(strings.Join([]string{str(m, "operationType"), str(m, "operationCode")}, " / "), " /")
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"),
			Extra: map[string]string{
				"siteId":        siteID,
				"operationType": str(m, "operationType"),
				"operationCode": str(m, "operationCode"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchRuns(ctx context.Context) ([]NamedItem, error) {
	siteID := strings.TrimSpace(s.Scope.SiteID)
	if siteID == "" {
		return nil, fmt.Errorf("select a site before resolving a run")
	}
	return s.fetchRunsForSite(ctx, siteID)
}

func (s *Session) fetchRunsForSite(_ context.Context, siteID string) ([]NamedItem, error) {
	items, err := s.fetchAll(apiPath(s, "task/run"), map[string]string{"siteId": siteID})
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, len(items))
	for i, m := range items {
		name := strings.TrimSpace(str(m, "name"))
		if name == "" {
			name = strings.Trim(strings.Join([]string{str(m, "operationType"), str(m, "operationCode")}, " / "), " /")
		}
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{
				"siteId":        siteID,
				"operationType": str(m, "operationType"),
				"operationCode": str(m, "operationCode"),
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchVPCPeerings(ctx context.Context) ([]NamedItem, error) {
	q := map[string]string{}
	if s.Scope.SiteID != "" {
		q["siteId"] = s.Scope.SiteID
	}
	if s.Scope.VpcID != "" {
		q["vpcId"] = s.Scope.VpcID
	}
	items, err := s.fetchAll(apiPath(s, "vpc-peering"), q)
	if err != nil {
		return nil, err
	}

	vpcNames := map[string]string{}
	if vpcs, fetchErr := s.Resolver.Fetch(ctx, "vpc"); fetchErr == nil {
		for _, vpc := range vpcs {
			vpcNames[vpc.ID] = vpc.Name
		}
	}

	result := make([]NamedItem, len(items))
	for i, m := range items {
		vpc1ID := str(m, "vpc1Id")
		vpc2ID := str(m, "vpc2Id")
		vpc1Name := nestedString(m, "vpc1", "name")
		if vpc1Name == "" {
			vpc1Name = strings.TrimSpace(vpcNames[vpc1ID])
		}
		if vpc1Name == "" {
			vpc1Name = vpc1ID
		}
		vpc2Name := nestedString(m, "vpc2", "name")
		if vpc2Name == "" {
			vpc2Name = strings.TrimSpace(vpcNames[vpc2ID])
		}
		if vpc2Name == "" {
			vpc2Name = vpc2ID
		}
		name := strings.Trim(strings.Join([]string{vpc1Name, vpc2Name}, " <-> "), " <>-")
		if name == "" {
			name = str(m, "id")
		}
		result[i] = NamedItem{
			Name: name, ID: str(m, "id"), Status: str(m, "status"),
			Extra: map[string]string{
				"siteId": str(m, "siteId"),
				"vpc1Id": vpc1ID,
				"vpc2Id": vpc2ID,
			},
			Raw: m,
		}
	}
	return result, nil
}

func (s *Session) fetchTenants(ctx context.Context) ([]NamedItem, error) {
	result := []NamedItem{}
	seen := map[string]struct{}{}
	add := func(name, id, status string, raw interface{}) {
		id = strings.TrimSpace(id)
		if id == "" {
			return
		}
		if _, ok := seen[id]; ok {
			return
		}
		name = strings.TrimSpace(name)
		if name == "" {
			name = id
		}
		seen[id] = struct{}{}
		result = append(result, NamedItem{Name: name, ID: id, Status: status, Raw: raw})
	}

	accounts, accountsErr := s.fetchTenantAccounts(ctx)
	if accountsErr == nil {
		for _, account := range accounts {
			add(account.Extra["tenantOrg"], account.Extra["tenantId"], account.Status, account.Raw)
		}
	}

	currentTenantID, currentErr := s.getTenantID(ctx)
	if currentErr == nil {
		add(s.Org, currentTenantID, "", nil)
	}
	if len(result) == 0 && accountsErr != nil && currentErr != nil {
		return nil, fmt.Errorf("fetching tenants: tenant accounts: %v; current tenant: %w", accountsErr, currentErr)
	}
	return result, nil
}

func (s *Session) fetchTrayComponents(ctx context.Context) ([]NamedItem, error) {
	trays, err := s.Resolver.Fetch(ctx, "tray")
	if err != nil {
		return nil, err
	}
	result := make([]NamedItem, 0, len(trays))
	for _, tray := range trays {
		componentID := strings.TrimSpace(tray.Extra["componentId"])
		if componentID == "" {
			continue
		}
		name := strings.TrimSpace(tray.Name)
		if name == "" {
			name = componentID
		}
		result = append(result, NamedItem{
			Name:   name,
			ID:     componentID,
			Status: tray.Extra["type"],
			Extra: map[string]string{
				"trayId": tray.ID,
				"rackId": tray.Extra["rackId"],
				"type":   tray.Extra["type"],
			},
			Raw: tray.Raw,
		})
	}
	return result, nil
}
