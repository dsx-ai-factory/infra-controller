// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"cmp"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"sort"
	"strconv"
	"strings"
	"text/tabwriter"

	cli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/vpcprefix"
)

// Command represents a registered interactive command.
type Command struct {
	Name        string
	Description string
	Sensitive   bool
	Run         func(s *Session, args []string) error
}

// AllCommands returns all available commands.
func AllCommands() []Command {
	commands := []Command{
		{Name: "site list", Description: "List all sites", Run: cmdSiteList},
		{Name: "site get", Description: "Get site details", Run: cmdSiteGet},
		{Name: "site create", Description: "Create a site", Run: cmdSiteCreate},
		{Name: "site update", Description: "Update a site", Run: cmdSiteUpdate},
		{Name: "site delete", Description: "Delete a site", Run: cmdSiteDelete},

		{Name: "vpc list", Description: "List all VPCs", Run: cmdVPCList},
		{Name: "vpc get", Description: "Get VPC details", Run: cmdVPCGet},
		{Name: "vpc create", Description: "Create a VPC", Run: cmdVPCCreate},
		{Name: "vpc update", Description: "Update a VPC", Run: cmdVPCUpdate},
		{Name: "vpc virtualization update", Description: "Update VPC virtualization", Run: cmdVPCVirtualizationUpdate},
		{Name: "vpc delete", Description: "Delete a VPC", Run: cmdVPCDelete},
		{Name: "vpc-peering create", Description: "Create VPC peerings", Run: cmdVPCPeeringCreate},

		{Name: "subnet list", Description: "List all subnets", Run: cmdSubnetList},
		{Name: "subnet get", Description: "Get subnet details", Run: cmdSubnetGet},
		{Name: "subnet create", Description: "Create an IPv4 Subnet in an Ethernet virtualizer VPC", Run: cmdSubnetCreate},
		{Name: "subnet attach-vpc", Description: "Attach a subnet to a VPC", Run: cmdSubnetAttachVPC},
		{Name: "subnet attach-vpc-to attach-vpc-to-subnet", Description: "Attach a subnet to a VPC", Run: cmdSubnetAttachVPC},
		{Name: "subnet update", Description: "Update a subnet", Run: cmdSubnetUpdate},
		{Name: "subnet delete", Description: "Delete a subnet", Run: cmdSubnetDelete},

		{Name: "instance-type list", Description: "List instance types", Run: cmdInstanceTypeList},
		{Name: "instance-type get", Description: "Get instance type details", Run: cmdInstanceTypeGet},

		{Name: "instance list", Description: "List all instances", Run: cmdInstanceList},
		{Name: "instance get", Description: "Get instance details", Run: cmdInstanceGet},
		{Name: "instance create", Description: "Create an instance on a machine", Run: cmdInstanceCreate},
		{Name: "instance update", Description: "Update an instance (rename, change OS, rotate ssh key groups)", Run: cmdInstanceUpdate},
		{Name: "instance reboot", Description: "Reboot an instance, optionally with custom iPXE / pending updates", Run: cmdInstanceReboot},
		{Name: "instance delete", Description: "Delete an instance", Run: cmdInstanceDelete},

		{Name: "machine list", Description: "List machines", Run: cmdMachineList},
		{Name: "machine get", Description: "Get machine details", Run: cmdMachineGet},
		{Name: "machine dpu get", Description: "Get DPU machines attached to a host machine", Run: cmdMachineDpuGet},

		{Name: "operating-system list", Description: "List operating systems", Run: cmdOSList},
		{Name: "operating-system get", Description: "Get operating system details", Run: cmdOSGet},
		{Name: "operating-system create", Description: "Create an operating system", Run: cmdOSCreate},
		{Name: "operating-system update", Description: "Update an operating system", Run: cmdOSUpdate},
		{Name: "operating-system delete", Description: "Delete an operating system", Run: cmdOSDelete},

		{Name: "ssh-key-group list", Description: "List SSH key groups", Run: cmdSSHKeyGroupList},
		{Name: "ssh-key-group get", Description: "Get SSH key group details", Run: cmdSSHKeyGroupGet},
		{Name: "ssh-key-group create", Description: "Create an SSH key group", Run: cmdSSHKeyGroupCreate},
		{Name: "ssh-key-group update", Description: "Update an SSH key group", Run: cmdSSHKeyGroupUpdate},
		{Name: "ssh-key-group delete", Description: "Delete an SSH key group", Run: cmdSSHKeyGroupDelete},

		{Name: "ssh-key list", Description: "List SSH keys", Run: cmdSSHKeyList},
		{Name: "ssh-key get", Description: "Get SSH key details", Run: cmdSSHKeyGet},
		{Name: "ssh-key create", Description: "Create an SSH key", Run: cmdSSHKeyCreate},
		{Name: "ssh-key update", Description: "Update an SSH key", Run: cmdSSHKeyUpdate},
		{Name: "ssh-key delete", Description: "Delete an SSH key", Run: cmdSSHKeyDelete},

		{Name: "allocation list", Description: "List allocations", Run: cmdAllocationList},
		{Name: "allocation get", Description: "Get allocation details", Run: cmdAllocationGet},
		{Name: "allocation create", Description: "Create an allocation", Run: cmdAllocationCreate},
		{Name: "allocation update", Description: "Update an allocation", Run: cmdAllocationUpdate},
		{Name: "allocation delete", Description: "Delete an allocation", Run: cmdAllocationDelete},

		{Name: "ip-block list", Description: "List IP blocks", Run: cmdIPBlockList},
		{Name: "ip-block get", Description: "Get IP block details", Run: cmdIPBlockGet},
		{Name: "ip-block create", Description: "Create an IP block", Run: cmdIPBlockCreate},
		{Name: "ip-block update", Description: "Update an IP block", Run: cmdIPBlockUpdate},
		{Name: "ip-block delete", Description: "Delete an IP block", Run: cmdIPBlockDelete},

		{Name: "network-security-group list", Description: "List network security groups", Run: cmdNSGList},
		{Name: "network-security-group get", Description: "Get network security group details", Run: cmdNSGGet},
		{Name: "network-security-group create", Description: "Create a network security group", Run: cmdNSGCreate},
		{Name: "network-security-group update", Description: "Update a network security group", Run: cmdNSGUpdate},
		{Name: "network-security-group delete", Description: "Delete a network security group", Run: cmdNSGDelete},

		{Name: "sku list", Description: "List SKUs", Run: cmdSKUList},
		{Name: "sku get", Description: "Get SKU details", Run: cmdSKUGet},

		{Name: "rack list", Description: "List racks", Run: cmdRackList},
		{Name: "rack get", Description: "Get rack details", Run: cmdRackGet},
		{Name: "rack bringup", Description: "Bring up a rack", Run: cmdRackBringup},
		{Name: "rack power", Description: "Power control a rack", Run: cmdRackPower},
		{Name: "rack firmware", Description: "Firmware update a rack", Run: cmdRackFirmware},
		{Name: "rack validate", Description: "Validate a rack against expected inventory", Run: cmdRackValidate},
		{Name: "rack task get", Description: "Get rack/tray task status", Run: cmdRackTaskGet},
		{Name: "rack task cancel", Description: "Cancel a rack/tray task", Run: cmdRackTaskCancel},

		{Name: "tray list", Description: "List trays", Run: cmdTrayList},
		{Name: "tray get", Description: "Get tray details", Run: cmdTrayGet},
		{Name: "tray power", Description: "Power control a tray", Run: cmdTrayPower},
		{Name: "tray firmware", Description: "Firmware update a tray", Run: cmdTrayFirmware},
		{Name: "tray validate", Description: "Validate a tray against expected inventory", Run: cmdTrayValidate},

		{Name: "vpc-prefix list", Description: "List VPC prefixes", Run: cmdVPCPrefixList},
		{Name: "vpc-prefix get", Description: "Get VPC prefix details", Run: cmdVPCPrefixGet},
		{Name: "vpc-prefix create", Description: "Create a VPC prefix", Run: cmdVPCPrefixCreate},
		{Name: "vpc-prefix update", Description: "Update a VPC prefix", Run: cmdVPCPrefixUpdate},
		{Name: "vpc-prefix delete", Description: "Delete a VPC prefix", Run: cmdVPCPrefixDelete},

		{Name: "tenant-account list", Description: "List tenant accounts", Run: cmdTenantAccountList},
		{Name: "tenant-account get", Description: "Get tenant account details", Run: cmdTenantAccountGet},
		{Name: "tenant-account create", Description: "Create a tenant account", Run: cmdTenantAccountCreate},
		{Name: "tenant-account update", Description: "Update a tenant account", Run: cmdTenantAccountUpdate},
		{Name: "tenant-account delete", Description: "Delete a tenant account", Run: cmdTenantAccountDelete},

		{Name: "expected-machine list", Description: "List expected machines", Run: cmdExpectedMachineList},
		{Name: "expected-machine get", Description: "Get expected machine details", Run: cmdExpectedMachineGet},

		{Name: "expected-rack list", Description: "List expected racks", Run: cmdExpectedRackList},
		{Name: "expected-rack get", Description: "Get expected rack details", Run: cmdExpectedRackGet},

		{Name: "expected-switch list", Description: "List expected switches", Run: cmdExpectedSwitchList},
		{Name: "expected-switch get", Description: "Get expected switch details", Run: cmdExpectedSwitchGet},

		{Name: "expected-power-shelf list", Description: "List expected power shelves", Run: cmdExpectedPowerShelfList},
		{Name: "expected-power-shelf get", Description: "Get expected power shelf details", Run: cmdExpectedPowerShelfGet},

		{Name: "infiniband-partition list", Description: "List InfiniBand partitions", Run: cmdInfiniBandPartitionList},
		{Name: "infiniband-partition get", Description: "Get InfiniBand partition details", Run: cmdInfiniBandPartitionGet},

		{Name: "nvlink-logical-partition list", Description: "List NVLink logical partitions", Run: cmdNVLinkLogicalPartitionList},
		{Name: "nvlink-logical-partition get", Description: "Get NVLink logical partition details", Run: cmdNVLinkLogicalPartitionGet},

		{Name: "dpu-extension-service list", Description: "List DPU extension services", Run: cmdDPUExtensionServiceList},
		{Name: "dpu-extension-service get", Description: "Get DPU extension service details", Run: cmdDPUExtensionServiceGet},

		{Name: "audit list", Description: "List audit log entries", Run: cmdAuditList},
		{Name: "audit get", Description: "Get audit log entry details", Run: cmdAuditGet},

		{Name: "tenant-identity spiffe-jwks get", Description: "Fetch SPIFFE JWKS (.well-known/spiffe/jwks.json)", Run: cmdTenantIdentityWellKnownSpiffeJWKS},
		{Name: "tenant-identity jwks get", Description: "Fetch OIDC JWKS (.well-known/jwks.json)", Run: cmdTenantIdentityWellKnownJWKS},
		{Name: "tenant-identity openid-configuration get", Description: "Fetch OIDC discovery document (.well-known/openid-configuration)", Run: cmdTenantIdentityWellKnownOpenID},
		{Name: "tenant-identity token-delegation delete", Description: "Delete RFC 8693 token delegation settings", Run: cmdTenantIdentityTokenDelegationDelete},
		{Name: "tenant-identity token-delegation update", Description: "Create or update token delegation settings", Run: cmdTenantIdentityTokenDelegationUpdate},
		{Name: "tenant-identity token-delegation get", Description: "Get RFC 8693 token delegation settings", Run: cmdTenantIdentityTokenDelegationGet},
		{Name: "tenant-identity delete", Description: "Delete tenant identity configuration for a site", Run: cmdTenantIdentityConfigDelete},
		{Name: "tenant-identity update", Description: "Create or update tenant identity configuration", Run: cmdTenantIdentityConfigUpdate},
		{Name: "tenant-identity get", Description: "Get tenant identity configuration for a site", Run: cmdTenantIdentityConfigGet},

		{Name: "metadata get", Description: "Get API metadata", Run: cmdMetadataGet},
		{Name: "user current", Description: "Get current user", Run: cmdUserCurrent},
		{Name: "tenant current", Description: "Get current tenant", Run: cmdTenantCurrent},
		{Name: "tenant stats", Description: "Get tenant stats", Run: cmdTenantStats},
		{Name: "infrastructure-provider current", Description: "Get current infrastructure provider", Run: cmdInfraProviderCurrent},
		{Name: "infrastructure-provider stats", Description: "Get infrastructure provider stats", Run: cmdInfraProviderStats},

		{Name: "service-account current", Description: "Get current service account status", Run: cmdServiceAccountCurrent},

		{Name: "login", Description: "Login / refresh auth token", Run: cmdLogin},
		{Name: "env", Description: "Show NICO_* environment variables in use", Run: cmdEnv},
		{Name: "help", Description: "Show available commands", Run: cmdHelp},
	}
	return appendGeneratedCommands(commands)
}

// LogCmd prints the equivalent cli one-liner for reference.
func LogCmd(s *Session, parts ...string) {
	cmdParts := []string{"cli"}
	if s != nil && strings.TrimSpace(s.ConfigPath) != "" {
		cmdParts = append(cmdParts, "--config", s.ConfigPath)
	}
	cmdParts = append(cmdParts, appendScopeFlags(s, parts)...)
	fmt.Printf("%s %s\n", Dim("INFO:"), strings.Join(cmdParts, " "))
}

func shellQuoteCLIArg(value string) string {
	return "'" + strings.ReplaceAll(value, "'", "'\\''") + "'"
}

func appendScopeFlags(s *Session, parts []string) []string {
	out := append([]string(nil), parts...)
	if s == nil || len(parts) < 2 {
		return out
	}
	resource := strings.TrimSpace(parts[0])
	action := strings.TrimSpace(parts[1])
	if action != "list" {
		return out
	}
	scopeSiteID := strings.TrimSpace(s.Scope.SiteID)
	scopeVpcID := strings.TrimSpace(s.Scope.VpcID)
	switch resource {
	case "vpc", "domain", "allocation", "ip-block", "operating-system", "ssh-key-group",
		"network-security-group", "sku", "rack", "expected-machine", "instance-type",
		"expected-rack", "expected-switch", "expected-power-shelf", "tray",
		"dpu-machine", "dpu-extension-service", "infiniband-partition", "nvlink-logical-partition":
		if scopeSiteID != "" {
			out = append(out, "--site-id", scopeSiteID)
		}
	case "subnet", "vpc-prefix", "instance", "machine":
		if scopeSiteID != "" {
			out = append(out, "--site-id", scopeSiteID)
		}
		if scopeVpcID != "" {
			out = append(out, "--vpc-id", scopeVpcID)
		}
	}
	return out
}

func fetchMachinesWithSiteFallback(s *Session, missingSitePrompt string) ([]NamedItem, error) {
	savedSiteID := s.Scope.SiteID
	savedVpcID, savedVpcName := s.Scope.VpcID, s.Scope.VpcName

	s.Scope.VpcID = ""
	s.Scope.VpcName = ""
	s.Cache.InvalidateFiltered()
	defer func() {
		if s.Scope.SiteID == savedSiteID {
			s.Scope.VpcID = savedVpcID
			s.Scope.VpcName = savedVpcName
		}
		s.Cache.InvalidateFiltered()
	}()

	items, err := s.Resolver.Fetch(context.Background(), "machine")
	if err == nil {
		return items, nil
	}
	if s.Scope.SiteID == "" && strings.Contains(err.Error(), "400") {
		fmt.Printf("%s %s\n", Dim("Note:"), missingSitePrompt)
		site, resolveErr := s.Resolver.Resolve(context.Background(), "site", "Site")
		if resolveErr != nil {
			return nil, resolveErr
		}
		s.Scope.SiteID = site.ID
		s.Scope.SiteName = site.Name
		s.Cache.InvalidateFiltered()
		return s.Resolver.Fetch(context.Background(), "machine")
	}
	return nil, err
}

func setSiteScopeFromID(s *Session, siteID string) {
	siteID = strings.TrimSpace(siteID)
	if siteID == "" || s.Scope.SiteID == siteID {
		return
	}
	s.Scope.SiteID = siteID
	s.Scope.SiteName = s.Resolver.ResolveID("site", siteID)
	s.Scope.VpcID = ""
	s.Scope.VpcName = ""
	s.Cache.InvalidateFiltered()
}

// requireSiteScope returns the current site scope ID. If unset, prompts the
// user to pick a site and persists that as the active scope, mirroring how
// fetchMachinesWithSiteFallback handles missing site context. Used by the
// rack and tray lifecycle commands, where every endpoint requires a siteId
// query/body parameter.
func requireSiteScope(s *Session, missingSitePrompt string) (string, error) {
	if id := strings.TrimSpace(s.Scope.SiteID); id != "" {
		return id, nil
	}
	fmt.Printf("%s %s\n", Dim("Note:"), missingSitePrompt)
	site, err := s.Resolver.Resolve(context.Background(), "site", "Site")
	if err != nil {
		return "", err
	}
	setSiteScopeFromID(s, site.ID)
	return site.ID, nil
}

func readyMachineItemsForSite(machines []NamedItem, siteID string) []SelectItem {
	siteID = strings.TrimSpace(siteID)
	readyItems := make([]SelectItem, 0, len(machines))
	for _, m := range machines {
		if siteID != "" && strings.TrimSpace(m.Extra["siteId"]) != siteID {
			continue
		}
		if strings.EqualFold(strings.TrimSpace(m.Status), "Ready") {
			readyItems = append(readyItems, SelectItem{Label: machineSelectLabel(m), ID: m.ID})
		}
	}
	return readyItems
}

func promptInstanceMachine(s *Session, ctx context.Context, siteID string) (*SelectItem, error) {
	canTargetMachine, err := s.tenantHasTargetedInstanceCreationAtSite(ctx, siteID)
	if err != nil {
		return nil, fmt.Errorf("checking targeted instance creation capability: %w", err)
	}
	if !canTargetMachine {
		return nil, fmt.Errorf("current tenant does not have effective targeted instance creation permission for the selected site")
	}

	// Temporarily clear VPC scope so fetchMachines returns all Site Machines
	// rather than Machines already assigned to the scoped VPC.
	savedVpcID, savedVpcName := s.Scope.VpcID, s.Scope.VpcName
	s.Scope.VpcID, s.Scope.VpcName = "", ""
	machines, fetchErr := fetchMachinesWithSiteFallback(s, "Machine listing requires a site filter. Select a site.")
	s.Scope.VpcID, s.Scope.VpcName = savedVpcID, savedVpcName
	if fetchErr != nil {
		return nil, fmt.Errorf("fetching machines: %w", fetchErr)
	}
	readyItems := readyMachineItemsForSite(machines, siteID)
	if len(readyItems) == 0 {
		return nil, fmt.Errorf("no machines in Ready state available for selected VPC site")
	}
	return Select("Machine", readyItems)
}

// machineSelectLabel formats a machine for the interactive select list. It
// always includes the resolved display name (which may be a serial number when
// no friendly labels are set) plus the full machine ID, so reviewers and
// scripts that already track machines by ID can find them without having to
// memorize serial-to-id mappings.
func machineSelectLabel(m NamedItem) string {
	name := strings.TrimSpace(m.Name)
	id := strings.TrimSpace(m.ID)
	if name == "" {
		return id
	}
	if id == "" {
		return name
	}
	return name + "  " + Dim(id)
}

