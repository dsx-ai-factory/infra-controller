// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

// registeredSiteItems leaves the shared site cache intact: inspection and
// registration workflows must still be able to resolve unregistered sites.
func registeredSiteItems(s *Session, ctx context.Context) ([]NamedItem, error) {
	sites, err := s.Resolver.Fetch(ctx, "site")
	if err != nil {
		return nil, fmt.Errorf("fetching site: %w", err)
	}
	registered := make([]NamedItem, 0, len(sites))
	for _, site := range sites {
		if site.Status == "Registered" {
			registered = append(registered, site)
		}
	}
	return registered, nil
}

func promptRegisteredSite(s *Session, ctx context.Context) (*NamedItem, error) {
	sites, err := registeredSiteItems(s, ctx)
	if err != nil {
		return nil, err
	}
	if len(sites) == 0 {
		return nil, fmt.Errorf("no Registered site available")
	}
	return s.Resolver.SelectFromItems("Site", sites)
}

func validateRegisteredSiteIDs(s *Session, ctx context.Context, ids []string) error {
	if len(ids) == 0 {
		return nil
	}
	sites, err := registeredSiteItems(s, ctx)
	if err != nil {
		return err
	}
	registered := make(map[string]bool, len(sites))
	for _, site := range sites {
		registered[strings.ToLower(site.ID)] = true
	}
	for _, id := range ids {
		if !registered[strings.ToLower(id)] {
			return fmt.Errorf("site %q is unavailable or not in Registered state", id)
		}
	}
	return nil
}

func requireRegisteredSiteScope(s *Session, missingSitePrompt string) (string, error) {
	ctx := context.Background()
	id := strings.TrimSpace(s.Scope.SiteID)
	if id != "" {
		return id, validateRegisteredSiteIDs(s, ctx, []string{id})
	}
	fmt.Printf("%s %s\n", Dim("Note:"), missingSitePrompt)
	site, err := promptRegisteredSite(s, ctx)
	if err != nil {
		return "", err
	}
	setSiteScopeFromID(s, site.ID)
	return site.ID, nil
}

func promptOptionalRegisteredSiteIDs(s *Session, ctx context.Context) ([]string, error) {
	sites, err := registeredSiteItems(s, ctx)
	if err != nil {
		return nil, err
	}
	return promptOptionalItemIDs(s, sites, "site", "site")
}

func sshKeyGroupSiteItems(s *Session, ctx context.Context, group *NamedItem) ([]NamedItem, error) {
	// REST checks registration only for new associations. Retaining an existing
	// association must remain possible when its site is no longer Registered.
	sites, err := registeredSiteItems(s, ctx)
	if err != nil {
		return nil, err
	}
	seen := make(map[string]bool, len(sites))
	for _, site := range sites {
		seen[site.ID] = true
	}
	raw, _ := group.Raw.(map[string]interface{})
	associations, _ := raw["siteAssociations"].([]interface{})
	for _, value := range associations {
		association, _ := value.(map[string]interface{})
		site, _ := association["site"].(map[string]interface{})
		id := str(site, "id")
		if id == "" || seen[id] {
			continue
		}
		name := str(site, "name")
		if name == "" {
			name = id
		}
		sites = append(sites, NamedItem{
			ID:     id,
			Name:   name,
			Status: str(site, "status"),
		})
		seen[id] = true
	}
	return sites, nil
}

// These operations explicitly require Registered sites in their REST handlers.
// Keep this opt-in: site administration, IP blocks, allocations and instance
// types have different registration requirements. Expected-machine batch-create
// also lacks the single-create REST gate. Operation IDs cover
// generated aliases without depending on their displayed command names.
func generatedCommandRequiresRegisteredSite(info appcli.GeneratedCommandInfo) bool {
	switch info.OperationID {
	case "create-operating-system", "create-vpc", "create-vpc-peering",
		"create-network-security-group", "create-ssh-key-group",
		"create-infiniband-partition", "create-nvlink-logical-partition",
		"create-spectrumx-partition", "create-dpu-extension-service",
		"create-expected-machine",
		"create-expected-switch", "create-expected-power-shelf",
		"create-expected-rack", "create-expected-rack-group",
		"replace-all-expected-machine", "replace-all-expected-switch",
		"replace-all-expected-power-shelf", "replace-all-expected-rack",
		"replace-all-expected-rack-group":
		return true
	default:
		return false
	}
}

// Validate the effective request after scope injection as well as filtering
// pickers. Explicit flags and JSON must not bypass the operation's site policy.
func validateGeneratedSiteSelections(s *Session, info appcli.GeneratedCommandInfo, args []string) ([]string, error) {
	if !generatedCommandRequiresRegisteredSite(info) {
		return args, nil
	}
	out := append([]string(nil), args...)
	var ids []string
	for _, option := range generatedArgumentOptions(info, args) {
		switch option.name {
		case "site-id", "site-ids":
			ids = append(ids, splitCommaSeparated(option.value)...)
		case "data", "data-file":
			body := []byte(option.value)
			if option.name == "data-file" {
				var err error
				body, err = appcli.ReadBodyInput("", option.value)
				if err != nil {
					return nil, err
				}
				// Execute exactly the bytes validated here, including stdin input.
				if option.inline {
					out[option.index] = "--data=" + string(body)
				} else {
					out[option.index] = "--data"
					out[option.valueIndex] = string(body)
				}
			}
			bodyIDs, err := generatedBodySiteIDs(body, info.BodyFormFields)
			if err != nil {
				return nil, fmt.Errorf("reading request sites: %w", err)
			}
			ids = append(ids, bodyIDs...)
		}
	}
	err := validateRegisteredSiteIDs(s, context.Background(), ids)
	return out, err
}

// Walk schema-declared fields so opaque labels or payloads named "siteId"
// are not mistaken for resource references. This also covers bulk requests.
func generatedBodySiteIDs(body json.RawMessage, fields []appcli.GeneratedCommandBodyFormField) ([]string, error) {
	body = json.RawMessage(strings.TrimSpace(string(body)))
	if len(body) > 0 && body[0] == '[' {
		var items []json.RawMessage
		err := json.Unmarshal(body, &items)
		if err != nil {
			return nil, err
		}
		var ids []string
		for _, item := range items {
			itemIDs, err := generatedBodySiteIDs(item, fields)
			if err != nil {
				return nil, err
			}
			ids = append(ids, itemIDs...)
		}
		return ids, nil
	}
	var object map[string]json.RawMessage
	err := json.Unmarshal(body, &object)
	if err != nil {
		return nil, err
	}
	var ids []string
	for _, field := range fields {
		value, present := object[field.JSONName]
		if !present || string(value) == "null" {
			continue
		}
		switch field.JSONName {
		case "siteId":
			var id string
			err = json.Unmarshal(value, &id)
			ids = append(ids, id)
		case "siteIds":
			var values []string
			err = json.Unmarshal(value, &values)
			ids = append(ids, values...)
		default:
			if len(field.Properties) > 0 {
				var nested []string
				nested, err = generatedBodySiteIDs(value, field.Properties)
				ids = append(ids, nested...)
			}
		}
		if err != nil {
			return nil, err
		}
	}
	return ids, nil
}
