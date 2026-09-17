// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"net/url"
	"strings"
)

// GeneratedResourceDescriptor describes how an OpenAPI parameter can be
// resolved to a selectable API resource. FreeFormReason is set only for a
// reviewed parameter that has no usable list surface.
type GeneratedResourceDescriptor struct {
	ResourceType    string
	ParentParameter string
	ScopeParameter  string
	FreeFormReason  string
}

// CanonicalGeneratedResourceType derives the resolver key for an OpenAPI
// parameter. It also normalizes acronym spellings and numbered VPC fields to
// the resource keys registered by Session.
func CanonicalGeneratedResourceType(commandName, parameter string) string {
	base := strings.TrimSpace(parameter)
	lower := strings.ToLower(base)
	switch {
	case strings.HasSuffix(lower, "ids"):
		base = base[:len(base)-3]
	case strings.HasSuffix(lower, "id"):
		base = base[:len(base)-2]
	}
	if strings.EqualFold(base, "") || strings.EqualFold(base, "id") {
		base, _, _ = strings.Cut(commandName, " ")
	}

	resourceType := camelToKebab(base)
	switch resourceType {
	case "account":
		return "tenant-account"
	case "audit-entry":
		return "audit"
	case "infini-band-partition":
		return "infiniband-partition"
	case "nv-link-logical-partition":
		return "nvlink-logical-partition"
	case "component":
		return "tray-component"
	case "run":
		return "task-run"
	case "vpc1", "vpc2", "secondary-vpc":
		return "vpc"
	default:
		return resourceType
	}
}

// GeneratedPathResourceDescriptor derives the resource selector and any
// parent/scope value that must be resolved before it can be listed.
func GeneratedPathResourceDescriptor(commandName, parameter string) GeneratedResourceDescriptor {
	descriptor := GeneratedResourceDescriptor{
		ResourceType: CanonicalGeneratedResourceType(commandName, parameter),
	}

	switch {
	case commandName == "allocation constraint update" && strings.EqualFold(parameter, "allocationConstraintId"):
		descriptor.ResourceType = "allocation-constraint"
		descriptor.ParentParameter = "allocationId"
	case strings.HasPrefix(commandName, "dpu-extension-service version ") && strings.EqualFold(parameter, "version"):
		descriptor.ResourceType = "dpu-extension-service-version"
		descriptor.ParentParameter = "dpuExtensionServiceId"
	case commandName == "machine health-report delete" && strings.EqualFold(parameter, "source"):
		descriptor.ResourceType = "health-report-source"
		descriptor.ParentParameter = "machineId"
	case commandName == "instance-type machine-association delete" && strings.EqualFold(parameter, "machineAssociationId"):
		descriptor.ResourceType = "instance-type-machine"
		descriptor.ParentParameter = "instanceTypeId"
	}

	if descriptor.ResourceType == "rule" || descriptor.ResourceType == "task-run" {
		descriptor.ScopeParameter = "siteId"
	}
	if descriptor.ResourceType == "task" {
		descriptor.FreeFormReason = "task IDs come from prior lifecycle actions; no site-wide task list API exists"
	}
	if descriptor.ResourceType == "nvlink-domain" {
		descriptor.FreeFormReason = "NVLink domain IDs come from prior lifecycle actions; no list API exists"
	}
	if strings.HasPrefix(commandName, "measured-boot") && strings.EqualFold(parameter, "id") {
		descriptor.FreeFormReason = "measured-boot approvals and their selected machine or profile IDs share this parameter; enter the ID that matches --selector"
	}
	return descriptor
}

// ResolveGeneratedResource resolves a supplied name/ID, or opens an
// interactive selector when value is empty. supported is false only when the
// parameter has no known list surface.
func (s *Session) ResolveGeneratedResource(
	ctx context.Context,
	descriptor GeneratedResourceDescriptor,
	resolvedValues map[string]string,
	label string,
	value string,
) (item *NamedItem, supported bool, err error) {
	if s == nil {
		return nil, false, fmt.Errorf("interactive session is required")
	}
	items, supported, err := s.GeneratedResourceItems(ctx, descriptor, resolvedValues)
	if err != nil || !supported {
		return nil, supported, err
	}

	value = strings.TrimSpace(value)
	if value == "" {
		if s.Resolver == nil {
			return nil, true, fmt.Errorf(
				"interactive resolver is required to select %s",
				descriptor.ResourceType,
			)
		}
		item, err = s.Resolver.SelectFromItems(label, items)
		return item, true, err
	}
	matches := matchingGeneratedResourceItems(items, value)
	if len(matches) > 1 {
		return nil, true, fmt.Errorf(
			"ambiguous %s %q matches %d resources; use an exact ID",
			descriptor.ResourceType,
			value,
			len(matches),
		)
	}
	if len(matches) == 1 {
		fmt.Printf("%s %s %s\n", Bold(label+":"), Green(matches[0].Name), Dim("(matched)"))
		return &matches[0], true, nil
	}
	return nil, true, fmt.Errorf("no %s matching %q found", descriptor.ResourceType, value)
}