// parseMutationResponseRequiringID rejects success payloads that cannot identify the
// mutated resource.
func parseMutationResponseRequiringID(resp []byte, description string) (map[string]interface{}, error) {
	var parsed map[string]interface{}
	err := json.Unmarshal(resp, &parsed)
	if err != nil {
		return nil, fmt.Errorf("parsing %s response: %w", description, err)
	}
	if strings.TrimSpace(str(parsed, "id")) == "" {
		return nil, fmt.Errorf("parsing %s response: missing id", description)
	}
	return parsed, nil
}

// -- List commands --

func cmdSiteList(s *Session, _ []string) error {
	LogCmd(s, "site", "list")
	items, err := s.Resolver.Fetch(context.Background(), "site")
	if err != nil {
		return err
	}
	return printResourceTable(os.Stdout, "NAME", "STATUS", "ID", items)
}

func cmdSiteCreate(s *Session, _ []string) error {
	name, err := PromptText("Site name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	serialConsoleHostname, err := PromptText("Serial console hostname (optional)", false)
	if err != nil {
		return err
	}
	city, err := PromptText("Location city (optional)", false)
	if err != nil {
		return err
	}
	state, err := PromptText("Location state (optional)", false)
	if err != nil {
		return err
	}
	country, err := PromptText("Location country (optional)", false)
	if err != nil {
		return err
	}
	contactEmail, err := PromptText("Contact email (optional)", false)
	if err != nil {
		return err
	}

	body := map[string]interface{}{
		"name": name,
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if strings.TrimSpace(serialConsoleHostname) != "" {
		body["serialConsoleHostname"] = strings.TrimSpace(serialConsoleHostname)
	}
	location := map[string]interface{}{}
	if strings.TrimSpace(city) != "" {
		location["city"] = strings.TrimSpace(city)
	}
	if strings.TrimSpace(state) != "" {
		location["state"] = strings.TrimSpace(state)
	}
	if strings.TrimSpace(country) != "" {
		location["country"] = strings.TrimSpace(country)
	}
	if len(location) > 0 {
		body["location"] = location
	}
	if strings.TrimSpace(contactEmail) != "" {
		body["contact"] = map[string]interface{}{"email": strings.TrimSpace(contactEmail)}
	}

	LogCmd(s, "site", "create", "--name", name)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "site"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating site: %w", err)
	}
	s.Cache.Invalidate("site")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created site")
	if err != nil {
		return err
	}
	fmt.Printf("%s Site created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdSiteUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "site", "Site to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("Site name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	serialConsoleHostname, err := PromptText("Serial console hostname (optional)", false)
	if err != nil {
		return err
	}
	renewTokenText, err := PromptText("Renew registration token? (true/false, blank to keep)", false)
	if err != nil {
		return err
	}
	serialEnabledText, err := PromptText("Serial console enabled? (true/false, blank to keep)", false)
	if err != nil {
		return err
	}
	sshKeysEnabledText, err := PromptText("Serial console SSH keys enabled? (true/false, blank to keep)", false)
	if err != nil {
		return err
	}
	idleTimeoutText, err := PromptText("Serial console idle timeout seconds (optional)", false)
	if err != nil {
		return err
	}
	maxSessionText, err := PromptText("Serial console max session length seconds (optional)", false)
	if err != nil {
		return err
	}
	city, err := PromptText("Location city (optional)", false)
	if err != nil {
		return err
	}
	state, err := PromptText("Location state (optional)", false)
	if err != nil {
		return err
	}
	country, err := PromptText("Location country (optional)", false)
	if err != nil {
		return err
	}
	contactEmail, err := PromptText("Contact email (optional)", false)
	if err != nil {
		return err
	}

	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if strings.TrimSpace(serialConsoleHostname) != "" {
		body["serialConsoleHostname"] = strings.TrimSpace(serialConsoleHostname)
	}
	if v, ok := parseOptionalBool(renewTokenText); ok {
		body["renewRegistrationToken"] = v
	}
	if v, ok := parseOptionalBool(serialEnabledText); ok {
		body["isSerialConsoleEnabled"] = v
	}
	if v, ok := parseOptionalBool(sshKeysEnabledText); ok {
		body["isSerialConsoleSSHKeysEnabled"] = v
	}
	if v, ok := parseOptionalInt(idleTimeoutText); ok {
		body["serialConsoleIdleTimeout"] = v
	}
	if v, ok := parseOptionalInt(maxSessionText); ok {
		body["serialConsoleMaxSessionLength"] = v
	}
	location := map[string]interface{}{}
	if strings.TrimSpace(city) != "" {
		location["city"] = strings.TrimSpace(city)
	}
	if strings.TrimSpace(state) != "" {
		location["state"] = strings.TrimSpace(state)
	}
	if strings.TrimSpace(country) != "" {
		location["country"] = strings.TrimSpace(country)
	}
	if len(location) > 0 {
		body["location"] = location
	}
	if strings.TrimSpace(contactEmail) != "" {
		body["contact"] = map[string]interface{}{"email": strings.TrimSpace(contactEmail)}
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}

	LogCmd(s, "site", "update", item.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "site/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating site: %w", err)
	}
	s.Cache.Invalidate("site")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated site")
	if err != nil {
		return err
	}
	fmt.Printf("%s Site updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdSiteDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "site", "Site to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete site %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	purgeMachines, err := PromptConfirm("Purge machine data as part of delete?")
	if err != nil {
		return err
	}
	query := map[string]string{}
	if purgeMachines {
		query["purgeMachines"] = "true"
	}
	LogCmd(s, "site", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "site/{id}"), map[string]string{"id": item.ID}, query, nil)
	if err != nil {
		return fmt.Errorf("deleting site: %w", err)
	}
	s.Cache.Invalidate("site")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s Site delete requested: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdVPCList(s *Session, args []string) error {
	LogCmd(s, "vpc", "list")
	items, err := s.Resolver.Fetch(context.Background(), "vpc")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	siteNameByID := map[string]string{}
	if sites, err := s.Resolver.Fetch(context.Background(), "site"); err == nil {
		for _, site := range sites {
			siteNameByID[site.ID] = site.Name
		}
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tSITE\tLABELS\tID")
	for _, item := range items {
		siteID := item.Extra["siteId"]
		siteName := strings.TrimSpace(siteNameByID[siteID])
		if siteName == "" {
			siteName = siteID
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\n", item.Name, item.Status, siteName, formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdVPCCreate(s *Session, _ []string) error {
	site, err := s.Resolver.Resolve(context.Background(), "site", "Site")
	if err != nil {
		return err
	}
	name, err := PromptText("VPC name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{"name": name, "siteId": site.ID}
	if strings.TrimSpace(desc) != "" {
		body["description"] = desc
	}

	routingProfile := ""
	routingProfileOverride := ""
	siteRaw, _ := site.Raw.(map[string]interface{})
	siteCapabilities, _ := siteRaw["capabilities"].(map[string]interface{})
	nativeNetworking, _ := siteCapabilities["nativeNetworking"].(bool)
	if nativeNetworking {
		routingProfileResponse, _, requestErr := s.Client.Do("GET", apiPath(s, "tenant/current/routing-profile"), nil, map[string]string{"siteId": site.ID}, nil)
		if requestErr != nil {
			return fmt.Errorf("fetching Tenant routing profiles: %w", requestErr)
		}
		var tenantRoutingProfile struct {
			DefaultRoutingProfile    string   `json:"defaultRoutingProfile"`
			PermittedRoutingProfiles []string `json:"permittedRoutingProfiles"`
		}
		if err := json.Unmarshal(routingProfileResponse, &tenantRoutingProfile); err != nil {
			return fmt.Errorf("parsing Tenant routing profiles: %w", err)
		}
		routingProfile, err = PromptChoice(
			fmt.Sprintf("Routing profile (%s (tenant default))", tenantRoutingProfile.DefaultRoutingProfile),
			tenantRoutingProfile.PermittedRoutingProfiles,
			tenantRoutingProfile.DefaultRoutingProfile,
		)
		if err != nil {
			return err
		}
		if routingProfile != tenantRoutingProfile.DefaultRoutingProfile {
			routingProfileOverride = routingProfile
			body["routingProfile"] = routingProfileOverride
		}
	}

	logArgs := []string{"vpc", "create", "--name", name, "--site-id", site.ID}
	if routingProfileOverride != "" {
		logArgs = append(logArgs, "--routing-profile", routingProfileOverride)
	}
	LogCmd(s, logArgs...)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "vpc"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating VPC: %w", err)
	}
	s.Cache.Invalidate("vpc")
	created, err := parseMutationResponseRequiringID(resp, "created VPC")
	if err != nil {
		return err
	}
	resolvedRoutingProfile := str(created, "routingProfile")
	if resolvedRoutingProfile != "" {
		fmt.Printf("%s VPC created: %s (%s), routing profile: %s\n", Green("OK"), str(created, "name"), str(created, "id"), resolvedRoutingProfile)
	} else {
		fmt.Printf("%s VPC created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	}
	return nil
}

func cmdVPCUpdate(s *Session, args []string) error {
	vpc, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc", "VPC to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("VPC name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}
	LogCmd(s, "vpc", "update", vpc.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "vpc/{id}"), map[string]string{"id": vpc.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating VPC: %w", err)
	}
	s.Cache.Invalidate("vpc")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated VPC")
	if err != nil {
		return err
	}
	fmt.Printf("%s VPC updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdVPCVirtualizationUpdate(s *Session, args []string) error {
	vpc, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc", "VPC to update virtualization", args)
	if err != nil {
		return err
	}
	virtType, err := PromptText("Network virtualization type (ETHERNET_VIRTUALIZER or FNN)", true)
	if err != nil {
		return err
	}
	virtType = strings.ToUpper(strings.TrimSpace(virtType))
	if virtType != "ETHERNET_VIRTUALIZER" && virtType != "FNN" {
		return fmt.Errorf("network virtualization type must be ETHERNET_VIRTUALIZER or FNN")
	}
	body := map[string]interface{}{
		"networkVirtualizationType": virtType,
	}
	if virtType == "FNN" {
		useNVLink, err := PromptConfirm("Select NVLink logical partition?")
		if err != nil {
			return err
		}
		if useNVLink {
			item, err := s.Resolver.Resolve(context.Background(), "nvlink-logical-partition", "NVLink Logical Partition")
			if err != nil {
				return err
			}
			body["nvLinkLogicalPartitionId"] = item.ID
		}
	}
	LogCmd(s, "vpc", "virtualization", "update", vpc.ID, "--network-virtualization-type", virtType)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "vpc/{id}/virtualization"), map[string]string{"id": vpc.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating VPC virtualization: %w", err)
	}
	s.Cache.Invalidate("vpc")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated VPC virtualization")
	if err != nil {
		return err
	}
	fmt.Printf("%s VPC virtualization update submitted: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdVPCDelete(s *Session, args []string) error {
	vpc, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc", "VPC to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete VPC %s (%s)?", vpc.Name, vpc.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "vpc", "delete", vpc.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "vpc/{id}"), map[string]string{"id": vpc.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting VPC: %w", err)
	}
	s.Cache.Invalidate("vpc")
	fmt.Printf("%s VPC deleted: %s\n", Green("OK"), vpc.Name)
	return nil
}

func cmdSubnetList(s *Session, _ []string) error {
	LogCmd(s, "subnet", "list")
	items, err := s.Resolver.Fetch(context.Background(), "subnet")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tVPC\tID")
	for _, item := range items {
		vpcName := s.Resolver.ResolveID("vpc", item.Extra["vpcId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, vpcName, item.ID)
	}
	return tw.Flush()
}

// validateIPv4SubnetPrefixLength checks the REST IPv4 Subnet range.
func validateIPv4SubnetPrefixLength(prefixLength int) error {
	if prefixLength < 8 || prefixLength > 30 {
		return fmt.Errorf("prefix length must be between 8 and 30")
	}
	return nil
}

// filterSubnetVPCs keeps Ready Ethernet virtualizer and legacy untyped VPCs
// that the REST Subnet handler accepts.
func filterSubnetVPCs(vpcs []NamedItem) []NamedItem {
	filtered := make([]NamedItem, 0, len(vpcs))
	for _, vpc := range vpcs {
		if !strings.EqualFold(strings.TrimSpace(vpc.Status), "Ready") {
			continue
		}
		virtualizationType := strings.TrimSpace(vpc.Extra["networkVirtualizationType"])
		// An empty type identifies a legacy VPC that the server preserves.
		if virtualizationType != "" && virtualizationType != "ETHERNET_VIRTUALIZER" {
			continue
		}
		filtered = append(filtered, vpc)
	}
	return filtered
}

// filterSubnetAttachVPCs keeps Ready tenant-owned Ethernet virtualizer VPCs
// at the Subnet's Site. Empty virtualization type is the server's legacy
// Ethernet virtualizer representation. excludedVpcID is omitted from target
// selection so the interactive operation cannot be used as a no-op attachment.
func filterSubnetAttachVPCs(vpcs []NamedItem, siteID, tenantID, excludedVpcID, requiredVirtualizationType string) []NamedItem {
	filtered := make([]NamedItem, 0, len(vpcs))
	for _, vpc := range vpcs {
		if !strings.EqualFold(strings.TrimSpace(vpc.Status), "Ready") {
			continue
		}
		virtualizationType := cmp.Or(strings.TrimSpace(vpc.Extra["networkVirtualizationType"]), "ETHERNET_VIRTUALIZER")
		if virtualizationType != "ETHERNET_VIRTUALIZER" &&
			virtualizationType != "ETHERNET_VIRTUALIZER_WITH_NVUE" {
			continue
		}
		if requiredVirtualizationType != "" && virtualizationType != requiredVirtualizationType {
			continue
		}
		if strings.TrimSpace(vpc.Extra["siteId"]) != strings.TrimSpace(siteID) {
			continue
		}
		if strings.TrimSpace(vpc.Extra["tenantId"]) != strings.TrimSpace(tenantID) {
			continue
		}
		if strings.TrimSpace(vpc.ID) == strings.TrimSpace(excludedVpcID) {
			continue
		}
		filtered = append(filtered, vpc)
	}
	return filtered
}

// filterSubnetAttachSources keeps unallocated Ready Subnets whose source VPC
// satisfies the same tenant, Site, and Ethernet virtualizer contract as the
// target VPC. Two acquired IPs are the Subnet's reserved gateway and broadcast
// addresses; any higher value means the Subnet has workload Interfaces.
func filterSubnetAttachSources(subnets, vpcs []NamedItem, siteID, tenantID string) []NamedItem {
	eligibleVPCs := filterSubnetAttachVPCs(vpcs, siteID, tenantID, "", "")
	eligibleVPCIDs := make(map[string]struct{}, len(eligibleVPCs))
	for _, vpc := range eligibleVPCs {
		eligibleVPCIDs[strings.TrimSpace(vpc.ID)] = struct{}{}
	}

	filtered := make([]NamedItem, 0, len(subnets))
	for _, subnet := range subnets {
		if !strings.EqualFold(strings.TrimSpace(subnet.Status), "Ready") ||
			strings.TrimSpace(subnet.Extra["siteId"]) != strings.TrimSpace(siteID) ||
			strings.TrimSpace(subnet.Extra["acquiredIPs"]) != "2" {
			continue
		}
		if _, ok := eligibleVPCIDs[strings.TrimSpace(subnet.Extra["vpcId"])]; !ok {
			continue
		}
		filtered = append(filtered, subnet)
	}
	return filtered
}

func resolveSubnetAttachSource(s *Session, sources []NamedItem, args []string) (*NamedItem, error) {
	if len(args) == 0 || strings.TrimSpace(args[0]) == "" {
		return s.Resolver.SelectFromItems("Ready unallocated tenant Subnet", sources)
	}
	matches := matchingGeneratedResourceItems(sources, args[0])
	if len(matches) > 1 {
		return nil, fmt.Errorf("ambiguous subnet %q matches %d resources; use an exact ID", args[0], len(matches))
	}
	if len(matches) == 0 {
		return nil, fmt.Errorf("no eligible subnet matching %q found", args[0])
	}
	fmt.Printf("%s %s %s\n", Bold("Subnet to attach:"), Green(matches[0].Name), Dim("(matched)"))
	return &matches[0], nil
}

func (s *Session) subnetAttachSources(ctx context.Context, siteID, tenantID string) ([]NamedItem, error) {
	vpcs, err := s.Resolver.Fetch(ctx, "vpc")
	if err != nil {
		return nil, fmt.Errorf("fetching vpc: %w", err)
	}
	subnets, err := s.Resolver.Fetch(ctx, "subnet")
	if err != nil {
		return nil, fmt.Errorf("fetching subnet: %w", err)
	}
	return filterSubnetAttachSources(subnets, vpcs, siteID, tenantID), nil
}

// buildSubnetIPBlockSelectItems returns Ready, tenant-owned IPv4 allocation
// blocks at the selected VPC's Site.
func buildSubnetIPBlockSelectItems(ipBlocks []NamedItem, siteID, tenantID string) []SelectItem {
	siteID = strings.TrimSpace(siteID)
	tenantID = strings.TrimSpace(tenantID)
	items := make([]SelectItem, 0, len(ipBlocks))
	for _, block := range ipBlocks {
		if !strings.EqualFold(strings.TrimSpace(block.Status), "Ready") {
			continue
		}
		if strings.TrimSpace(block.Extra["protocolVersion"]) != "IPv4" {
			continue
		}
		if siteID != "" && strings.TrimSpace(block.Extra["siteId"]) != siteID {
			continue
		}
		if strings.TrimSpace(block.Extra["tenantId"]) != tenantID {
			continue
		}
		blockID := strings.TrimSpace(block.ID)
		if blockID == "" {
			continue
		}
		label := strings.TrimSpace(block.Name)
		if label == "" {
			label = blockID
		}
		items = append(items, SelectItem{Label: label, ID: blockID})
	}
	return items
}

func cmdSubnetCreate(s *Session, _ []string) error {
	vpcs, err := s.Resolver.Fetch(context.Background(), "vpc")
	if err != nil {
		return fmt.Errorf("fetching vpc: %w", err)
	}
	vpc, err := s.Resolver.SelectFromItems("Ready Ethernet virtualizer VPC", filterSubnetVPCs(vpcs))
	if err != nil {
		return err
	}
	vpcSiteID := strings.TrimSpace(vpc.Extra["siteId"])
	setSiteScopeFromID(s, vpcSiteID)

	name, err := PromptText("Subnet name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	prefixLenText, err := PromptText("IPv4 prefix length (8-30)", true)
	if err != nil {
		return err
	}
	prefixLen, err := strconv.Atoi(strings.TrimSpace(prefixLenText))
	if err != nil {
		return fmt.Errorf("prefix length must be an integer: %w", err)
	}
	err = validateIPv4SubnetPrefixLength(prefixLen)
	if err != nil {
		return err
	}

	ipBlocks, tenantID, err := s.fetchTenantIPBlocks(context.Background())
	if err != nil {
		return fmt.Errorf("fetching IP blocks: %w", err)
	}
	blockItems := buildSubnetIPBlockSelectItems(ipBlocks, vpcSiteID, tenantID)
	if len(blockItems) == 0 {
		if vpcSiteID != "" {
			return fmt.Errorf("no Ready IPv4 IP blocks available for current tenant at selected VPC site")
		}
		return fmt.Errorf("no Ready IPv4 IP blocks available for current tenant")
	}
	block, err := Select("Tenant IPv4 Block:", blockItems)
	if err != nil {
		return err
	}
	assignDomain, err := PromptConfirm("Assign a DNS Domain?")
	if err != nil {
		return err
	}
	var domain *NamedItem
	if assignDomain {
		domain, err = s.Resolver.Resolve(context.Background(), "domain", "DNS Domain")
		if err != nil {
			return err
		}
		if strings.TrimSpace(domain.Extra["siteId"]) != vpcSiteID {
			return fmt.Errorf("selected Domain is not at the selected VPC site")
		}
	}

	body := map[string]interface{}{
		"name":         name,
		"vpcId":        vpc.ID,
		"ipv4BlockId":  block.ID,
		"prefixLength": prefixLen,
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	logArgs := []string{"subnet", "create", "--name", name, "--vpc-id", vpc.ID, "--ipv4-block-id", block.ID, "--prefix-length", prefixLenText}
	if domain != nil {
		body["subdomainId"] = domain.ID
		logArgs = append(logArgs, "--subdomain-id", domain.ID)
	}
	LogCmd(s, logArgs...)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "subnet"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating subnet: %w", err)
	}
	s.Cache.Invalidate("subnet")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created subnet")
	if err != nil {
		return err
	}
	fmt.Printf("%s IPv4 Subnet created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdSubnetAttachVPC(s *Session, args []string) error {
	ctx := context.Background()
	siteID, err := requireSiteScope(s, "Subnet attachment requires a Site. Select a Site.")
	if err != nil {
		return err
	}
	tenantID, err := s.getTenantID(ctx)
	if err != nil {
		return err
	}

	// Attachment is Site-wide even when the interactive session has a narrower
	// VPC scope. Clear that scope for source and target discovery, then restore
	// it and invalidate filtered caches on every return path.
	savedVpcID, savedVpcName := s.Scope.VpcID, s.Scope.VpcName
	s.Scope.VpcID = ""
	s.Scope.VpcName = ""
	s.Cache.InvalidateFiltered()
	defer func() {
		s.Scope.VpcID = savedVpcID
		s.Scope.VpcName = savedVpcName
		s.Cache.InvalidateFiltered()
	}()

	sources, err := s.subnetAttachSources(ctx, siteID, tenantID)
	if err != nil {
		return err
	}
	subnet, err := resolveSubnetAttachSource(s, sources, args)
	if err != nil {
		return err
	}
	sourceVpcID := strings.TrimSpace(subnet.Extra["vpcId"])
	vpcs, err := s.Resolver.Fetch(ctx, "vpc")
	if err != nil {
		return fmt.Errorf("fetching vpc: %w", err)
	}
	sourceVirtualizationType := ""
	for _, vpc := range filterSubnetAttachVPCs(vpcs, siteID, tenantID, "", "") {
		if strings.TrimSpace(vpc.ID) == sourceVpcID {
			sourceVirtualizationType = cmp.Or(strings.TrimSpace(vpc.Extra["networkVirtualizationType"]), "ETHERNET_VIRTUALIZER")
			break
		}
	}
	if sourceVirtualizationType == "" {
		return fmt.Errorf("source VPC for Subnet %s is no longer eligible for attachment", subnet.ID)
	}
	target, err := s.Resolver.SelectFromItems(
		"Target Ready tenant Ethernet virtualizer VPC",
		filterSubnetAttachVPCs(vpcs, siteID, tenantID, sourceVpcID, sourceVirtualizationType),
	)
	if err != nil {
		return err
	}
	confirmed, err := PromptConfirm(fmt.Sprintf("Allow replacing Subnet %s (%s) attachment with VPC %s (%s)?", subnet.Name, subnet.ID, target.Name, target.ID))
	if err != nil || !confirmed {
		return err
	}
	bodyJSON, _ := json.Marshal(map[string]interface{}{"vpcId": target.ID, "allowReplace": true})
	LogCmd(s, "subnet", "attach-vpc", subnet.ID, "--vpc-id", target.ID, "--allow-replace")
	resp, _, err := s.Client.Do(
		"POST",
		apiPath(s, "subnet/{subnetId}/attach-vpc"),
		map[string]string{"subnetId": subnet.ID},
		nil,
		bodyJSON,
	)
	if err != nil {
		return fmt.Errorf("attaching subnet to VPC: %w", err)
	}
	s.Cache.Invalidate("subnet")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "attached subnet")
	if err != nil {
		return err
	}
	if str(updated, "id") != subnet.ID {
		return fmt.Errorf("parsing attached subnet response: id does not match selected Subnet")
	}
	if str(updated, "vpcId") != target.ID {
		return fmt.Errorf("parsing attached subnet response: vpcId does not match selected target VPC")
	}
	fmt.Printf("%s Subnet attached to VPC: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdSubnetUpdate(s *Session, args []string) error {
	subnet, err := s.Resolver.ResolveWithArgs(context.Background(), "subnet", "Subnet to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("Subnet name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}
	LogCmd(s, "subnet", "update", subnet.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "subnet/{id}"), map[string]string{"id": subnet.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating subnet: %w", err)
	}
	s.Cache.Invalidate("subnet")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated subnet")
	if err != nil {
		return err
	}
	fmt.Printf("%s Subnet updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdSubnetDelete(s *Session, args []string) error {
	subnet, err := s.Resolver.ResolveWithArgs(context.Background(), "subnet", "Subnet to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete subnet %s (%s)?", subnet.Name, subnet.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "subnet", "delete", subnet.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "subnet/{id}"), map[string]string{"id": subnet.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting subnet: %w", err)
	}
	s.Cache.Invalidate("subnet")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s Subnet deleted: %s\n", Green("OK"), subnet.Name)
	return nil
}

func cmdInstanceTypeList(s *Session, args []string) error {
	LogCmd(s, "instance-type", "list")
	items, err := s.Resolver.Fetch(context.Background(), "instance-type")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tLABELS\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdInstanceTypeGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "instance-type", "Instance Type", args)
	if err != nil {
		return err
	}
	LogCmd(s, "instance-type", "get", item.ID)
	return getAndPrint(s, apiPath(s, "instance/type/{id}"), item.ID)
}

func cmdInstanceList(s *Session, args []string) error {
	LogCmd(s, "instance", "list")
	_, _ = s.Resolver.Fetch(context.Background(), "vpc")
	_, _ = s.Resolver.Fetch(context.Background(), "site")
	items, err := s.Resolver.Fetch(context.Background(), "instance")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tIP ADDRESSES\tSTATUS\tVPC\tSITE\tLABELS\tID")
	for _, item := range items {
		ipAddresses := strings.Join(instanceIPAddresses(item.Raw), ", ")
		if ipAddresses == "" {
			ipAddresses = "-"
		}
		vpcName := s.Resolver.ResolveID("vpc", item.Extra["vpcId"])
		siteName := s.Resolver.ResolveID("site", item.Extra["siteId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\n", item.Name, ipAddresses, item.Status, vpcName, siteName, formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func instanceIPAddresses(raw interface{}) []string {
	instance, ok := raw.(map[string]interface{})
	if !ok {
		return nil
	}
	addresses := interfaceIPAddresses(instance["interfaces"])
	if len(addresses) > 0 {
		return addresses
	}
	status, ok := instance["status"].(map[string]interface{})
	if !ok {
		return nil
	}
	network, ok := status["network"].(map[string]interface{})
	if !ok {
		return nil
	}
	return interfaceIPAddresses(network["interfaces"])
}

func interfaceIPAddresses(raw interface{}) []string {
	interfaces, _ := raw.([]interface{})
	var addresses []string
	for _, rawInterface := range interfaces {
		instanceInterface, ok := rawInterface.(map[string]interface{})
		if !ok {
			continue
		}
		addresses = append(addresses, stringSlice(instanceInterface["ipAddresses"])...)
	}
	return addresses
}

func cmdMachineList(s *Session, args []string) error {
	LogCmd(s, "machine", "list")
	items, err := fetchMachinesWithSiteFallback(s, "Machine listing requires a site filter. Select a site.")
	if err != nil {
		return err
	}

	_, _ = s.Resolver.Fetch(context.Background(), "vpc")
	vpcNamesByMachineID := s.buildMachineVPCNames(context.Background())

	if s.Scope.VpcID != "" {
		filtered := make([]NamedItem, 0, len(items))
		for _, item := range items {
			if _, ok := vpcNamesByMachineID[item.ID]; ok {
				filtered = append(filtered, item)
			}
		}
		items = filtered
	}

	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}

	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tIP ADDRESS\tSTATUS\tBLOCKED BY\tSITE\tVPC\tLABELS\tID")
	for _, item := range items {
		ipAddress := firstMachineIPAddress(item.Raw)
		siteName := s.Resolver.ResolveID("site", item.Extra["siteId"])
		vpcNames := strings.TrimSpace(vpcNamesByMachineID[item.ID])
		if vpcNames == "" {
			vpcNames = "-"
		}
		blockedBy := summarizeBlockingAlert(item.Raw)
		if blockedBy == "" {
			blockedBy = "-"
		}
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", item.Name, ipAddress, item.Status, blockedBy, siteName, vpcNames, formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func firstMachineIPAddress(raw interface{}) string {
	machine, ok := raw.(map[string]interface{})
	if !ok {
		return ""
	}
	interfaces, ok := machine["machineInterfaces"].([]interface{})
	if !ok {
		return ""
	}
	for _, rawInterface := range interfaces {
		machineInterface, ok := rawInterface.(map[string]interface{})
		if !ok {
			continue
		}
		ipAddresses, ok := machineInterface["ipAddresses"].([]interface{})
		if !ok {
			continue
		}
		for _, rawIPAddress := range ipAddresses {
			ipAddress, ok := rawIPAddress.(string)
			if ok && strings.TrimSpace(ipAddress) != "" {
				return ipAddress
			}
		}
	}
	return ""
}

// blockingHealthAlert captures the fields from MachineHealthProbeAlert that we
// surface in machine list/get to explain why a machine is blocked. Populated
// from raw[health][alerts][n] when alerts[n].classifications contains
// "PreventAllocations".
type blockingHealthAlert struct {
	ID              string
	Target          string
	Message         string
	Classifications []string
}

// extractBlockingAlerts walks raw["health"]["alerts"] and returns the alerts
// whose classifications include "PreventAllocations". These are the alerts
// that prevent the machine from being allocated to a tenant, which is what
// operators care about when triaging Error-state machines. Other alert types
// are intentionally skipped to keep the table column actionable instead of
// noisy.
func extractBlockingAlerts(raw interface{}) []blockingHealthAlert {
	m, ok := raw.(map[string]interface{})
	if !ok {
		return nil
	}
	health, ok := m["health"].(map[string]interface{})
	if !ok {
		return nil
	}
	rawAlerts, ok := health["alerts"].([]interface{})
	if !ok {
		return nil
	}
	out := make([]blockingHealthAlert, 0, len(rawAlerts))
	for _, ra := range rawAlerts {
		alert, ok := ra.(map[string]interface{})
		if !ok {
			continue
		}
		classifications := stringSliceField(alert, "classifications")
		if !containsCaseInsensitive(classifications, "PreventAllocations") {
			continue
		}
		out = append(out, blockingHealthAlert{
			ID:              str(alert, "id"),
			Target:          str(alert, "target"),
			Message:         str(alert, "message"),
			Classifications: classifications,
		})
	}
	return out
}

// summarizeBlockingAlert returns a short one-line summary for the machine list
// table column. Returns "" when the machine has no blocking alerts. Format is
// "<id>" or "<id> <target>" when target is concise enough to fit -- target is
// truncated at 24 chars to keep table rows readable on standard terminals.
func summarizeBlockingAlert(raw interface{}) string {
	alerts := extractBlockingAlerts(raw)
	if len(alerts) == 0 {
		return ""
	}
	a := alerts[0]
	id := strings.TrimSpace(a.ID)
	target := strings.TrimSpace(a.Target)
	if id == "" && target == "" {
		return ""
	}
	if target == "" {
		return id
	}
	const maxTarget = 24
	if len(target) > maxTarget {
		target = target[:maxTarget-3] + "..."
	}
	if id == "" {
		return target
	}
	return id + " " + target
}

func stringSliceField(m map[string]interface{}, key string) []string {
	raw, ok := m[key].([]interface{})
	if !ok {
		return nil
	}
	out := make([]string, 0, len(raw))
	for _, v := range raw {
		if s, ok := v.(string); ok {
			out = append(out, s)
		}
	}
	return out
}

func containsCaseInsensitive(haystack []string, needle string) bool {
	for _, h := range haystack {
		if strings.EqualFold(strings.TrimSpace(h), needle) {
			return true
		}
	}
	return false
}

func cmdOSList(s *Session, _ []string) error {
	LogCmd(s, "operating-system", "list")
	items, err := s.Resolver.Fetch(context.Background(), "operating-system")
	if err != nil {
		return err
	}
	return printResourceTable(os.Stdout, "NAME", "STATUS", "ID", items)
}

func cmdOSCreate(s *Session, _ []string) error {
	name, err := PromptText("Operating system name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	tenantID, err := s.getTenantID(context.Background())
	if err != nil {
		return fmt.Errorf("resolving tenant id: %w", err)
	}
	ipxeScript, err := PromptText("iPXE script or URL", true)
	if err != nil {
		return err
	}
	userData, err := PromptText("User data (optional)", false)
	if err != nil {
		return err
	}
	allowOverride, err := PromptConfirm("Allow override at instance creation?")
	if err != nil {
		return err
	}
	phoneHomeEnabled, err := PromptConfirm("Enable phone home?")
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"name":             name,
		"tenantId":         tenantID,
		"ipxeScript":       ipxeScript,
		"allowOverride":    allowOverride,
		"phoneHomeEnabled": phoneHomeEnabled,
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if strings.TrimSpace(userData) != "" {
		body["userData"] = strings.TrimSpace(userData)
	}
	LogCmd(s, "operating-system", "create", "--name", name)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "operating-system"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating operating system: %w", err)
	}
	s.Cache.Invalidate("operating-system")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created operating system")
	if err != nil {
		return err
	}
	fmt.Printf("%s Operating system created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdOSUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "operating-system", "Operating System to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("Operating system name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	ipxeScript, err := PromptText("iPXE script or URL (optional)", false)
	if err != nil {
		return err
	}
	userData, err := PromptText("User data (optional)", false)
	if err != nil {
		return err
	}
	allowOverrideText, err := PromptText("Allow override? (true/false, blank to keep)", false)
	if err != nil {
		return err
	}
	phoneHomeText, err := PromptText("Phone home enabled? (true/false, blank to keep)", false)
	if err != nil {
		return err
	}
	activeText, err := PromptText("Set active? (true/false, blank to keep)", false)
	if err != nil {
		return err
	}

	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if strings.TrimSpace(ipxeScript) != "" {
		body["ipxeScript"] = strings.TrimSpace(ipxeScript)
	}
	if strings.TrimSpace(userData) != "" {
		body["userData"] = strings.TrimSpace(userData)
	}
	if v, ok := parseOptionalBool(allowOverrideText); ok {
		body["allowOverride"] = v
	}
	if v, ok := parseOptionalBool(phoneHomeText); ok {
		body["phoneHomeEnabled"] = v
	}
	if v, ok := parseOptionalBool(activeText); ok {
		body["isActive"] = v
		if !v {
			note, err := PromptText("Deactivation note (optional)", false)
			if err != nil {
				return err
			}
			if strings.TrimSpace(note) != "" {
				body["deactivationNote"] = strings.TrimSpace(note)
			}
		}
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}
	LogCmd(s, "operating-system", "update", item.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "operating-system/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating operating system: %w", err)
	}
	s.Cache.Invalidate("operating-system")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated operating system")
	if err != nil {
		return err
	}
	fmt.Printf("%s Operating system updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdOSDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "operating-system", "Operating System to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete operating system %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "operating-system", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "operating-system/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting operating system: %w", err)
	}
	s.Cache.Invalidate("operating-system")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s Operating system deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdSSHKeyGroupList(s *Session, _ []string) error {
	LogCmd(s, "ssh-key-group", "list")
	items, err := s.Resolver.Fetch(context.Background(), "ssh-key-group")
	if err != nil {
		return err
	}
	return printResourceTable(os.Stdout, "NAME", "STATUS", "ID", items)
}

func cmdSSHKeyGroupCreate(s *Session, _ []string) error {
	ctx := context.Background()
	name, err := PromptText("SSH key group name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}

	siteIDs, err := promptOptionalResourceIDs(s, ctx, "site", "site")
	if err != nil {
		return err
	}
	sshKeyIDs, err := promptOptionalResourceIDs(s, ctx, "ssh-key", "SSH key")
	if err != nil {
		return err
	}

	body := map[string]interface{}{"name": name}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if len(siteIDs) > 0 {
		body["siteIds"] = siteIDs
	}
	if len(sshKeyIDs) > 0 {
		body["sshKeyIds"] = sshKeyIDs
	}

	LogCmd(s, "ssh-key-group", "create", "--name", name)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "sshkeygroup"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating SSH key group: %w", err)
	}
	s.Cache.Invalidate("ssh-key-group")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created SSH key group")
	if err != nil {
		return err
	}
	fmt.Printf("%s SSH key group created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdSSHKeyGroupUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ssh-key-group", "SSH Key Group to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("SSH key group name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	siteIDsText, err := PromptText("Replace site IDs (comma-separated, blank to keep)", false)
	if err != nil {
		return err
	}
	sshKeyIDsText, err := PromptText("Replace SSH key IDs (comma-separated, blank to keep)", false)
	if err != nil {
		return err
	}

	version := rawFieldString(item.Raw, "version")
	if strings.TrimSpace(version) == "" {
		version, err = PromptText("Version", true)
		if err != nil {
			return err
		}
	}
	body := map[string]interface{}{
		"version": version,
	}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if strings.TrimSpace(siteIDsText) != "" {
		body["siteIds"] = splitCommaSeparated(siteIDsText)
	}
	if strings.TrimSpace(sshKeyIDsText) != "" {
		body["sshKeyIds"] = splitCommaSeparated(sshKeyIDsText)
	}

	LogCmd(s, "ssh-key-group", "update", item.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "sshkeygroup/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating SSH key group: %w", err)
	}
	s.Cache.Invalidate("ssh-key-group")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated SSH key group")
	if err != nil {
		return err
	}
	fmt.Printf("%s SSH key group updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdSSHKeyGroupDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ssh-key-group", "SSH Key Group to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete SSH key group %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "ssh-key-group", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "sshkeygroup/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting SSH key group: %w", err)
	}
	s.Cache.Invalidate("ssh-key-group")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s SSH key group deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdSSHKeyList(s *Session, _ []string) error {
	LogCmd(s, "ssh-key", "list")
	items, err := s.Resolver.Fetch(context.Background(), "ssh-key")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tFINGERPRINT\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\n", item.Name, item.Extra["fingerprint"], item.ID)
	}
	return tw.Flush()
}

func cmdSSHKeyCreate(s *Session, _ []string) error {
	name, err := PromptText("SSH key name", true)
	if err != nil {
		return err
	}
	publicKey, err := PromptText("Public key", true)
	if err != nil {
		return err
	}
	sshKeyGroupID, err := PromptText("SSH key group ID (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"name":      name,
		"publicKey": publicKey,
	}
	if strings.TrimSpace(sshKeyGroupID) != "" {
		body["sshKeyGroupId"] = strings.TrimSpace(sshKeyGroupID)
	}
	LogCmd(s, "ssh-key", "create", "--name", name)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "sshkey"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating SSH key: %w", err)
	}
	s.Cache.Invalidate("ssh-key")
	s.Cache.Invalidate("ssh-key-group")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created SSH key")
	if err != nil {
		return err
	}
	fmt.Printf("%s SSH key created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdSSHKeyUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ssh-key", "SSH Key to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("SSH key name", true)
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"name": strings.TrimSpace(name),
	}
	LogCmd(s, "ssh-key", "update", item.ID, "--name", strings.TrimSpace(name))
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "sshkey/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating SSH key: %w", err)
	}
	s.Cache.Invalidate("ssh-key")
	s.Cache.Invalidate("ssh-key-group")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated SSH key")
	if err != nil {
		return err
	}
	fmt.Printf("%s SSH key updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdSSHKeyDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ssh-key", "SSH Key to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete SSH key %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "ssh-key", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "sshkey/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting SSH key: %w", err)
	}
	s.Cache.Invalidate("ssh-key")
	s.Cache.Invalidate("ssh-key-group")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s SSH key deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdAllocationList(s *Session, _ []string) error {
	LogCmd(s, "allocation", "list")
	items, err := s.Resolver.Fetch(context.Background(), "allocation")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tSITE\tID")
	for _, item := range items {
		siteName := s.Resolver.ResolveID("site", item.Extra["siteId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, siteName, item.ID)
	}
	return tw.Flush()
}

func cmdAllocationCreate(s *Session, _ []string) error {
	ctx := context.Background()
	site, err := s.Resolver.Resolve(ctx, "site", "Site")
	if err != nil {
		return err
	}
	// Scope subsequent resolver lookups (ip-block, instance-type) to the
	// allocation site so the constraint resource belongs to the same site.
	// This mutation is reverted on every exit path (success, error, or
	// interactive cancel) so the user's interactive scope is unchanged
	// after the command returns.
	savedScope := s.Scope
	setSiteScopeFromID(s, site.ID)
	defer func() {
		s.Scope = savedScope
		s.Cache.InvalidateFiltered()
	}()
	name, err := PromptText("Allocation name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	tenantID, err := promptAllocationTenantID(s, ctx)
	if err != nil {
		return err
	}
	constraints, err := promptAllocationConstraints(s, ctx)
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"name":                  name,
		"siteId":                site.ID,
		"tenantId":              tenantID,
		"allocationConstraints": constraints,
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	LogCmd(s, "allocation", "create", "--name", name, "--site-id", site.ID, "--tenant-id", tenantID)
	fmt.Fprintf(os.Stderr, "%s allocation constraints are passed via JSON body, not flags; see --debug for the full request\n", Dim("note:"))
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "allocation"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating allocation: %w", err)
	}
	s.Cache.Invalidate("allocation")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created allocation")
	if err != nil {
		return err
	}
	fmt.Printf("%s Allocation created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

const tenantManualEntrySentinel = "__manual__"

// promptAllocationTenantID prompts the user to pick a tenant for the allocation.
// It lists tenants derived from tenant-accounts (the allocation API expects a
// tenant ID, not a tenant-account ID) and falls back to manual entry if no
// tenant accounts are visible or if the user explicitly opts out of the list.
func promptAllocationTenantID(s *Session, ctx context.Context) (string, error) {
	accounts, err := s.Resolver.Fetch(ctx, "tenant-account")
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s could not list tenant accounts (%v); falling back to manual entry\n", Dim("note:"), err)
		accounts = nil
	}
	// Dev orgs (and any org that is both Provider Admin and Tenant Admin)
	// have an implicit self-tenant that does not appear in tenant-accounts.
	// Surface it as a first-class option so the operator does not have to
	// paste a raw UUID just to allocate to themselves.
	selfTenantID, _ := s.getTenantID(ctx)
	items := buildTenantSelectItems(accounts, selfTenantID, s.Org)
	if len(items) == 0 {
		fmt.Fprintf(os.Stderr, "%s no tenants found via tenant-account or current-tenant; falling back to manual entry\n", Dim("note:"))
		return promptTenantIDRaw()
	}
	selected, err := Select("Tenant:", items)
	if err != nil {
		return "", err
	}
	if selected.ID == tenantManualEntrySentinel {
		return promptTenantIDRaw()
	}
	return selected.ID, nil
}

func promptTenantIDRaw() (string, error) {
	raw, err := PromptText("Tenant ID", true)
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(raw), nil
}

// buildTenantSelectItems builds the tenant picker for allocation create.
//
// Sources (all optional):
//   - accounts: tenant-account rows the provider has established. Each row's
//     `Extra["tenantId"]` is the selector ID; duplicate tenantIds across rows
//     are collapsed.
//   - selfTenantID / selfOrg: the caller's own tenant, surfaced first with a
//     "(self)" suffix when the caller also holds a Tenant Admin role. Common
//     for dev orgs where provider and tenant are the same entity.
//
// Returns nil when no source yields a tenantId so the caller can fall back
// to raw manual entry. When any items exist, a trailing manual-entry
// sentinel is always appended so the user can still type a raw UUID.
// Distinct tenants that share a display name are disambiguated with a
// short tenant-id suffix so the picker is never ambiguous.
func buildTenantSelectItems(accounts []NamedItem, selfTenantID, selfOrg string) []SelectItem {
	items := make([]SelectItem, 0, len(accounts)+2)
	seen := make(map[string]struct{}, len(accounts)+1)
	labelCounts := make(map[string]int, len(accounts)+1)

	selfTenantID = strings.TrimSpace(selfTenantID)
	if selfTenantID != "" {
		selfLabel := strings.TrimSpace(selfOrg)
		if selfLabel == "" {
			selfLabel = selfTenantID
		}
		selfLabel += " (self)"
		seen[selfTenantID] = struct{}{}
		labelCounts[selfLabel]++
		items = append(items, SelectItem{Label: selfLabel, ID: selfTenantID})
	}

	for _, acc := range accounts {
		tenantID := ""
		if acc.Extra != nil {
			tenantID = acc.Extra["tenantId"]
		}
		if tenantID == "" {
			continue
		}
		if _, dup := seen[tenantID]; dup {
			continue
		}
		seen[tenantID] = struct{}{}
		label := acc.Name
		if strings.TrimSpace(label) == "" {
			label = tenantID
		}
		if acc.Status != "" {
			label += "  " + acc.Status
		}
		labelCounts[label]++
		items = append(items, SelectItem{Label: label, ID: tenantID})
	}
	if len(items) == 0 {
		return nil
	}
	// Disambiguate any items that share a label: distinct tenants with the
	// same display name would otherwise route the request to whichever the
	// user happens to highlight, which is silently wrong. Append a short
	// tenant id suffix so the picker is always unambiguous.
	for i := range items {
		if labelCounts[items[i].Label] > 1 {
			items[i].Label = fmt.Sprintf("%s (%s)", items[i].Label, shortTenantID(items[i].ID))
		}
	}
	// Sort everything except the self entry (kept at the top so the
	// operator does not have to hunt for the common case).
	sortStart := 0
	if selfTenantID != "" {
		sortStart = 1
	}
	if sortStart < len(items) {
		tail := items[sortStart:]
		sort.SliceStable(tail, func(i, j int) bool {
			if tail[i].Label == tail[j].Label {
				return tail[i].ID < tail[j].ID
			}
			return tail[i].Label < tail[j].Label
		})
	}
	items = append(items, SelectItem{Label: "Enter Tenant ID manually...", ID: tenantManualEntrySentinel})
	return items
}

// shortTenantID returns up to the last 8 chars of a tenant UUID for use
// inside disambiguating picker labels. UUID strings shorter than 8 chars
// are returned as-is.
func shortTenantID(id string) string {
	if len(id) <= 8 {
		return id
	}
	return id[len(id)-8:]
}

// promptAllocationConstraints collects exactly one allocation constraint.
// The REST API enforces len(allocationConstraints) == 1 in
// APIAllocationCreateRequest.Validate, so multiple entries are rejected
// server-side and a zero-length list is rejected as well.
func promptAllocationConstraints(s *Session, ctx context.Context) ([]map[string]interface{}, error) {
	fmt.Fprintf(os.Stderr, "%s allocation requires exactly one constraint\n", Dim("note:"))
	c, err := promptSingleAllocationConstraint(s, ctx)
	if err != nil {
		return nil, err
	}
	return []map[string]interface{}{c}, nil
}

func promptSingleAllocationConstraint(s *Session, ctx context.Context) (map[string]interface{}, error) {
	rt, err := Select("Resource type:", allocationConstraintResourceTypes())
	if err != nil {
		return nil, err
	}
	resolverKey, resolverLabel, ok := resolverResourceForAllocationResourceType(rt.ID)
	if !ok {
		return nil, fmt.Errorf("unsupported resource type %q", rt.ID)
	}
	item, err := s.Resolver.Resolve(ctx, resolverKey, resolverLabel)
	if err != nil {
		return nil, err
	}
	ct, err := Select("Constraint type:", allocationConstraintTypes())
	if err != nil {
		return nil, err
	}
	valueText, err := PromptText(fmt.Sprintf("Constraint value (%s)", allocationConstraintValueHint(rt.ID)), true)
	if err != nil {
		return nil, err
	}
	return buildAllocationConstraint(rt.ID, item.ID, ct.ID, valueText)
}

// allocationConstraintResourceTypes lists the supported resource types for an
// allocation constraint. These mirror the values accepted by the REST API
// (see APIAllocationConstraintCreateRequest.Validate).
func allocationConstraintResourceTypes() []SelectItem {
	return []SelectItem{
		{Label: "IPBlock (IP address allocation; value = prefix length)", ID: "IPBlock"},
		{Label: "InstanceType (machine allocation; value = machine count)", ID: "InstanceType"},
	}
}

// allocationConstraintTypes lists the constraint types offered to the user.
// Only Reserved is exposed: the API validator in
// api/pkg/api/model/allocationconstraint.go accepts OnDemand and Preemptible
// as well, but those two are documented as "not supported by current
// implementation" in the SDK and would turn a normal create flow into a
// server-side failure path.
//
// TODO(reenable-on-demand-preemptible): re-enable OnDemand and Preemptible
// SelectItems once the backend implements them end-to-end (track via the
// constraint-type validator in api/pkg/api/model/allocationconstraint.go).
func allocationConstraintTypes() []SelectItem {
	return []SelectItem{
		{Label: "Reserved", ID: "Reserved"},
	}
}

// resolverResourceForAllocationResourceType maps an allocation constraint
// resource type (as accepted by the API) to the TUI resolver key and a
// human-readable label to use when prompting.
func resolverResourceForAllocationResourceType(resourceType string) (resolverKey, label string, ok bool) {
	switch resourceType {
	case "IPBlock":
		return "ip-block", "IP Block", true
	case "InstanceType":
		return "instance-type", "Instance Type", true
	}
	return "", "", false
}

// allocationConstraintValueHint returns a short hint describing what the
// constraint value represents for a given resource type.
func allocationConstraintValueHint(resourceType string) string {
	switch resourceType {
	case "IPBlock":
		return "prefix length, e.g. 28"
	case "InstanceType":
		return "machine count, e.g. 4"
	}
	return "integer"
}

// buildAllocationConstraint assembles an API-shaped constraint body from its
// prompted fields. valueText is parsed as an integer and range-checked against
// the resource type so the user gets immediate feedback rather than waiting
// for a server-side rejection.
func buildAllocationConstraint(resourceType, resourceTypeID, constraintType, valueText string) (map[string]interface{}, error) {
	value, err := strconv.Atoi(strings.TrimSpace(valueText))
	if err != nil {
		return nil, fmt.Errorf("constraint value must be an integer: %w", err)
	}
	if err := validateAllocationConstraintValue(resourceType, value); err != nil {
		return nil, err
	}
	return map[string]interface{}{
		"resourceType":    resourceType,
		"resourceTypeId":  resourceTypeID,
		"constraintType":  constraintType,
		"constraintValue": value,
	}, nil
}

// validateAllocationConstraintValue enforces per-resource value ranges that
// the REST API itself currently validates only loosely (ConstraintValue is
// required but not range-checked, see the TODO in
// api/pkg/api/model/allocationconstraint.go).
func validateAllocationConstraintValue(resourceType string, value int) error {
	switch resourceType {
	case "IPBlock":
		if value < 1 || value > 32 {
			return fmt.Errorf("IPBlock constraint value must be an IPv4 prefix length between 1 and 32, got %d", value)
		}
	case "InstanceType":
		if value < 1 {
			return fmt.Errorf("InstanceType constraint value (machine count) must be at least 1, got %d", value)
		}
	default:
		if value <= 0 {
			return fmt.Errorf("constraint value must be a positive integer, got %d", value)
		}
	}
	return nil
}

func cmdAllocationUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "allocation", "Allocation to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("Allocation name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}
	LogCmd(s, "allocation", "update", item.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "allocation/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating allocation: %w", err)
	}
	s.Cache.Invalidate("allocation")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated allocation")
	if err != nil {
		return err
	}
	fmt.Printf("%s Allocation updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdIPBlockList(s *Session, _ []string) error {
	LogCmd(s, "ip-block", "list")
	items, err := s.Resolver.Fetch(context.Background(), "ip-block")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tSITE\tID")
	for _, item := range items {
		siteName := s.Resolver.ResolveID("site", item.Extra["siteId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, siteName, item.ID)
	}
	return tw.Flush()
}

// ipBlockProtocolVersions are the valid values for the protocolVersion field on
// the ipblock create API. Order matters: the first option is used as the
// default in interactive prompts.
var ipBlockProtocolVersions = []string{"IPv4", "IPv6"}

// ipBlockRoutingTypes are the valid values for the routingType field on the
// ipblock create API. Order matters: the first option is used as the default
// in interactive prompts.
var ipBlockRoutingTypes = []string{"DatacenterOnly", "Public"}

// validateIPBlockPrefixLength returns an error when the prefix length is out
// of range for the selected protocol version. IPv4 allows 1-32, IPv6 allows
// 1-128.
func validateIPBlockPrefixLength(protocolVersion string, prefixLength int) error {
	switch protocolVersion {
	case "IPv4":
		if prefixLength < 1 || prefixLength > 32 {
			return fmt.Errorf("prefix length must be between 1 and 32 for IPv4")
		}
	case "IPv6":
		if prefixLength < 1 || prefixLength > 128 {
			return fmt.Errorf("prefix length must be between 1 and 128 for IPv6")
		}
	default:
		return fmt.Errorf("unsupported protocol version: %s", protocolVersion)
	}
	return nil
}

// buildIPBlockCreateBody constructs the JSON request body sent to the ipblock
// create API. The field names must match the API's APIIPBlockCreateRequest
// struct tags or the server will reject the request with a 400.
func buildIPBlockCreateBody(name, siteID, prefix string, prefixLength int, protocolVersion, routingType string) map[string]interface{} {
	return map[string]interface{}{
		"name":            name,
		"siteId":          siteID,
		"prefix":          prefix,
		"prefixLength":    prefixLength,
		"protocolVersion": protocolVersion,
		"routingType":     routingType,
	}
}

func cmdIPBlockCreate(s *Session, _ []string) error {
	site, err := s.Resolver.Resolve(context.Background(), "site", "Site")
	if err != nil {
		return err
	}
	name, err := PromptText("IP block name", true)
	if err != nil {
		return err
	}
	protocolVersion, err := PromptChoice("Protocol version", ipBlockProtocolVersions, ipBlockProtocolVersions[0])
	if err != nil {
		return err
	}
	routingType, err := PromptChoice("Routing type", ipBlockRoutingTypes, ipBlockRoutingTypes[0])
	if err != nil {
		return err
	}
	prefix, err := PromptText("Prefix (e.g. 10.0.0.0)", true)
	if err != nil {
		return err
	}
	prefixLen, err := PromptText("Prefix length (e.g. 16)", true)
	if err != nil {
		return err
	}
	pl, err := strconv.Atoi(strings.TrimSpace(prefixLen))
	if err != nil {
		return fmt.Errorf("prefix length must be a number, got %q", prefixLen)
	}
	if err := validateIPBlockPrefixLength(protocolVersion, pl); err != nil {
		return err
	}
	LogCmd(s, "ip-block", "create",
		"--name", name,
		"--site-id", site.ID,
		"--protocol-version", protocolVersion,
		"--routing-type", routingType,
		"--prefix", prefix,
		"--prefix-length", prefixLen,
	)
	body := buildIPBlockCreateBody(name, site.ID, prefix, pl, protocolVersion, routingType)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "ipblock"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating IP block: %w", err)
	}
	s.Cache.Invalidate("ip-block")
	created, err := parseMutationResponseRequiringID(resp, "created IP block")
	if err != nil {
		return err
	}
	fmt.Printf("%s IP block created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdIPBlockUpdate(s *Session, args []string) error {
	block, err := s.Resolver.ResolveWithArgs(context.Background(), "ip-block", "IP Block to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("IP block name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}
	LogCmd(s, "ip-block", "update", block.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "ipblock/{id}"), map[string]string{"id": block.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating IP block: %w", err)
	}
	s.Cache.Invalidate("ip-block")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated IP block")
	if err != nil {
		return err
	}
	fmt.Printf("%s IP block updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdIPBlockDelete(s *Session, args []string) error {
	block, err := s.Resolver.ResolveWithArgs(context.Background(), "ip-block", "IP Block to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete IP block %s (%s)?", block.Name, block.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "ip-block", "delete", block.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "ipblock/{id}"), map[string]string{"id": block.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting IP block: %w", err)
	}
	s.Cache.Invalidate("ip-block")
	fmt.Printf("%s IP block deleted: %s\n", Green("OK"), block.Name)
	return nil
}

func cmdNSGList(s *Session, args []string) error {
	LogCmd(s, "network-security-group", "list")
	items, err := s.Resolver.Fetch(context.Background(), "network-security-group")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tLABELS\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdNSGCreate(s *Session, _ []string) error {
	site, err := s.Resolver.Resolve(context.Background(), "site", "Site")
	if err != nil {
		return err
	}
	name, err := PromptText("Network security group name", true)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{"name": name, "siteId": site.ID}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	LogCmd(s, "network-security-group", "create", "--name", name, "--site-id", site.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "network-security-group"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating network security group: %w", err)
	}
	s.Cache.Invalidate("network-security-group")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created network security group")
	if err != nil {
		return err
	}
	fmt.Printf("%s Network security group created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

func cmdNSGUpdate(s *Session, args []string) error {
	nsg, err := s.Resolver.ResolveWithArgs(context.Background(), "network-security-group", "Network Security Group to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("Network security group name (optional)", false)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	body := map[string]interface{}{}
	if strings.TrimSpace(name) != "" {
		body["name"] = strings.TrimSpace(name)
	}
	if strings.TrimSpace(desc) != "" {
		body["description"] = strings.TrimSpace(desc)
	}
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}
	LogCmd(s, "network-security-group", "update", nsg.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "network-security-group/{id}"), map[string]string{"id": nsg.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating network security group: %w", err)
	}
	s.Cache.Invalidate("network-security-group")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated network security group")
	if err != nil {
		return err
	}
	fmt.Printf("%s Network security group updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdNSGDelete(s *Session, args []string) error {
	nsg, err := s.Resolver.ResolveWithArgs(context.Background(), "network-security-group", "Network Security Group to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete network security group %s (%s)?", nsg.Name, nsg.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "network-security-group", "delete", nsg.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "network-security-group/{id}"), map[string]string{"id": nsg.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting network security group: %w", err)
	}
	s.Cache.Invalidate("network-security-group")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s Network security group deleted: %s\n", Green("OK"), nsg.Name)
	return nil
}

func cmdSKUList(s *Session, _ []string) error {
	LogCmd(s, "sku", "list")
	items, err := s.Resolver.Fetch(context.Background(), "sku")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "DEVICE TYPE\tSITE\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\n", item.Extra["deviceType"], item.Extra["siteId"], item.ID)
	}
	return tw.Flush()
}

func cmdRackList(s *Session, _ []string) error {
	LogCmd(s, "rack", "list")
	items, err := s.Resolver.Fetch(context.Background(), "rack")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tMANUFACTURER\tMODEL\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Extra["manufacturer"], item.Extra["model"], item.ID)
	}
	return tw.Flush()
}

func cmdTrayList(s *Session, _ []string) error {
	siteID, err := requireSiteScope(s, "Tray listing requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	_ = siteID
	LogCmd(s, "tray", "list")
	items, err := s.Resolver.Fetch(context.Background(), "tray")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tTYPE\tPOWER\tFW\tMANUFACTURER\tMODEL\tRACK\tID")
	for _, item := range items {
		rackName := s.Resolver.ResolveID("rack", item.Extra["rackId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
			item.Name, item.Extra["type"], item.Status, item.Extra["firmwareVersion"],
			item.Extra["manufacturer"], item.Extra["model"], rackName, item.ID)
	}
	return tw.Flush()
}

func cmdVPCPrefixList(s *Session, _ []string) error {
	LogCmd(s, "vpc-prefix", "list")
	items, err := s.Resolver.Fetch(context.Background(), "vpc-prefix")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tVPC\tID")
	for _, item := range items {
		vpcName := s.Resolver.ResolveID("vpc", item.Extra["vpcId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, vpcName, item.ID)
	}
	return tw.Flush()
}

func cmdVPCPrefixCreate(s *Session, _ []string) error {
	vpc, err := s.Resolver.Resolve(context.Background(), "vpc", "VPC")
	if err != nil {
		return err
	}
	vpcSiteID := strings.TrimSpace(vpc.Extra["siteId"])
	setSiteScopeFromID(s, vpcSiteID)

	name, err := PromptText("VPC prefix name", true)
	if err != nil {
		return err
	}
	ipBlock, err := promptVPCPrefixIPBlock(context.Background(), s)
	if err != nil {
		return err
	}
	family := vpcprefix.IPFamily(strings.TrimSpace(ipBlock.Extra["protocolVersion"]))
	slaacEnabled, err := vpcPrefixSlaacEnabled(family, vpc)
	if err != nil {
		return err
	}
	maximumLength, knownFamily := family.MaximumPrefixLength(slaacEnabled)
	var promptLabel string
	if knownFamily {
		promptLabel = fmt.Sprintf("%s prefix length (%d-%d)", family, vpcprefix.PrefixLengthMinimum, maximumLength)
	} else {
		promptLabel = fmt.Sprintf("Prefix length (%d-%d; API validates the IP block and VPC limit)", vpcprefix.PrefixLengthMinimum, maximumLength)
	}
	prefixLenText, err := PromptText(promptLabel, true)
	if err != nil {
		return err
	}
	prefixLen, err := strconv.Atoi(strings.TrimSpace(prefixLenText))
	if err != nil {
		return fmt.Errorf("prefix length must be an integer: %w", err)
	}
	err = validateVPCPrefixLength(maximumLength, prefixLen)
	if err != nil {
		return err
	}

	body := map[string]interface{}{
		"name":         name,
		"vpcId":        vpc.ID,
		"ipBlockId":    ipBlock.ID,
		"prefixLength": prefixLen,
	}
	LogCmd(s, "vpc-prefix", "create", "--name", name, "--vpc-id", vpc.ID, "--ip-block-id", ipBlock.ID, "--prefix-length", prefixLenText)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "vpc-prefix"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating VPC prefix: %w", err)
	}
	s.Cache.Invalidate("vpc-prefix")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created VPC prefix")
	if err != nil {
		return err
	}
	fmt.Printf("%s VPC prefix created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

// ipBlockManualEntrySentinel is the select ID used for the trailing "enter
// manually" option in the IP block picker, mirroring tenantManualEntrySentinel.
const ipBlockManualEntrySentinel = "__manual__"

// validateVPCPrefixLength checks the shared minimum and the maximum resolved
// from the selected IP Block family and VPC address mode.
func validateVPCPrefixLength(maximumLength, prefixLength int) error {
	if prefixLength < vpcprefix.PrefixLengthMinimum || prefixLength > maximumLength {
		return fmt.Errorf("prefix length must be between %d and %d", vpcprefix.PrefixLengthMinimum, maximumLength)
	}
	return nil
}

// vpcPrefixSlaacEnabled reads the selected VPC's address mode when it affects
// an IPv6 VPC Prefix. IPv4 and manual block selection do not depend on it.
func vpcPrefixSlaacEnabled(family vpcprefix.IPFamily, vpc *NamedItem) (bool, error) {
	if family != vpcprefix.IPFamilyIPv6 {
		return false, nil
	}
	raw, ok := vpc.Raw.(map[string]interface{})
	if !ok {
		return false, fmt.Errorf("could not determine whether VPC %q uses SLAAC", vpc.Name)
	}
	enabled, ok := raw["slaacEnabled"].(bool)
	if !ok {
		return false, fmt.Errorf("could not determine whether VPC %q uses SLAAC", vpc.Name)
	}
	return enabled, nil
}

// promptVPCPrefixIPBlock picks the IP block for a new VPC prefix. `ipBlockId`
// is required by the API, so list the Ready tenant blocks already scoped to
// the VPC's Site instead of requiring a raw UUID. The selected protocol lets
// the next prompt show the relevant prefix range. Manual entry remains
// available when listing fails, no blocks are visible, or the operator chooses
// the trailing option (NVBug 6105076).
func promptVPCPrefixIPBlock(ctx context.Context, s *Session) (SelectItem, error) {
	blocks, tenantID, err := s.fetchTenantIPBlocks(ctx)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s could not list current tenant IP blocks (%v); falling back to manual entry\n", Dim("note:"), err)
		return promptVPCPrefixIPBlockRaw()
	}
	items := buildIPBlockSelectItems(blocks, tenantID)
	if len(items) == 1 {
		// Only the manual-entry sentinel: no usable tenant IP blocks for this site.
		fmt.Fprintf(os.Stderr, "%s no Ready tenant IP blocks found for this site; create an allocation or enter an IP block ID manually\n", Dim("note:"))
		return promptVPCPrefixIPBlockRaw()
	}
	selected, err := Select("IP block:", items)
	if err != nil {
		return SelectItem{}, err
	}
	if selected.ID == ipBlockManualEntrySentinel {
		return promptVPCPrefixIPBlockRaw()
	}
	return *selected, nil
}

// promptVPCPrefixIPBlockRaw returns a manually entered block without a known
// protocol. The server resolves its family before allocating the prefix.
func promptVPCPrefixIPBlockRaw() (SelectItem, error) {
	raw, err := PromptText("IP block ID", true)
	if err != nil {
		return SelectItem{}, err
	}
	return SelectItem{ID: strings.TrimSpace(raw)}, nil
}

// buildIPBlockSelectItems turns the resolver's Ready tenant IP blocks into
// picker options whose ID is the IP block UUID and whose label surfaces the
// block name (falling back to the UUID when unnamed), protocol version, and
// status. It also preserves the protocol version for the prefix prompt for
// that family. Provider IP blocks and tenant IP blocks that are not Ready
// cannot back a VPC prefix and are skipped. A trailing sentinel for manual
// entry is always appended so the operator can still type a raw UUID for a
// block that isn't listed.
func buildIPBlockSelectItems(blocks []NamedItem, tenantID string) []SelectItem {
	tenantID = strings.TrimSpace(tenantID)
	items := make([]SelectItem, 0, len(blocks)+1)
	for _, b := range blocks {
		if strings.TrimSpace(b.Extra["tenantId"]) != tenantID || !strings.EqualFold(strings.TrimSpace(b.Status), "Ready") {
			continue
		}
		id := strings.TrimSpace(b.ID)
		if id == "" {
			continue
		}
		label := strings.TrimSpace(b.Name)
		if label == "" {
			label = id
		}
		protocolVersion := strings.TrimSpace(b.Extra["protocolVersion"])
		if protocolVersion != "" {
			label += "  " + Dim(protocolVersion)
		}
		if strings.TrimSpace(b.Status) != "" {
			label += "  " + Dim(b.Status)
		}
		items = append(items, SelectItem{
			Label: label,
			ID:    id,
			Extra: map[string]string{"protocolVersion": protocolVersion},
		})
	}
	items = append(items, SelectItem{Label: "Enter IP block ID manually...", ID: ipBlockManualEntrySentinel})
	return items
}

func cmdVPCPrefixUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc-prefix", "VPC Prefix to update", args)
	if err != nil {
		return err
	}
	name, err := PromptText("VPC prefix name", true)
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"name": strings.TrimSpace(name),
	}
	LogCmd(s, "vpc-prefix", "update", item.ID, "--name", strings.TrimSpace(name))
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "vpc-prefix/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating VPC prefix: %w", err)
	}
	s.Cache.Invalidate("vpc-prefix")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated VPC prefix")
	if err != nil {
		return err
	}
	fmt.Printf("%s VPC prefix updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	return nil
}

func cmdVPCPrefixDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc-prefix", "VPC Prefix to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete VPC prefix %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "vpc-prefix", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "vpc-prefix/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting VPC prefix: %w", err)
	}
	s.Cache.Invalidate("vpc-prefix")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s VPC prefix deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdTenantAccountList(s *Session, _ []string) error {
	ctx := context.Background()
	var extraFlags []string
	if id, err := s.getInfrastructureProviderID(ctx); err == nil {
		extraFlags = append(extraFlags, "--infrastructure-provider-id", id)
	} else if id, err := s.getTenantID(ctx); err == nil {
		extraFlags = append(extraFlags, "--tenant-id", id)
	}
	LogCmd(s, append([]string{"tenant-account", "list"}, extraFlags...)...)
	items, err := s.Resolver.Fetch(ctx, "tenant-account")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "TENANT ORG\tSTATUS\tINFRA PROVIDER ID\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, item.Extra["infrastructureProviderId"], item.ID)
	}
	return tw.Flush()
}

func cmdTenantAccountCreate(s *Session, _ []string) error {
	infrastructureProviderID, err := PromptText("Infrastructure provider ID", true)
	if err != nil {
		return err
	}
	tenantOrg, err := PromptText("Tenant org", true)
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"infrastructureProviderId": strings.TrimSpace(infrastructureProviderID),
		"tenantOrg":                strings.TrimSpace(tenantOrg),
	}
	LogCmd(s, "tenant-account", "create", "--tenant-org", strings.TrimSpace(tenantOrg))
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "tenant/account"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating tenant account: %w", err)
	}
	s.Cache.Invalidate("tenant-account")
	created, err := parseMutationResponseRequiringID(resp, "created tenant account")
	if err != nil {
		return err
	}
	fmt.Printf("%s Tenant account created: %s (%s)\n", Green("OK"), str(created, "tenantOrg"), str(created, "id"))
	return nil
}

func cmdTenantAccountUpdate(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tenant-account", "Tenant Account to accept", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Accept invitation for tenant account %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "tenant-account", "update", item.ID)
	bodyJSON, _ := json.Marshal(map[string]interface{}{})
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "tenant/account/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("accepting tenant account invitation: %w", err)
	}
	s.Cache.Invalidate("tenant-account")
	updated, err := parseMutationResponseRequiringID(resp, "accepted tenant account")
	if err != nil {
		return err
	}
	fmt.Printf("%s Tenant account accepted: %s (%s)\n", Green("OK"), str(updated, "tenantOrg"), str(updated, "id"))
	return nil
}

func cmdTenantAccountDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tenant-account", "Tenant Account to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete tenant account %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "tenant-account", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "tenant/account/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting tenant account: %w", err)
	}
	s.Cache.Invalidate("tenant-account")
	fmt.Printf("%s Tenant account deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdExpectedMachineList(s *Session, args []string) error {
	LogCmd(s, "expected-machine", "list")
	items, err := s.Resolver.Fetch(context.Background(), "expected-machine")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "SITE ID\tBMC MAC\tCHASSIS SN\tLABELS\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\n", item.Extra["siteId"], item.Extra["bmcMacAddress"], item.Extra["chassisSerialNumber"], formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdExpectedRackList(s *Session, args []string) error {
	LogCmd(s, "expected-rack", "list")
	items, err := s.Resolver.Fetch(context.Background(), "expected-rack")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tRACK ID\tPROFILE\tSITE\tLABELS\tID")
	for _, item := range items {
		siteName := s.Resolver.ResolveID("site", item.Extra["siteId"])
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\n",
			item.Name, item.Extra["rackId"], item.Extra["rackProfileId"], siteName,
			formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdExpectedSwitchList(s *Session, args []string) error {
	LogCmd(s, "expected-switch", "list")
	items, err := s.Resolver.Fetch(context.Background(), "expected-switch")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSWITCH SN\tBMC MAC\tRACK\tMANUFACTURER\tLABELS\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
			item.Name, item.Extra["switchSerialNumber"], item.Extra["bmcMacAddress"],
			item.Extra["rackId"], item.Extra["manufacturer"],
			formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdExpectedPowerShelfList(s *Session, args []string) error {
	LogCmd(s, "expected-power-shelf", "list")
	items, err := s.Resolver.Fetch(context.Background(), "expected-power-shelf")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSHELF SN\tBMC MAC\tRACK\tMANUFACTURER\tLABELS\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\t%s\t%s\t%s\n",
			item.Name, item.Extra["shelfSerialNumber"], item.Extra["bmcMacAddress"],
			item.Extra["rackId"], item.Extra["manufacturer"],
			formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdInfiniBandPartitionList(s *Session, args []string) error {
	LogCmd(s, "infiniband-partition", "list")
	items, err := s.Resolver.Fetch(context.Background(), "infiniband-partition")
	if err != nil {
		return err
	}
	_, cmdLabels, sortKey, err := parseLabelArgs(args)
	if err != nil {
		return err
	}
	merged, mergeErr := mergeLabels(s.Scope.LabelFilters, cmdLabels)
	if mergeErr != nil {
		return mergeErr
	}
	items = filterByLabels(items, merged)
	if sortKey != "" {
		items = sortByLabelKey(items, sortKey)
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	defer printLabelHint(os.Stderr, items, merged)
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tSTATUS\tLABELS\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Status, formatLabels(item.Labels, 60), item.ID)
	}
	return tw.Flush()
}

func cmdNVLinkLogicalPartitionList(s *Session, _ []string) error {
	LogCmd(s, "nvlink-logical-partition", "list")
	items, err := s.Resolver.Fetch(context.Background(), "nvlink-logical-partition")
	if err != nil {
		return err
	}
	return printResourceTable(os.Stdout, "NAME", "STATUS", "ID", items)
}

func cmdDPUExtensionServiceList(s *Session, _ []string) error {
	LogCmd(s, "dpu-extension-service", "list")
	items, err := s.Resolver.Fetch(context.Background(), "dpu-extension-service")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "NAME\tTYPE\tSITE ID\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Name, item.Extra["serviceType"], item.Extra["siteId"], item.ID)
	}
	return tw.Flush()
}

func cmdAuditList(s *Session, _ []string) error {
	LogCmd(s, "audit", "list")
	items, err := s.Resolver.Fetch(context.Background(), "audit")
	if err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "METHOD\tENDPOINT\tSTATUS CODE\tID")
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\t%s\n", item.Extra["method"], item.Extra["endpoint"], item.Status, item.ID)
	}
	return tw.Flush()
}

// -- Get commands (raw JSON detail) --

func cmdSiteGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "site", "Site", args)
	if err != nil {
		return err
	}
	LogCmd(s, "site", "get", item.ID)
	return getAndPrint(s, apiPath(s, "site/{id}"), item.ID)
}

func cmdVPCGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc", "VPC", args)
	if err != nil {
		return err
	}
	LogCmd(s, "vpc", "get", item.ID)
	return getAndPrint(s, apiPath(s, "vpc/{id}"), item.ID)
}

func cmdSubnetGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "subnet", "Subnet", args)
	if err != nil {
		return err
	}
	LogCmd(s, "subnet", "get", item.ID)
	return getAndPrint(s, apiPath(s, "subnet/{id}"), item.ID)
}

func cmdInstanceGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "instance", "Instance", args)
	if err != nil {
		return err
	}
	LogCmd(s, "instance", "get", item.ID)
	return getAndPrint(s, apiPath(s, "instance/{id}"), item.ID)
}

func cmdInstanceCreate(s *Session, _ []string) error {
	ctx := context.Background()
	vpc, err := s.Resolver.Resolve(ctx, "vpc", "VPC")
	if err != nil {
		return err
	}
	networkConfig, err := instanceNetworkConfigForVPC(vpc)
	if err != nil {
		return err
	}
	vpcSiteID := vpc.Extra["siteId"]
	setSiteScopeFromID(s, vpcSiteID)

	machine, err := promptInstanceMachine(s, ctx, vpcSiteID)
	if err != nil {
		return err
	}
	if networkConfig.detectMultiDPU {
		dpuCapability, capabilityErr := fetchInstanceMultiDPUCapability(s, machine.ID)
		if capabilityErr != nil {
			return capabilityErr
		}
		networkConfig.dpuCapability = dpuCapability
	}
	name, err := PromptText("Instance name", true)
	if err != nil {
		return err
	}

	var osID *string
	osList, osErr := s.Resolver.Fetch(ctx, "operating-system")
	if osErr == nil && len(osList) > 0 {
		useOS, confirmErr := PromptConfirm("Select an operating system?")
		if confirmErr != nil {
			return confirmErr
		}
		if useOS {
			osItem, selectErr := s.Resolver.Resolve(ctx, "operating-system", "Operating System")
			if selectErr != nil {
				return selectErr
			}
			osID = &osItem.ID
		}
	}

	// Scope network-resource lookups to the selected VPC so the picker only
	// offers subnets or VPC prefixes that are attachable to this instance.
	savedVpcID2, savedVpcName2 := s.Scope.VpcID, s.Scope.VpcName
	s.Scope.VpcID, s.Scope.VpcName = vpc.ID, vpc.Name
	s.Cache.InvalidateFiltered()
	defer func() {
		s.Scope.VpcID, s.Scope.VpcName = savedVpcID2, savedVpcName2
		s.Cache.InvalidateFiltered()
	}()

	interfaces, err := promptInstanceInterfaces(s, networkConfig)
	if err != nil {
		return err
	}

	sshKeyGroupIDs, err := promptOptionalResourceIDs(s, ctx, "ssh-key-group", "SSH key group")
	if err != nil {
		return err
	}

	body := map[string]interface{}{
		"name":      name,
		"machineId": machine.ID,
		"vpcId":     vpc.ID,
	}
	if osID != nil {
		body["operatingSystemId"] = *osID
	}
	if len(interfaces) > 0 {
		body["interfaces"] = interfaces
	}
	if networkConfig.autoNetwork {
		body["autoNetwork"] = true
	}
	if len(sshKeyGroupIDs) > 0 {
		body["sshKeyGroupIds"] = sshKeyGroupIDs
	}
	bodyJSON, err := json.Marshal(body)
	if err != nil {
		return fmt.Errorf("encoding instance create request: %w", err)
	}
	LogCmd(s, "instance", "create", "--data", shellQuoteCLIArg(string(bodyJSON)))
	resp, _, err := s.Client.Do("POST", apiPath(s, "instance"), nil, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("creating instance: %w", err)
	}
	s.Cache.Invalidate("instance")
	s.Cache.InvalidateFiltered()
	created, err := parseMutationResponseRequiringID(resp, "created instance")
	if err != nil {
		return err
	}
	fmt.Printf("%s Instance created: %s (%s)\n", Green("OK"), str(created, "name"), str(created, "id"))
	return nil
}

type instanceNetworkConfig struct {
	autoNetwork    bool
	detectMultiDPU bool
	dpuCapability  *instanceDPUDeviceNetworkCapability
	resourceType   string
	reuseResources bool
	singular       string
	plural         string
	selectorKey    string
}

func instanceNetworkConfigForVPC(vpc *NamedItem) (instanceNetworkConfig, error) {
	if vpc == nil {
		return instanceNetworkConfig{}, fmt.Errorf("selected VPC is missing")
	}

	virtualizationType := vpc.Extra["networkVirtualizationType"]
	switch virtualizationType {
	case "ETHERNET_VIRTUALIZER":
		return instanceNetworkConfig{
			resourceType: "subnet",
			singular:     "Subnet",
			plural:       "subnets",
			selectorKey:  "subnetId",
		}, nil
	case "FNN":
		return instanceNetworkConfig{
			detectMultiDPU: true,
			resourceType:   "vpc-prefix",
			reuseResources: true,
			singular:       "VPC prefix",
			plural:         "VPC prefixes",
			selectorKey:    "vpcPrefixId",
		}, nil
	case "FLAT":
		return instanceNetworkConfig{
			autoNetwork: true,
		}, nil
	case "":
		return instanceNetworkConfig{}, fmt.Errorf("selected VPC has no network virtualization type")
	default:
		return instanceNetworkConfig{}, fmt.Errorf(
			"instance creation does not support VPC network virtualization type %q",
			virtualizationType,
		)
	}
}

type instanceDPUDeviceNetworkCapability struct {
	name  string
	count int
}

func fetchInstanceMultiDPUCapability(s *Session, machineID string) (*instanceDPUDeviceNetworkCapability, error) {
	body, _, err := s.Client.Do(
		"GET",
		apiPath(s, "machine/{id}"),
		map[string]string{
			"id": machineID,
		},
		nil,
		nil,
	)
	if err != nil {
		return nil, fmt.Errorf("fetching capabilities for machine %s: %w", machineID, err)
	}

	var machine struct {
		MachineCapabilities []struct {
			Type       string `json:"type"`
			Name       string `json:"name"`
			Count      *int   `json:"count"`
			DeviceType string `json:"deviceType"`
		} `json:"machineCapabilities"`
	}
	err = json.Unmarshal(body, &machine)
	if err != nil {
		return nil, fmt.Errorf("parsing capabilities for machine %s: %w", machineID, err)
	}

	for _, capability := range machine.MachineCapabilities {
		if !strings.EqualFold(capability.Type, "Network") {
			continue
		}
		if !strings.EqualFold(capability.DeviceType, "DPU") {
			continue
		}
		if capability.Count == nil || *capability.Count <= 1 {
			continue
		}
		name := capability.Name
		if name == "" {
			continue
		}
		return &instanceDPUDeviceNetworkCapability{
			name:  name,
			count: *capability.Count,
		}, nil
	}
	return nil, nil
}

// promptInstanceInterfaces builds the interfaces[] array for an instance
// create request using the selected VPC's network configuration. Ethernet
// virtualizer VPCs use subnets, FNN VPCs use VPC prefixes, and Flat VPCs use
// autoNetwork without explicit interfaces. For interface-backed VPCs, the
// first interface is always prompted; subsequent interfaces are opt-in.
func promptInstanceInterfaces(s *Session, networkConfig instanceNetworkConfig) ([]map[string]interface{}, error) {
	if networkConfig.autoNetwork {
		return nil, nil
	}
	readyItems, err := fetchReadyInstanceNetworkResources(s, networkConfig)
	if err != nil {
		return nil, fmt.Errorf("listing %s for selected VPC: %w", networkConfig.plural, err)
	}
	if len(readyItems) == 0 {
		return nil, fmt.Errorf("no Ready %s available for selected VPC", networkConfig.plural)
	}
	if networkConfig.dpuCapability != nil {
		return promptMultiDPUInstanceInterfaces(s, networkConfig, readyItems)
	}
	var ifaces []map[string]interface{}
	usedResourceIDs := make(map[string]bool)
	usedVirtualFunctionIDs := make(map[int]bool)
	for {
		label := networkConfig.singular + " for interface"
		if len(ifaces) > 0 {
			if len(usedVirtualFunctionIDs) == virtualFunctionIDCount {
				return ifaces, nil
			}
			confirmLabel := fmt.Sprintf("Add another interface (have %d)?", len(ifaces))
			more, confirmErr := PromptConfirm(confirmLabel)
			if confirmErr != nil {
				return ifaces, confirmErr
			}
			if !more {
				return ifaces, nil
			}
		}
		available := readyItems
		if !networkConfig.reuseResources {
			available = make([]NamedItem, 0, len(readyItems))
			for _, item := range readyItems {
				if !usedResourceIDs[item.ID] {
					available = append(available, item)
				}
			}
		}
		if len(available) == 0 {
			fmt.Fprintf(os.Stderr, "%s no more %s to attach\n", Dim("note:"), networkConfig.plural)
			return ifaces, nil
		}
		picked, err := s.Resolver.SelectFromItems(label, available)
		if err != nil {
			return ifaces, err
		}
		if !networkConfig.reuseResources {
			usedResourceIDs[picked.ID] = true
		}
		isPhysical := len(ifaces) == 0
		iface := map[string]interface{}{
			networkConfig.selectorKey: picked.ID,
			"isPhysical":              isPhysical,
		}
		if !isPhysical {
			virtualFunctionID, promptErr := promptVirtualFunctionID(
				"Virtual function ID (0-15)",
				usedVirtualFunctionIDs,
			)
			if promptErr != nil {
				return ifaces, promptErr
			}
			iface["virtualFunctionId"] = virtualFunctionID
		}
		ifaces = append(ifaces, iface)
	}
}

func fetchReadyInstanceNetworkResources(s *Session, networkConfig instanceNetworkConfig) ([]NamedItem, error) {
	query := map[string]string{
		"orderBy": "NAME_ASC",
		"status":  "Ready",
	}
	if s.Scope.SiteID != "" {
		query["siteId"] = s.Scope.SiteID
	}
	if s.Scope.VpcID != "" {
		query["vpcId"] = s.Scope.VpcID
	}

	resources, err := s.fetchAll(apiPath(s, networkConfig.resourceType), query)
	if err != nil {
		return nil, err
	}
	items := make([]NamedItem, len(resources))
	for i, resource := range resources {
		items[i] = NamedItem{
			Name: str(resource, "name"),
			ID:   str(resource, "id"),
			Raw:  resource,
		}
	}
	return items, nil
}

const (
	virtualFunctionIDMinimum = 0
	virtualFunctionIDMaximum = 15
	virtualFunctionIDCount   = virtualFunctionIDMaximum - virtualFunctionIDMinimum + 1
)

type deviceVirtualFunctionIDs struct {
	used map[int]bool
}

func (vfIDs deviceVirtualFunctionIDs) exhausted() bool {
	return len(vfIDs.used) == virtualFunctionIDCount
}

func promptMultiDPUInstanceInterfaces(s *Session, networkConfig instanceNetworkConfig, readyItems []NamedItem) ([]map[string]interface{}, error) {
	capability := networkConfig.dpuCapability
	if capability == nil {
		return nil, fmt.Errorf("multi-DPU interface prompting requires a DPU capability")
	}
	ifaces := make([]map[string]interface{}, 0, capability.count)
	for deviceInstance := range capability.count {
		if deviceInstance > 0 {
			configureDevice, confirmErr := PromptConfirm(fmt.Sprintf("Configure DPU %d?", deviceInstance))
			if confirmErr != nil {
				return ifaces, confirmErr
			}
			if !configureDevice {
				return ifaces, nil
			}
		}

		physical, err := selectDPUInterfaceResource(
			s,
			readyItems,
			fmt.Sprintf("%s for DPU %d physical interface", networkConfig.singular, deviceInstance),
		)
		if err != nil {
			return ifaces, err
		}
		ifaces = append(ifaces, map[string]interface{}{
			networkConfig.selectorKey: physical.ID,
			"device":                  capability.name,
			"deviceInstance":          deviceInstance,
			"isPhysical":              true,
		})

		vfIDs := deviceVirtualFunctionIDs{
			used: make(map[int]bool),
		}
		for !vfIDs.exhausted() {
			more, confirmErr := PromptConfirm(fmt.Sprintf(
				"Add a virtual function for DPU %d (configured functions: %d)?",
				deviceInstance,
				countInterfacesForDevice(ifaces, deviceInstance),
			))
			if confirmErr != nil {
				return ifaces, confirmErr
			}
			if !more {
				break
			}

			virtual, selectErr := selectDPUInterfaceResource(
				s,
				readyItems,
				fmt.Sprintf("%s for DPU %d virtual interface", networkConfig.singular, deviceInstance),
			)
			if selectErr != nil {
				return ifaces, selectErr
			}
			virtualFunctionID, promptErr := promptVirtualFunctionID(
				fmt.Sprintf("Virtual function ID for DPU %d (0-15)", deviceInstance),
				vfIDs.used,
			)
			if promptErr != nil {
				return ifaces, promptErr
			}
			iface := map[string]interface{}{
				networkConfig.selectorKey: virtual.ID,
				"device":                  capability.name,
				"deviceInstance":          deviceInstance,
				"isPhysical":              false,
				"virtualFunctionId":       virtualFunctionID,
			}
			ifaces = append(ifaces, iface)
		}
	}
	return ifaces, nil
}

func selectDPUInterfaceResource(
	s *Session,
	readyItems []NamedItem,
	label string,
) (*NamedItem, error) {
	picked, err := s.Resolver.SelectFromItems(label, readyItems)
	if err != nil {
		return nil, err
	}
	return picked, nil
}

func countInterfacesForDevice(ifaces []map[string]interface{}, deviceInstance int) int {
	count := 0
	for _, iface := range ifaces {
		if iface["deviceInstance"] == deviceInstance {
			count++
		}
	}
	return count
}

func promptVirtualFunctionID(label string, used map[int]bool) (int, error) {
	for {
		valueText, err := PromptText(label, true)
		if err != nil {
			return 0, err
		}
		value, err := strconv.Atoi(valueText)
		if err != nil || value < virtualFunctionIDMinimum || value > virtualFunctionIDMaximum {
			fmt.Println(Red("  (required; must be an integer from 0 to 15)"))
			continue
		}
		if used[value] {
			fmt.Println(Red("  (must be unique among virtual interfaces on this device)"))
			continue
		}
		used[value] = true
		return value, nil
	}
}

// promptOptionalResourceIDs offers a series of single-select pickers on the
// given resource type, accumulating IDs until the user declines to add
// another. Returns nil if the resource type has no items at all (so callers
// can omit the field entirely from the request body).
func promptOptionalResourceIDs(s *Session, ctx context.Context, resourceType, singular string) ([]string, error) {
	items, err := s.Resolver.Fetch(ctx, resourceType)
	if err != nil {
		fmt.Fprintf(os.Stderr, "%s could not list %s (%v); skipping\n", Dim("note:"), resourceType, err)
		return nil, nil
	}
	if len(items) == 0 {
		return nil, nil
	}
	var ids []string
	picked := make(map[string]bool)
	for {
		prompt := fmt.Sprintf("Add %s?", singular)
		if len(ids) > 0 {
			prompt = fmt.Sprintf("Add another %s (have %d)?", singular, len(ids))
		}
		more, err := PromptConfirm(prompt)
		if err != nil {
			return ids, err
		}
		if !more {
			return ids, nil
		}
		available := make([]NamedItem, 0, len(items))
		for _, item := range items {
			if !picked[item.ID] {
				available = append(available, item)
			}
		}
		if len(available) == 0 {
			fmt.Fprintf(os.Stderr, "%s no more %s available\n", Dim("note:"), resourceType)
			return ids, nil
		}
		selected, err := s.Resolver.SelectFromItems(singular, available)
		if err != nil {
			return ids, err
		}
		ids = append(ids, selected.ID)
		picked[selected.ID] = true
	}
}

type instanceAttributeUpdateInputs struct {
	name           string
	description    string
	osID           string
	sshKeyGroupIDs []string
}

func (u instanceAttributeUpdateInputs) attributeBody() map[string]interface{} {
	body := map[string]interface{}{}
	if strings.TrimSpace(u.name) != "" {
		body["name"] = strings.TrimSpace(u.name)
	}
	if strings.TrimSpace(u.description) != "" {
		body["description"] = strings.TrimSpace(u.description)
	}
	if strings.TrimSpace(u.osID) != "" {
		body["operatingSystemId"] = strings.TrimSpace(u.osID)
	}
	if len(u.sshKeyGroupIDs) > 0 {
		body["sshKeyGroupIds"] = u.sshKeyGroupIDs
	}
	return body
}

type instanceRebootInputs struct {
	rebootWithCustomIpxe bool
	applyUpdatesOnReboot bool
}

func (u instanceRebootInputs) rebootBody() map[string]interface{} {
	body := map[string]interface{}{"triggerReboot": true}
	if u.rebootWithCustomIpxe {
		body["rebootWithCustomIpxe"] = true
	}
	if u.applyUpdatesOnReboot {
		body["applyUpdatesOnReboot"] = true
	}
	return body
}

func cmdInstanceUpdate(s *Session, args []string) error {
	ctx := context.Background()
	item, err := s.Resolver.ResolveWithArgs(ctx, "instance", "Instance to update", args)
	if err != nil {
		return err
	}
	inputs := instanceAttributeUpdateInputs{}
	inputs.name, err = PromptText("New name (optional)", false)
	if err != nil {
		return err
	}
	inputs.description, err = PromptText("New description (optional)", false)
	if err != nil {
		return err
	}

	if osList, osErr := s.Resolver.Fetch(ctx, "operating-system"); osErr == nil && len(osList) > 0 {
		changeOS, confirmErr := PromptConfirm("Change operating system?")
		if confirmErr != nil {
			return confirmErr
		}
		if changeOS {
			osItem, selectErr := s.Resolver.Resolve(ctx, "operating-system", "Operating System")
			if selectErr != nil {
				return selectErr
			}
			inputs.osID = osItem.ID
		}
	}

	rotateGroups, err := PromptConfirm("Replace ssh key groups (this overwrites the existing list)?")
	if err != nil {
		return err
	}
	if rotateGroups {
		inputs.sshKeyGroupIDs, err = promptOptionalResourceIDs(s, ctx, "ssh-key-group", "SSH key group")
		if err != nil {
			return err
		}
	}

	body := inputs.attributeBody()
	if len(body) == 0 {
		return fmt.Errorf("no updates provided")
	}

	LogCmd(s, "instance", "update", item.ID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "instance/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating instance: %w", err)
	}
	s.Cache.Invalidate("instance")
	s.Cache.InvalidateFiltered()
	updated, err := parseMutationResponseRequiringID(resp, "updated instance")
	if err != nil {
		return err
	}
	fmt.Printf("%s Instance updated: %s (%s)\n", Green("OK"), str(updated, "name"), str(updated, "id"))
	fmt.Fprintf(os.Stderr, "%s run `instance reboot` when ready\n", Dim("note:"))
	return nil
}

func cmdInstanceReboot(s *Session, args []string) error {
	ctx := context.Background()
	item, err := s.Resolver.ResolveWithArgs(ctx, "instance", "Instance to reboot", args)
	if err != nil {
		return err
	}
	rebootWithCustomIpxe, err := PromptConfirm("Reboot with custom iPXE (one-time)?")
	if err != nil {
		return err
	}
	applyUpdatesOnReboot, err := PromptConfirm("Apply pending updates on reboot?")
	if err != nil {
		return err
	}
	confirm, err := PromptConfirm(fmt.Sprintf("Reboot instance %s (%s) now?", item.Name, item.ID))
	if err != nil || !confirm {
		return err
	}

	body := instanceRebootInputs{
		rebootWithCustomIpxe: rebootWithCustomIpxe,
		applyUpdatesOnReboot: applyUpdatesOnReboot,
	}.rebootBody()

	LogCmd(s, "instance", "update", item.ID, "--trigger-reboot=true")
	bodyJSON, _ := json.Marshal(body)
	_, _, err = s.Client.Do("PATCH", apiPath(s, "instance/{id}"), map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("rebooting instance: %w", err)
	}
	s.Cache.Invalidate("instance")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s Reboot requested for instance %s (%s)\n", Green("OK"), item.Name, item.ID)
	return nil
}

func cmdInstanceDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "instance", "Instance to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete instance %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "instance", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "instance/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting instance: %w", err)
	}
	s.Cache.Invalidate("instance")
	s.Cache.InvalidateFiltered()
	fmt.Printf("%s Instance deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdMachineGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "machine", "Machine", args)
	if err != nil {
		return err
	}
	LogCmd(s, "machine", "get", item.ID)
	body, _, err := s.Client.Do("GET", apiPath(s, "machine/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return err
	}
	printMachineHealthSummary(os.Stdout, body)
	return printDetailJSON(os.Stdout, body)
}

// cmdMachineDpuGet prints the DPU machines attached to a host machine.
func cmdMachineDpuGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "machine", "Machine", args)
	if err != nil {
		return err
	}
	LogCmd(s, "machine", "dpu get", item.ID)
	body, _, err := s.Client.Do("GET", apiPath(s, "machine/{id}/dpu"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return err
	}
	var dpus []json.RawMessage
	if err := json.Unmarshal(body, &dpus); err == nil {
		fmt.Fprintf(os.Stdout, "%d DPU(s) attached\n", len(dpus))
	}
	return printDetailJSON(os.Stdout, body)
}

// printMachineHealthSummary prints a human-readable summary of blocking
// health alerts and tenant-usability state above the verbose JSON. Operators
// triaging an Error-state machine should not have to scan multi-page JSON to
// find the blocker -- the summary surfaces the most actionable signals (id,
// target, classifications, short message, isUsableByTenant) up front. When
// there are no blocking alerts the summary block is suppressed entirely so
// healthy-machine output stays unchanged.
func printMachineHealthSummary(w io.Writer, body []byte) {
	var raw map[string]interface{}
	if err := json.Unmarshal(body, &raw); err != nil {
		return
	}
	alerts := extractBlockingAlerts(raw)
	if len(alerts) == 0 {
		return
	}
	status := strings.TrimSpace(str(raw, "status"))
	usable, hasUsable := raw["isUsableByTenant"].(bool)

	fmt.Fprintln(w, Bold("Blocking health alerts:"))
	if status != "" {
		fmt.Fprintf(w, "  Status: %s\n", status)
	}
	if hasUsable {
		fmt.Fprintf(w, "  Usable by tenant: %t\n", usable)
	}
	for i, a := range alerts {
		fmt.Fprintf(w, "  [%d] %s\n", i+1, a.ID)
		if t := strings.TrimSpace(a.Target); t != "" {
			fmt.Fprintf(w, "      Target: %s\n", t)
		}
		if len(a.Classifications) > 0 {
			fmt.Fprintf(w, "      Classifications: %s\n", strings.Join(a.Classifications, ", "))
		}
		if msg := shortMessage(a.Message); msg != "" {
			fmt.Fprintf(w, "      Message: %s\n", msg)
		}
	}
	fmt.Fprintln(w)
}

// shortMessage trims an alert message to a single readable line. Health
// alerts often include multi-line probe output; the first non-empty line is
// usually the actionable summary, so we surface that and indicate truncation
// when more lines follow.
func shortMessage(msg string) string {
	msg = strings.TrimSpace(msg)
	if msg == "" {
		return ""
	}
	lines := strings.Split(msg, "\n")
	first := strings.TrimSpace(lines[0])
	if first == "" {
		for _, l := range lines[1:] {
			if t := strings.TrimSpace(l); t != "" {
				first = t
				break
			}
		}
	}
	const maxLen = 200
	if len(first) > maxLen {
		first = first[:maxLen-3] + "..."
	}
	if len(lines) > 1 && strings.TrimSpace(strings.Join(lines[1:], "")) != "" {
		first += " (...)"
	}
	return first
}

func cmdOSGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "operating-system", "Operating System", args)
	if err != nil {
		return err
	}
	LogCmd(s, "operating-system", "get", item.ID)
	return getAndPrint(s, apiPath(s, "operating-system/{id}"), item.ID)
}

func cmdSSHKeyGroupGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ssh-key-group", "SSH Key Group", args)
	if err != nil {
		return err
	}
	LogCmd(s, "ssh-key-group", "get", item.ID)
	return getAndPrint(s, apiPath(s, "sshkeygroup/{id}"), item.ID)
}

func cmdSSHKeyGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ssh-key", "SSH Key", args)
	if err != nil {
		return err
	}
	LogCmd(s, "ssh-key", "get", item.ID)
	return getAndPrint(s, apiPath(s, "sshkey/{id}"), item.ID)
}

func cmdAllocationGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "allocation", "Allocation", args)
	if err != nil {
		return err
	}
	LogCmd(s, "allocation", "get", item.ID)
	return getAndPrint(s, apiPath(s, "allocation/{id}"), item.ID)
}

func cmdAllocationDelete(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "allocation", "Allocation to delete", args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete allocation %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	LogCmd(s, "allocation", "delete", item.ID)
	_, _, err = s.Client.Do("DELETE", apiPath(s, "allocation/{id}"), map[string]string{"id": item.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting allocation: %w", err)
	}
	s.Cache.Invalidate("allocation")
	fmt.Printf("%s Allocation deleted: %s\n", Green("OK"), item.Name)
	return nil
}

func cmdIPBlockGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "ip-block", "IP Block", args)
	if err != nil {
		return err
	}
	LogCmd(s, "ip-block", "get", item.ID)
	return getAndPrint(s, apiPath(s, "ipblock/{id}"), item.ID)
}

func cmdNSGGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "network-security-group", "Network Security Group", args)
	if err != nil {
		return err
	}
	LogCmd(s, "network-security-group", "get", item.ID)
	return getAndPrint(s, apiPath(s, "network-security-group/{id}"), item.ID)
}

func cmdSKUGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "sku", "SKU", args)
	if err != nil {
		return err
	}
	LogCmd(s, "sku", "get", item.ID)
	return getAndPrint(s, apiPath(s, "sku/{id}"), item.ID)
}

func cmdRackGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "rack", "Rack", args)
	if err != nil {
		return err
	}
	LogCmd(s, "rack", "get", item.ID)
	return getAndPrint(s, apiPath(s, "rack/{id}"), item.ID)
}

func cmdTrayGet(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Tray get requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tray", "Tray", args)
	if err != nil {
		return err
	}
	LogCmd(s, "tray", "get", item.ID, "--site-id", siteID)
	body, _, err := s.Client.Do("GET", apiPath(s, "tray/{id}"),
		map[string]string{"id": item.ID},
		map[string]string{"siteId": siteID}, nil)
	if err != nil {
		return err
	}
	return printDetailJSON(os.Stdout, body)
}

// powerStateChoices is the canonical list accepted by every power-control
// endpoint (see UpdatePowerStateRequest in OpenAPI). Kept in one place so
// rack and tray commands cannot drift from each other.
var powerStateChoices = []string{"on", "off", "cycle", "forceoff", "forcecycle"}

// printTaskIDs renders the standard taskIds-bearing response from a
// lifecycle action. Action endpoints return one task ID per affected
// component, which operators feed into rack task get to track progress.
// The full JSON response is also printed so operators can see any extra
// fields the server may add.
func printTaskIDs(body []byte, action string) error {
	var resp struct {
		TaskIDs []string `json:"taskIds"`
	}
	if err := json.Unmarshal(body, &resp); err == nil && len(resp.TaskIDs) > 0 {
		fmt.Printf("%s %s started; %d task(s):\n", Green("OK"), action, len(resp.TaskIDs))
		for _, id := range resp.TaskIDs {
			fmt.Printf("  %s\n", id)
		}
		fmt.Println()
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdRackBringup(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Rack bringup requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "rack", "Rack to bring up", args)
	if err != nil {
		return err
	}
	desc, err := PromptText("Description (optional)", false)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Bring up rack %s (%s)?", item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	body := map[string]interface{}{"siteId": siteID}
	if d := strings.TrimSpace(desc); d != "" {
		body["description"] = d
	}
	LogCmd(s, "rack", "bringup", item.ID, "--site-id", siteID)
	bodyJSON, _ := json.Marshal(body)
	resp, _, err := s.Client.Do("POST", apiPath(s, "rack/{id}/bringup"),
		map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("bringing up rack: %w", err)
	}
	return printTaskIDs(resp, "Rack bringup")
}

func cmdRackPower(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Rack power control requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "rack", "Rack to power-control", args)
	if err != nil {
		return err
	}
	state, err := PromptChoice("Power state", powerStateChoices, "")
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Apply power state %q to rack %s (%s)?", state, item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	bodyJSON, _ := json.Marshal(map[string]interface{}{"siteId": siteID, "state": state})
	LogCmd(s, "rack", "power", item.ID, "--state", state, "--site-id", siteID)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "rack/{id}/power"),
		map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("powering rack: %w", err)
	}
	return printTaskIDs(resp, "Rack power")
}

func cmdRackFirmware(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Rack firmware update requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "rack", "Rack to update firmware on", args)
	if err != nil {
		return err
	}
	version, err := PromptText("Target firmware version (blank for default/latest)", false)
	if err != nil {
		return err
	}
	versionDisplay := strings.TrimSpace(version)
	if versionDisplay == "" {
		versionDisplay = "<default>"
	}
	ok, err := PromptConfirm(fmt.Sprintf("Update firmware to %s on rack %s (%s)?", versionDisplay, item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	body := map[string]interface{}{"siteId": siteID}
	if v := strings.TrimSpace(version); v != "" {
		body["version"] = v
	}
	bodyJSON, _ := json.Marshal(body)
	logArgs := []string{"rack", "firmware", item.ID, "--site-id", siteID}
	if v := strings.TrimSpace(version); v != "" {
		logArgs = append(logArgs, "--version", v)
	}
	LogCmd(s, logArgs...)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "rack/{id}/firmware"),
		map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating rack firmware: %w", err)
	}
	return printTaskIDs(resp, "Rack firmware update")
}

func cmdRackValidate(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Rack validation requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "rack", "Rack to validate", args)
	if err != nil {
		return err
	}
	LogCmd(s, "rack", "validate", item.ID, "--site-id", siteID)
	body, _, err := s.Client.Do("GET", apiPath(s, "rack/{id}/validation"),
		map[string]string{"id": item.ID},
		map[string]string{"siteId": siteID}, nil)
	if err != nil {
		return fmt.Errorf("validating rack: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTrayPower(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Tray power control requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tray", "Tray to power-control", args)
	if err != nil {
		return err
	}
	state, err := PromptChoice("Power state", powerStateChoices, "")
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Apply power state %q to tray %s (%s)?", state, item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	bodyJSON, _ := json.Marshal(map[string]interface{}{"siteId": siteID, "state": state})
	LogCmd(s, "tray", "power", item.ID, "--state", state, "--site-id", siteID)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "tray/{id}/power"),
		map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("powering tray: %w", err)
	}
	return printTaskIDs(resp, "Tray power")
}