func matchingGeneratedResourceItems(items []NamedItem, value string) []NamedItem {
	var idMatches []NamedItem
	var nameMatches []NamedItem
	for i := range items {
		if strings.EqualFold(items[i].ID, value) {
			idMatches = append(idMatches, items[i])
		} else if strings.EqualFold(items[i].Name, value) {
			nameMatches = append(nameMatches, items[i])
		}
	}
	if len(idMatches) > 0 {
		return idMatches
	}
	return nameMatches
}

// GeneratedResourceItems returns selectable items for a generated parameter.
// resolvedValues is keyed by the original OpenAPI parameter names.
func (s *Session) GeneratedResourceItems(
	ctx context.Context,
	descriptor GeneratedResourceDescriptor,
	resolvedValues map[string]string,
) ([]NamedItem, bool, error) {
	if descriptor.FreeFormReason != "" {
		return nil, false, nil
	}
	switch descriptor.ResourceType {
	case "allocation-constraint":
		parent, err := requiredResolvedValue(descriptor, resolvedValues)
		if err != nil {
			return nil, true, err
		}
		items, err := s.fetchAllocationConstraints(parent)
		return items, true, err
	case "dpu-extension-service-version":
		parent, err := requiredResolvedValue(descriptor, resolvedValues)
		if err != nil {
			return nil, true, err
		}
		items, err := s.fetchDPUExtensionServiceVersions(parent)
		return items, true, err
	case "health-report-source":
		parent, err := requiredResolvedValue(descriptor, resolvedValues)
		if err != nil {
			return nil, true, err
		}
		items, err := s.fetchMachineHealthReportSources(parent)
		return items, true, err
	case "instance-type-machine":
		parent, err := requiredResolvedValue(descriptor, resolvedValues)
		if err != nil {
			return nil, true, err
		}
		items, err := s.fetchInstanceTypeMachines(ctx, parent)
		return items, true, err
	case "rule":
		siteID := strings.TrimSpace(resolvedValues[descriptor.ScopeParameter])
		if siteID == "" {
			siteID = strings.TrimSpace(s.Scope.SiteID)
		}
		if siteID == "" {
			return nil, true, fmt.Errorf("siteId must be resolved before operation rules")
		}
		items, err := s.fetchRulesForSite(ctx, siteID)
		return items, true, err
	case "task-run":
		siteID := strings.TrimSpace(resolvedValues[descriptor.ScopeParameter])
		if siteID == "" {
			siteID = strings.TrimSpace(s.Scope.SiteID)
		}
		if siteID == "" {
			return nil, true, fmt.Errorf("siteId must be resolved before task runs")
		}
		items, err := s.fetchRunsForSite(ctx, siteID)
		return items, true, err
	default:
		if s.Resolver == nil || !s.Resolver.HasFetcher(descriptor.ResourceType) {
			return nil, false, nil
		}
		items, err := s.Resolver.Fetch(ctx, descriptor.ResourceType)
		return items, true, err
	}
}

func requiredResolvedValue(descriptor GeneratedResourceDescriptor, resolvedValues map[string]string) (string, error) {
	value := strings.TrimSpace(resolvedValues[descriptor.ParentParameter])
	if value == "" {
		return "", fmt.Errorf(
			"%s must be resolved before %s",
			descriptor.ParentParameter,
			descriptor.ResourceType,
		)
	}
	return value, nil
}

func (s *Session) fetchAllocationConstraints(allocationID string) ([]NamedItem, error) {
	resource, err := s.fetchResourceObject("allocation/" + url.PathEscape(allocationID))
	if err != nil {
		return nil, err
	}
	rawConstraints, _ := resource["allocationConstraints"].([]interface{})
	items := make([]NamedItem, 0, len(rawConstraints))
	for _, raw := range rawConstraints {
		constraint, ok := raw.(map[string]interface{})
		if !ok {
			continue
		}
		id := str(constraint, "id")
		if id == "" {
			continue
		}
		resourceType := str(constraint, "resourceType")
		resourceName := ""
		switch resourceType {
		case "InstanceType":
			resourceName = nestedString(constraint, "instanceType", "name")
		case "IPBlock":
			resourceName = nestedString(constraint, "ipBlock", "name")
		}
		if resourceName == "" {
			resourceName = str(constraint, "resourceTypeId")
		}
		name := strings.Trim(strings.Join([]string{resourceType, resourceName}, " / "), " /")
		if name == "" {
			name = id
		}
		items = append(items, NamedItem{
			Name: name, ID: id, Status: str(constraint, "constraintType"),
			Extra: map[string]string{
				"allocationId":   allocationID,
				"resourceType":   resourceType,
				"resourceTypeId": str(constraint, "resourceTypeId"),
			},
			Raw: constraint,
		})
	}
	return items, nil
}