func cmdTrayFirmware(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Tray firmware update requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tray", "Tray to update firmware on", args)
	if err != nil {
		return err
	}
	version, err := PromptText("Target firmware version (blank for default/latest)", false)
	if err != nil {
		return err
	}
	versionDisplay := strings.TrimSpace(version)
	if versionDisplay == "" {
		versionDisplay = "<default>"
	}
	ok, err := PromptConfirm(fmt.Sprintf("Update firmware to %s on tray %s (%s)?", versionDisplay, item.Name, item.ID))
	if err != nil || !ok {
		return err
	}
	body := map[string]interface{}{"siteId": siteID}
	if v := strings.TrimSpace(version); v != "" {
		body["version"] = v
	}
	bodyJSON, _ := json.Marshal(body)
	logArgs := []string{"tray", "firmware", item.ID, "--site-id", siteID}
	if v := strings.TrimSpace(version); v != "" {
		logArgs = append(logArgs, "--version", v)
	}
	LogCmd(s, logArgs...)
	resp, _, err := s.Client.Do("PATCH", apiPath(s, "tray/{id}/firmware"),
		map[string]string{"id": item.ID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("updating tray firmware: %w", err)
	}
	return printTaskIDs(resp, "Tray firmware update")
}

func cmdTrayValidate(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Tray validation requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tray", "Tray to validate", args)
	if err != nil {
		return err
	}
	LogCmd(s, "tray", "validate", item.ID, "--site-id", siteID)
	body, _, err := s.Client.Do("GET", apiPath(s, "tray/{id}/validation"),
		map[string]string{"id": item.ID},
		map[string]string{"siteId": siteID}, nil)
	if err != nil {
		return fmt.Errorf("validating tray: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdRackTaskGet(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Task get requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	taskID, err := taskIDFromArgsOrPrompt(args, "Task ID")
	if err != nil {
		return err
	}
	LogCmd(s, "rack", "task", "get", taskID, "--site-id", siteID)
	body, _, err := s.Client.Do("GET", apiPath(s, "rack/task/{id}"),
		map[string]string{"id": taskID},
		map[string]string{"siteId": siteID}, nil)
	if err != nil {
		return fmt.Errorf("getting task: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdRackTaskCancel(s *Session, args []string) error {
	siteID, err := requireSiteScope(s, "Task cancel requires a site filter. Select a site.")
	if err != nil {
		return err
	}
	taskID, err := taskIDFromArgsOrPrompt(args, "Task ID to cancel")
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Cancel task %s?", taskID))
	if err != nil || !ok {
		return err
	}
	bodyJSON, _ := json.Marshal(map[string]interface{}{"siteId": siteID})
	LogCmd(s, "rack", "task", "cancel", taskID, "--site-id", siteID)
	resp, _, err := s.Client.Do("POST", apiPath(s, "rack/task/{id}/cancel"),
		map[string]string{"id": taskID}, nil, bodyJSON)
	if err != nil {
		return fmt.Errorf("cancelling task: %w", err)
	}
	fmt.Printf("%s Cancellation requested for task %s\n", Green("OK"), taskID)
	return printDetailJSON(os.Stdout, resp)
}

// taskIDFromArgsOrPrompt accepts a task ID from a positional argument when
// supplied (e.g. `rack task get <task-id>`) or interactively prompts the
// operator when not. Tasks are not pre-listed by the TUI -- IDs come from the
// taskIds output of preceding lifecycle actions, so resolver-style picking
// does not apply.
func taskIDFromArgsOrPrompt(args []string, label string) (string, error) {
	if len(args) > 0 && strings.TrimSpace(args[0]) != "" {
		return strings.TrimSpace(args[0]), nil
	}
	id, err := PromptText(label, true)
	if err != nil {
		return "", err
	}
	id = strings.TrimSpace(id)
	if id == "" {
		return "", fmt.Errorf("task ID is required")
	}
	return id, nil
}

func cmdVPCPrefixGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "vpc-prefix", "VPC Prefix", args)
	if err != nil {
		return err
	}
	LogCmd(s, "vpc-prefix", "get", item.ID)
	return getAndPrint(s, apiPath(s, "vpc-prefix/{id}"), item.ID)
}

func cmdTenantAccountGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "tenant-account", "Tenant Account", args)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-account", "get", item.ID)
	return getAndPrint(s, apiPath(s, "tenant/account/{id}"), item.ID)
}

func cmdExpectedMachineGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "expected-machine", "Expected Machine", args)
	if err != nil {
		return err
	}
	LogCmd(s, "expected-machine", "get", item.ID)
	return getAndPrint(s, apiPath(s, "expected-machine/{id}"), item.ID)
}

func cmdExpectedRackGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "expected-rack", "Expected Rack", args)
	if err != nil {
		return err
	}
	LogCmd(s, "expected-rack", "get", item.ID)
	return getAndPrint(s, apiPath(s, "expected-rack/{id}"), item.ID)
}

func cmdExpectedSwitchGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "expected-switch", "Expected Switch", args)
	if err != nil {
		return err
	}
	LogCmd(s, "expected-switch", "get", item.ID)
	return getAndPrint(s, apiPath(s, "expected-switch/{id}"), item.ID)
}

func cmdExpectedPowerShelfGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "expected-power-shelf", "Expected Power Shelf", args)
	if err != nil {
		return err
	}
	LogCmd(s, "expected-power-shelf", "get", item.ID)
	return getAndPrint(s, apiPath(s, "expected-power-shelf/{id}"), item.ID)
}

func cmdInfiniBandPartitionGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "infiniband-partition", "InfiniBand Partition", args)
	if err != nil {
		return err
	}
	LogCmd(s, "infiniband-partition", "get", item.ID)
	return getAndPrint(s, apiPath(s, "infiniband-partition/{id}"), item.ID)
}

func cmdNVLinkLogicalPartitionGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "nvlink-logical-partition", "NVLink Logical Partition", args)
	if err != nil {
		return err
	}
	LogCmd(s, "nvlink-logical-partition", "get", item.ID)
	return getAndPrint(s, apiPath(s, "nvlink-logical-partition/{id}"), item.ID)
}

func cmdDPUExtensionServiceGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "dpu-extension-service", "DPU Extension Service", args)
	if err != nil {
		return err
	}
	LogCmd(s, "dpu-extension-service", "get", item.ID)
	return getAndPrint(s, apiPath(s, "dpu-extension-service/{id}"), item.ID)
}

func cmdAuditGet(s *Session, args []string) error {
	item, err := s.Resolver.ResolveWithArgs(context.Background(), "audit", "Audit Entry", args)
	if err != nil {
		return err
	}
	LogCmd(s, "audit", "get", item.ID)
	return getAndPrint(s, apiPath(s, "audit/{id}"), item.ID)
}

func tenantIdentitySitePath(s *Session, tail string) string {
	return fmt.Sprintf("/v2/org/{org}/%s/site/{siteID}/%s", s.Client.APIName, tail)
}

func resolveTenantIdentitySite(s *Session, ctx context.Context, args []string) (*NamedItem, error) {
	if len(args) > 0 && strings.TrimSpace(args[0]) != "" {
		return s.Resolver.ResolveWithArgs(ctx, "site", "Site", args)
	}
	if sid := strings.TrimSpace(s.Scope.SiteID); sid != "" {
		name := s.Scope.SiteName
		if name == "" {
			name = s.Resolver.ResolveID("site", sid)
		}
		return &NamedItem{ID: sid, Name: name}, nil
	}
	return s.Resolver.Resolve(ctx, "site", "Site")
}

func splitCommaList(s string) []string {
	s = strings.TrimSpace(s)
	if s == "" {
		return nil
	}
	parts := strings.Split(s, ",")
	var out []string
	for _, p := range parts {
		p = strings.TrimSpace(p)
		if p != "" {
			out = append(out, p)
		}
	}
	return out
}