func (s *Session) fetchDPUExtensionServiceVersions(serviceID string) ([]NamedItem, error) {
	resource, err := s.fetchResourceObject("dpu-extension-service/" + url.PathEscape(serviceID))
	if err != nil {
		return nil, err
	}
	versions := stringSlice(resource["activeVersions"])
	if latest := strings.TrimSpace(str(resource, "version")); latest != "" {
		versions = append([]string{latest}, versions...)
	}
	seen := make(map[string]struct{}, len(versions))
	items := make([]NamedItem, 0, len(versions))
	for _, version := range versions {
		version = strings.TrimSpace(version)
		if version == "" {
			continue
		}
		if _, ok := seen[version]; ok {
			continue
		}
		seen[version] = struct{}{}
		items = append(items, NamedItem{
			Name: version,
			ID:   version,
			Extra: map[string]string{
				"dpuExtensionServiceId": serviceID,
			},
		})
	}
	return items, nil
}

func (s *Session) fetchMachineHealthReportSources(machineID string) ([]NamedItem, error) {
	raw, _, err := s.Client.Do(
		"GET",
		apiPath(s, "machine/"+url.PathEscape(machineID)+"/health-report"),
		nil,
		nil,
		nil,
	)
	if err != nil {
		return nil, err
	}
	var reports []map[string]interface{}
	if err := json.Unmarshal(raw, &reports); err != nil {
		return nil, fmt.Errorf("parsing machine health reports: %w", err)
	}
	items := make([]NamedItem, 0, len(reports))
	seen := map[string]struct{}{}
	for _, report := range reports {
		source := strings.TrimSpace(str(report, "source"))
		if source == "" {
			continue
		}
		if _, ok := seen[source]; ok {
			continue
		}
		seen[source] = struct{}{}
		items = append(items, NamedItem{
			Name: source, ID: source, Status: str(report, "mode"),
			Extra: map[string]string{"machineId": machineID},
			Raw:   report,
		})
	}
	return items, nil
}

func (s *Session) fetchInstanceTypeMachines(ctx context.Context, instanceTypeID string) ([]NamedItem, error) {
	associations, err := s.fetchAll(
		apiPath(s, "instance/type/"+url.PathEscape(instanceTypeID)+"/machine"),
		nil,
	)
	if err != nil {
		return nil, err
	}
	machineNames := map[string]string{}
	if s.Resolver != nil {
		if machines, fetchErr := s.Resolver.Fetch(ctx, "machine"); fetchErr == nil {
			for _, machine := range machines {
				machineNames[machine.ID] = machine.Name
			}
		}
	}
	items := make([]NamedItem, 0, len(associations))
	for _, association := range associations {
		machineID := strings.TrimSpace(str(association, "machineId"))
		if machineID == "" {
			continue
		}
		name := strings.TrimSpace(machineNames[machineID])
		if name == "" {
			name = machineID
		}
		items = append(items, NamedItem{
			Name: name,
			ID:   machineID,
			Extra: map[string]string{
				"instanceTypeId": instanceTypeID,
				"associationId":  str(association, "id"),
			},
			Raw: association,
		})
	}
	return items, nil
}

func (s *Session) fetchResourceObject(resourcePath string) (map[string]interface{}, error) {
	raw, _, err := s.Client.Do("GET", apiPath(s, resourcePath), nil, nil, nil)
	if err != nil {
		return nil, err
	}
	var resource map[string]interface{}
	if err := json.Unmarshal(raw, &resource); err != nil {
		return nil, fmt.Errorf("parsing resource response: %w", err)
	}
	return resource, nil
}

func nestedString(resource map[string]interface{}, objectKey, valueKey string) string {
	nested, ok := resource[objectKey].(map[string]interface{})
	if !ok {
		return ""
	}
	return str(nested, valueKey)
}

func stringSlice(value interface{}) []string {
	raw, ok := value.([]interface{})
	if !ok {
		return nil
	}
	result := make([]string, 0, len(raw))
	for _, item := range raw {
		if text, ok := item.(string); ok {
			result = append(result, text)
		}
	}
	return result
}