func cmdTenantIdentityConfigGet(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "get", site.ID)
	body, _, err := s.Client.Do("GET", tenantIdentitySitePath(s, "tenant-identity/config"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("getting tenant identity config: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTenantIdentityConfigUpdate(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	defaultAudience, err := PromptText("defaultAudience — JWT aud claim (required)", true)
	if err != nil {
		return err
	}
	issuer, err := PromptText("issuer — JWT iss / OIDC issuer (https://, http://, or spiffe://) (required)", true)
	if err != nil {
		return err
	}
	allowedStr, err := PromptText("allowedAudiences — comma-separated allowlist. Empty = only defaultAudience allowed; when non-empty, must include defaultAudience", false)
	if err != nil {
		return err
	}
	ttlStr, err := PromptText("tokenTtlSeconds — issued-token TTL seconds, must be >= 1; Core enforces a per-site window (required)", true)
	if err != nil {
		return err
	}
	subjectPrefix, err := PromptText("subjectPrefix — SPIFFE ID prefix, e.g. spiffe://issuer-trust-domain; must share trust domain with issuer (optional)", false)
	if err != nil {
		return err
	}
	enabledChoice, err := PromptChoice("enabled — issue JWT-SVIDs for this org", []string{"true", "false"}, "true")
	if err != nil {
		return err
	}
	rotate, err := PromptConfirm("rotateKey — regenerate the per-org signing keypair? (default no)")
	if err != nil {
		return err
	}

	overlapStr := ""
	if rotate {
		overlapStr, err = PromptText("signingKeyOverlapSeconds — seconds previous key stays in JWKS (required; must be >= tokenTtlSeconds)", true)
		if err != nil {
			return err
		}
	}

	body := map[string]interface{}{
		"enabled":         enabledChoice == "true",
		"issuer":          strings.TrimSpace(issuer),
		"defaultAudience": defaultAudience,
	}
	if auds := splitCommaList(allowedStr); len(auds) > 0 {
		body["allowedAudiences"] = auds
	}
	ttlU64, perr := strconv.ParseUint(strings.TrimSpace(ttlStr), 10, 32)
	if perr != nil {
		return fmt.Errorf("invalid tokenTtlSeconds: %w", perr)
	}
	body["tokenTtlSeconds"] = uint32(ttlU64)
	if strings.TrimSpace(subjectPrefix) != "" {
		body["subjectPrefix"] = strings.TrimSpace(subjectPrefix)
	}
	if rotate {
		body["rotateKey"] = true
		ovU64, perr := strconv.ParseUint(strings.TrimSpace(overlapStr), 10, 32)
		if perr != nil {
			return fmt.Errorf("invalid signingKeyOverlapSeconds: %w", perr)
		}
		body["signingKeyOverlapSeconds"] = uint32(ovU64)
	}

	payload, err := json.Marshal(body)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "update", site.ID)
	bodyOut, _, err := s.Client.Do("PUT", tenantIdentitySitePath(s, "tenant-identity/config"), map[string]string{"siteID": site.ID}, nil, payload)
	if err != nil {
		return fmt.Errorf("updating tenant identity config: %w", err)
	}
	fmt.Printf("%s Tenant identity configuration saved for site %s\n", Green("OK"), site.Name)
	return printDetailJSON(os.Stdout, bodyOut)
}

func cmdTenantIdentityConfigDelete(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete tenant identity configuration for site %s (%s)?", site.Name, site.ID))
	if err != nil {
		return err
	}
	if !ok {
		fmt.Println(Dim("Cancelled."))
		return nil
	}
	LogCmd(s, "tenant-identity", "delete", site.ID)
	_, _, err = s.Client.Do("DELETE", tenantIdentitySitePath(s, "tenant-identity/config"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting tenant identity config: %w", err)
	}
	fmt.Printf("%s Machine identity configuration deleted for site %s\n", Green("OK"), site.Name)
	return nil
}

func cmdTenantIdentityTokenDelegationGet(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "token-delegation", "get", site.ID)
	body, _, err := s.Client.Do("GET", tenantIdentitySitePath(s, "tenant-identity/token-delegation"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("getting token delegation: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTenantIdentityTokenDelegationUpdate(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	tokenEndpoint, err := PromptText("tokenEndpoint — RFC 8693 token exchange URL (https:// or http:// per site allowlist) (required)", true)
	if err != nil {
		return err
	}
	subjectAud, err := PromptText("subjectTokenAudience — aud on intermediate JWT-SVID (required)", true)
	if err != nil {
		return err
	}
	useSecret, err := PromptConfirm("clientSecretBasic — attach OAuth2 client_secret_basic credentials? (default no)")
	if err != nil {
		return err
	}
	body := map[string]interface{}{
		"tokenEndpoint":        strings.TrimSpace(tokenEndpoint),
		"subjectTokenAudience": strings.TrimSpace(subjectAud),
	}
	if useSecret {
		cid, err := PromptText("clientSecretBasic.clientId (required)", true)
		if err != nil {
			return err
		}
		sec, err := PromptSecret("clientSecretBasic.clientSecret (required, write-only)", true)
		if err != nil {
			return err
		}
		body["clientSecretBasic"] = map[string]string{
			"clientId":     strings.TrimSpace(cid),
			"clientSecret": sec,
		}
	}

	payload, err := json.Marshal(body)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "token-delegation", "update", site.ID)
	bodyOut, _, err := s.Client.Do("PUT", tenantIdentitySitePath(s, "tenant-identity/token-delegation"), map[string]string{"siteID": site.ID}, nil, payload)
	if err != nil {
		return fmt.Errorf("updating token delegation: %w", err)
	}
	fmt.Printf("%s Token delegation saved for site %s\n", Green("OK"), site.Name)
	return printDetailJSON(os.Stdout, bodyOut)
}

func cmdTenantIdentityTokenDelegationDelete(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	ok, err := PromptConfirm(fmt.Sprintf("Delete token delegation for site %s (%s)?", site.Name, site.ID))
	if err != nil {
		return err
	}
	if !ok {
		fmt.Println(Dim("Cancelled."))
		return nil
	}
	LogCmd(s, "tenant-identity", "token-delegation", "delete", site.ID)
	_, _, err = s.Client.Do("DELETE", tenantIdentitySitePath(s, "tenant-identity/token-delegation"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("deleting token delegation: %w", err)
	}
	fmt.Printf("%s Token delegation deleted for site %s\n", Green("OK"), site.Name)
	return nil
}

func cmdTenantIdentityWellKnownJWKS(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "jwks", "get", site.ID)
	body, _, err := s.Client.Do("GET", tenantIdentitySitePath(s, ".well-known/jwks.json"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("fetching JWKS: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTenantIdentityWellKnownSpiffeJWKS(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "spiffe-jwks", "get", site.ID)
	body, _, err := s.Client.Do("GET", tenantIdentitySitePath(s, ".well-known/spiffe/jwks.json"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("fetching SPIFFE JWKS: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTenantIdentityWellKnownOpenID(s *Session, args []string) error {
	ctx := context.Background()
	site, err := resolveTenantIdentitySite(s, ctx, args)
	if err != nil {
		return err
	}
	LogCmd(s, "tenant-identity", "openid-configuration", "get", site.ID)
	body, _, err := s.Client.Do("GET", tenantIdentitySitePath(s, ".well-known/openid-configuration"), map[string]string{"siteID": site.ID}, nil, nil)
	if err != nil {
		return fmt.Errorf("fetching OpenID configuration: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

// -- Singleton / info commands --

func cmdMetadataGet(s *Session, _ []string) error {
	LogCmd(s, "metadata", "get")
	body, _, err := s.Client.Do("GET", apiPath(s, "metadata"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting metadata: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdUserCurrent(s *Session, _ []string) error {
	LogCmd(s, "user", "current")
	body, _, err := s.Client.Do("GET", apiPath(s, "user/current"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting current user: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTenantCurrent(s *Session, _ []string) error {
	LogCmd(s, "tenant", "current")
	body, _, err := s.Client.Do("GET", apiPath(s, "tenant/current"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting current tenant: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdTenantStats(s *Session, _ []string) error {
	LogCmd(s, "tenant", "stats")
	body, _, err := s.Client.Do("GET", apiPath(s, "tenant/current/stats"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting tenant stats: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdInfraProviderCurrent(s *Session, _ []string) error {
	LogCmd(s, "infrastructure-provider", "current")
	body, _, err := s.Client.Do("GET", apiPath(s, "infrastructure-provider/current"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting infrastructure provider: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdInfraProviderStats(s *Session, _ []string) error {
	LogCmd(s, "infrastructure-provider", "stats")
	body, _, err := s.Client.Do("GET", apiPath(s, "infrastructure-provider/current/stats"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting infrastructure provider stats: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdServiceAccountCurrent(s *Session, _ []string) error {
	LogCmd(s, "service-account", "current")
	body, _, err := s.Client.Do("GET", apiPath(s, "service-account/current"), nil, nil, nil)
	if err != nil {
		return fmt.Errorf("getting service account: %w", err)
	}
	return printDetailJSON(os.Stdout, body)
}

func cmdLogin(s *Session, _ []string) error {
	if s.LoginFn == nil {
		return fmt.Errorf("login not available (no auth method configured)")
	}
	fmt.Println("Logging in...")
	token, err := s.LoginFn()
	if err != nil {
		return fmt.Errorf("login failed: %w", err)
	}
	s.RefreshClient(token)
	fmt.Printf("%s Logged in successfully.\n", Green("OK"))
	return nil
}

// cmdEnv prints every NICO_* environment variable currently set in the
// process, the config field (or flag) it influences, and its value. Pass
// "--mask" to redact sensitive values (tokens, secrets, passwords) for
// safer display in shared screens.
func cmdEnv(_ *Session, args []string) error {
	mask := false
	for _, a := range args {
		if a == "--mask" || a == "-m" {
			mask = true
		}
	}
	overrides := cli.EnvOverridesFromEnvironment()
	if len(overrides) == 0 {
		fmt.Println(Dim("No NICO_* environment variables set."))
		fmt.Println(Dim("Run 'help' for the full list of NICO_* variables nicocli understands."))
		return nil
	}
	fmt.Printf("%s %d NICO_* variable(s) in use:\n", Bold("env:"), len(overrides))
	fmt.Print(cli.FormatEnvOverrides(overrides, mask))
	if !mask {
		fmt.Printf("%s sensitive values shown in full; pass %s to redact.\n",
			Dim("Tip:"), Bold("env --mask"))
	}
	return nil
}

func cmdHelp(_ *Session, _ []string) error {
	tw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintln(tw, "\nCOMMAND\tDESCRIPTION")
	fmt.Fprintln(tw, "-------\t-----------")
	for _, cmd := range AllCommands() {
		fmt.Fprintf(tw, "%s\t%s\n", cmd.Name, cmd.Description)
	}
	fmt.Fprintln(tw, "org\tShow current org")
	fmt.Fprintln(tw, "org list\tList available orgs (from JWT claims)")
	fmt.Fprintln(tw, "org set <name>\tSwitch to a different org")
	fmt.Fprintln(tw, "scope\tShow current scope filters")
	fmt.Fprintln(tw, "scope site [name]\tSet site scope (filters lists)")
	fmt.Fprintln(tw, "scope vpc [name]\tSet VPC scope (filters lists)")
	fmt.Fprintln(tw, "scope label key=value\tAdd a label filter (persists across commands)")
	fmt.Fprintln(tw, "scope label clear\tClear all label filters")
	fmt.Fprintln(tw, "scope clear\tClear all scope filters")
	fmt.Fprintln(tw, "<list> --label key=value\tFilter a single label-capable list by a label (no persist)")
	fmt.Fprintln(tw, "<list> --sort-label key\tSort a single label-capable list by a label key")
	fmt.Fprintln(tw, "\t  label-capable lists: "+strings.Join(labelCapableListCommands, ", "))
	fmt.Fprintln(tw, "exit\tExit interactive mode")
	tw.Flush()

	envTw := tabwriter.NewWriter(os.Stdout, 0, 0, 3, ' ', 0)
	fmt.Fprintf(envTw, "\n%s\tCONFIG FIELD / NOTES\n", Bold("ENV VAR"))
	fmt.Fprintln(envTw, "-------\t--------------------")
	for _, d := range cli.KnownEnvVarDescriptors() {
		notes := d.ConfigPath
		if d.Sensitive {
			notes += " (sensitive)"
		}
		fmt.Fprintf(envTw, "%s\t%s\n", d.Name, notes)
	}
	fmt.Fprintln(envTw, "\t  Env vars override config file values; explicit flags still win.")
	fmt.Fprintln(envTw, "\t  Run 'env' to see currently set values, 'env --mask' to redact sensitive ones.")
	envTw.Flush()

	fmt.Printf("\n%s\n", Bold("KEYBINDINGS"))
	fmt.Println("  Ctrl+C    Clear current line")
	fmt.Println("  Ctrl+D    Quit interactive mode")
	fmt.Println("  Esc       Cancel current selection")
	fmt.Println("  Tab       Accept suggestion")
	fmt.Println("  Up/Down   Navigate suggestions or history")
	fmt.Println()
	return nil
}

// -- Helpers --

func splitCommaSeparated(input string) []string {
	trimmed := strings.TrimSpace(input)
	if trimmed == "" {
		return nil
	}
	parts := strings.Split(trimmed, ",")
	out := make([]string, 0, len(parts))
	for _, p := range parts {
		v := strings.TrimSpace(p)
		if v != "" {
			out = append(out, v)
		}
	}
	return out
}

func parseOptionalBool(input string) (bool, bool) {
	switch strings.ToLower(strings.TrimSpace(input)) {
	case "":
		return false, false
	case "true", "t", "yes", "y", "1":
		return true, true
	case "false", "f", "no", "n", "0":
		return false, true
	default:
		return false, false
	}
}

func parseOptionalInt(input string) (int, bool) {
	trimmed := strings.TrimSpace(input)
	if trimmed == "" {
		return 0, false
	}
	var out int
	if _, err := fmt.Sscanf(trimmed, "%d", &out); err != nil {
		return 0, false
	}
	return out, true
}

func rawFieldString(raw interface{}, key string) string {
	m, ok := raw.(map[string]interface{})
	if !ok {
		return ""
	}
	v, ok := m[key]
	if !ok {
		return ""
	}
	s, ok := v.(string)
	if !ok {
		return ""
	}
	return strings.TrimSpace(s)
}

func getAndPrint(s *Session, path, id string) error {
	body, _, err := s.Client.Do("GET", path, map[string]string{"id": id}, nil, nil)
	if err != nil {
		return err
	}
	return printDetailJSON(os.Stdout, body)
}

func printDetailJSON(w io.Writer, data []byte) error {
	var v interface{}
	if err := json.Unmarshal(data, &v); err != nil {
		fmt.Fprintln(w, string(data))
		return nil
	}
	pretty, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		fmt.Fprintln(w, string(data))
		return nil
	}
	fmt.Fprintln(w, string(pretty))
	return nil
}

func formatLabels(labels map[string]string, maxWidth int) string {
	if len(labels) == 0 {
		return ""
	}
	keys := make([]string, 0, len(labels))
	for k := range labels {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	parts := make([]string, 0, len(keys))
	for _, k := range keys {
		parts = append(parts, k+"="+labels[k])
	}
	s := strings.Join(parts, ", ")
	if maxWidth > 3 && len(s) > maxWidth {
		return s[:maxWidth-3] + "..."
	}
	return s
}

// labelCapableListCommands enumerates the list commands that render a LABELS
// column and accept --label / --sort-label flags. Kept here next to the hint
// helpers so adding a new label-capable list in one place keeps the hint
// behavior, the help text, and any future label-aware tooling in sync.
var labelCapableListCommands = []string{
	"vpc list",
	"instance-type list",
	"instance list",
	"machine list",
	"network-security-group list",
	"expected-machine list",
	"expected-rack list",
	"expected-switch list",
	"expected-power-shelf list",
	"infiniband-partition list",
}

// printLabelHint writes a one-line hint about how to filter, sort, and persist
// label filters. Uses generic <key>/<value> placeholders -- the hint is the
// same regardless of what is in the current result set, so no per-call sample
// or key listing is needed (and no max-keys cap to tune). Suppressed when a
// label filter is already active or when no items carry labels, so the hint
// only surfaces when it is actionable.
func printLabelHint(w io.Writer, items []NamedItem, activeFilters map[string]string) {
	if len(activeFilters) > 0 {
		return
	}
	if !anyItemHasLabels(items) {
		return
	}
	fmt.Fprintln(w, Dim(
		"Hint: --label <key>=<value> filter | --sort-label <key> sort | scope label <key>=<value> persist",
	))
}

// anyItemHasLabels reports whether at least one item carries a non-empty
// label key. Used to gate printLabelHint so the hint only appears on lists
// where labels are actually in play.
func anyItemHasLabels(items []NamedItem) bool {
	for _, item := range items {
		for k := range item.Labels {
			if strings.TrimSpace(k) != "" {
				return true
			}
		}
	}
	return false
}

func filterByLabels(items []NamedItem, filters map[string]string) []NamedItem {
	if len(filters) == 0 {
		return items
	}
	result := make([]NamedItem, 0, len(items))
	for _, item := range items {
		match := true
		for k, v := range filters {
			if item.Labels[k] != v {
				match = false
				break
			}
		}
		if match {
			result = append(result, item)
		}
	}
	return result
}

func sortByLabelKey(items []NamedItem, key string) []NamedItem {
	sorted := make([]NamedItem, len(items))
	copy(sorted, items)
	sort.SliceStable(sorted, func(i, j int) bool {
		vi, oki := sorted[i].Labels[key]
		vj, okj := sorted[j].Labels[key]
		if !oki && !okj {
			return false
		}
		if !oki {
			return false
		}
		if !okj {
			return true
		}
		return vi < vj
	})
	return sorted
}

// parseLabelArgs extracts --label key=value and --sort-label key from args.
// Returns the remaining args, label filters, sort-label key, and an error
// if a --label value is missing "=" or either flag is followed by an option.
func parseLabelArgs(args []string) (remaining []string, labels map[string]string, sortKey string, err error) {
	labels = map[string]string{}
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--label":
			if i+1 >= len(args) || strings.HasPrefix(args[i+1], "--") {
				return nil, nil, "", fmt.Errorf("--label requires a key=value argument")
			}
			i++
			if k, v, ok := strings.Cut(args[i], "="); ok {
				if prev, exists := labels[k]; exists && prev != v {
					return nil, nil, "", fmt.Errorf("conflicting --label filters for %q", k)
				}
				labels[k] = v
			} else {
				return nil, nil, "", fmt.Errorf("--label value %q must contain '='", args[i])
			}
		case "--sort-label":
			if i+1 >= len(args) || strings.HasPrefix(args[i+1], "--") {
				return nil, nil, "", fmt.Errorf("--sort-label requires a key argument")
			}
			i++
			sortKey = args[i]
		default:
			remaining = append(remaining, args[i])
		}
	}
	return remaining, labels, sortKey, nil
}

// mergeLabels combines scope label filters with per-command label filters.
// Returns an error if a command-level label conflicts with a scope label.
func mergeLabels(scope, cmd map[string]string) (map[string]string, error) {
	if len(scope) == 0 && len(cmd) == 0 {
		return nil, nil
	}
	merged := make(map[string]string, len(scope)+len(cmd))
	for k, v := range scope {
		merged[k] = v
	}
	for k, v := range cmd {
		if prev, exists := merged[k]; exists && prev != v {
			return nil, fmt.Errorf("--label %s=%s conflicts with scope label %s=%s", k, v, k, prev)
		}
		merged[k] = v
	}
	return merged, nil
}

func printResourceTable(w io.Writer, col1, col2, col3 string, items []NamedItem) error {
	fmt.Fprintf(os.Stderr, "%d items\n", len(items))
	tw := tabwriter.NewWriter(w, 0, 0, 3, ' ', 0)
	fmt.Fprintf(tw, "%s\t%s\t%s\n", col1, col2, col3)
	for _, item := range items {
		fmt.Fprintf(tw, "%s\t%s\t%s\n", item.Name, item.Status, item.ID)
	}
	return tw.Flush()
}
