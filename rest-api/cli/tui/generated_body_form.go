// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

type generatedBodyFormPrompter interface {
	Text(label string, required bool) (string, error)
	Secret(label string, required bool) (string, error)
	Confirm(label string) (bool, error)
	Choice(label string, options []string, defaultValue string) (string, error)
}

type terminalGeneratedBodyFormPrompter struct{}

var generatedBodySessionResourceTypes = map[string]struct{}{
	"allocation-constraint":         {},
	"dpu-extension-service-version": {},
	"health-report-source":          {},
	"instance-type-machine":         {},
	"rule":                          {},
}

func (terminalGeneratedBodyFormPrompter) Text(label string, required bool) (string, error) {
	return PromptText(label, required)
}

func (terminalGeneratedBodyFormPrompter) Secret(label string, required bool) (string, error) {
	return PromptSecret(label, required)
}

func (terminalGeneratedBodyFormPrompter) Confirm(label string) (bool, error) {
	return PromptConfirm(label)
}

func (terminalGeneratedBodyFormPrompter) Choice(
	label string,
	options []string,
	defaultValue string,
) (string, error) {
	return PromptChoice(label, options, defaultValue)
}

// addGeneratedBodyForm prompts for a generated command's request body unless
// the caller already supplied --data, --data-file, or scalar body flags. The
// guided mode walks object and array schemas, resolving resource IDs to names
// when a compatible list fetcher exists. JSON mode remains the escape hatch for
// arbitrary or schemaless payloads.
func addGeneratedBodyForm(
	s *Session,
	info appcli.GeneratedCommandInfo,
	args []string,
) ([]string, error) {
	return addGeneratedBodyFormWithPrompter(s, info, args, terminalGeneratedBodyFormPrompter{})
}

func addGeneratedBodyFormWithPrompter(
	s *Session,
	info appcli.GeneratedCommandInfo,
	args []string,
	prompter generatedBodyFormPrompter,
) ([]string, error) {
	out := append([]string(nil), args...)
	if !info.HasRequestBody || hasGeneratedBodyInput(info, out) {
		return out, nil
	}

	canGuide := (info.BodyRootType == "" || info.BodyRootType == "object" || info.BodyRootType == "array") &&
		len(info.BodyFormFields) > 0
	defaultMode := "json"
	if canGuide {
		defaultMode = "guided"
	}
	modes := []string{"json"}
	if canGuide {
		modes = []string{"guided", "json"}
	}
	if !info.RequestBodyRequired {
		modes = append(modes, "none")
	}
	mode, err := prompter.Choice(
		"Request body input",
		modes,
		defaultMode,
	)
	if err != nil {
		return nil, err
	}

	switch mode {
	case "none":
		return out, nil
	case "json":
		return addGeneratedJSONBody(info, out, prompter)
	case "guided":
		if !canGuide {
			return nil, fmt.Errorf("guided request-body input is unavailable for this schema; use JSON input")
		}
	default:
		return nil, fmt.Errorf("unsupported request-body input mode %q", mode)
	}

	body, set, err := buildGeneratedBodyForm(s, info, prompter)
	if err != nil {
		return nil, err
	}
	if !set {
		return out, nil
	}
	encoded, err := json.Marshal(body)
	if err != nil {
		return nil, fmt.Errorf("encoding request body: %w", err)
	}
	return append(out, "--data", string(encoded)), nil
}

func addGeneratedJSONBody(
	info appcli.GeneratedCommandInfo,
	args []string,
	prompter generatedBodyFormPrompter,
) ([]string, error) {
	label := "Request body as JSON"
	if !info.RequestBodyRequired {
		label += " (optional)"
	}
	var (
		body string
		err  error
	)
	if generatedCommandHasSensitiveBody(info) {
		body, err = prompter.Secret(label, info.RequestBodyRequired)
	} else {
		body, err = prompter.Text(label, info.RequestBodyRequired)
	}
	if err != nil {
		return nil, err
	}
	body = strings.TrimSpace(body)
	if body == "" {
		if info.RequestBodyRequired {
			return nil, fmt.Errorf("request body is required")
		}
		return args, nil
	}
	if !json.Valid([]byte(body)) {
		return nil, fmt.Errorf("request body is not valid JSON")
	}
	return append(args, "--data", body), nil
}

func buildGeneratedBodyForm(
	s *Session,
	info appcli.GeneratedCommandInfo,
	prompter generatedBodyFormPrompter,
) (interface{}, bool, error) {
	if info.BodyRootType == "array" {
		var items []map[string]interface{}
		for {
			item, err := buildGeneratedBodyFormObject(s, info, prompter)
			if err != nil {
				return nil, false, err
			}
			items = append(items, item)
			more, err := prompter.Confirm("Add another request item?")
			if err != nil {
				return nil, false, err
			}
			if !more {
				break
			}
		}
		return items, true, nil
	}

	body, err := buildGeneratedBodyFormObject(s, info, prompter)
	if err != nil {
		return nil, false, err
	}
	return body, len(body) > 0 || info.RequestBodyRequired, nil
}

func buildGeneratedBodyFormObject(
	s *Session,
	info appcli.GeneratedCommandInfo,
	prompter generatedBodyFormPrompter,
) (map[string]interface{}, error) {
	resolvedValues := make(map[string]string)
	return buildGeneratedBodyFormProperties(s, info, info.BodyFormFields, resolvedValues, prompter)
}

func buildGeneratedBodyFormProperties(
	s *Session,
	info appcli.GeneratedCommandInfo,
	fields []appcli.GeneratedCommandBodyFormField,
	resolvedValues map[string]string,
	prompter generatedBodyFormPrompter,
) (map[string]interface{}, error) {
	body := make(map[string]interface{})
	for _, field := range orderedGeneratedBodyFormFields(fields) {
		if scoped, ok := generatedBodyScopeValue(s, field.JSONName); ok {
			body[field.JSONName] = scoped
			resolvedValues[field.JSONName] = scoped
			continue
		}

		value, set, err := promptGeneratedBodyField(s, info, field, resolvedValues, prompter)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", field.JSONName, err)
		}
		if set {
			body[field.JSONName] = value
			if stringValue, ok := value.(string); ok {
				resolvedValues[field.JSONName] = stringValue
			}
		}
	}
	return body, nil
}

func orderedGeneratedBodyFormFields(
	fields []appcli.GeneratedCommandBodyFormField,
) []appcli.GeneratedCommandBodyFormField {
	ordered := make([]appcli.GeneratedCommandBodyFormField, 0, len(fields))
	for _, scopeName := range []string{"siteId", "vpcId"} {
		for _, field := range fields {
			if field.JSONName == scopeName {
				ordered = append(ordered, field)
			}
		}
	}
	for _, field := range fields {
		if field.JSONName != "siteId" && field.JSONName != "vpcId" {
			ordered = append(ordered, field)
		}
	}
	return ordered
}

func generatedBodyScopeValue(s *Session, jsonName string) (string, bool) {
	if s == nil {
		return "", false
	}
	var value string
	switch jsonName {
	case "siteId":
		value = strings.TrimSpace(s.Scope.SiteID)
	case "vpcId":
		value = strings.TrimSpace(s.Scope.VpcID)
	}
	return value, value != ""
}

func promptGeneratedBodyField(
	s *Session,
	info appcli.GeneratedCommandInfo,
	field appcli.GeneratedCommandBodyFormField,
	resolvedValues map[string]string,
	prompter generatedBodyFormPrompter,
) (interface{}, bool, error) {
	field = applyGeneratedBodyFieldPolicy(info, field)
	label := generatedParameterLabel(field.JSONName)
	descriptor, isResource := generatedBodyFieldResourceDescriptor(
		s,
		info,
		field.JSONName,
		resolvedValues,
	)

	if field.Type == "" || field.Type == "object" ||
		(field.Type == "array" && (field.ItemType == "object" || field.ItemType == "array")) {
		return promptGeneratedBodyComposite(
			s,
			info,
			field,
			resolvedValues,
			label,
			prompter,
		)
	}

	if !field.Required && generatedBodyFieldNeedsOptIn(field, isResource) {
		set, err := prompter.Confirm("Set " + label + "?")
		if err != nil || !set {
			return nil, false, err
		}
	}

	switch field.Type {
	case "array":
		return promptGeneratedBodyArray(
			s,
			field,
			descriptor,
			isResource,
			resolvedValues,
			label,
			prompter,
		)
	default:
		return promptGeneratedBodyScalar(
			s,
			field,
			descriptor,
			isResource,
			resolvedValues,
			label,
			prompter,
		)
	}
}

func applyGeneratedBodyFieldPolicy(
	info appcli.GeneratedCommandInfo,
	field appcli.GeneratedCommandBodyFormField,
) appcli.GeneratedCommandBodyFormField {
	if info.Name == "expected-machine batch-update" && field.JSONName == "id" {
		field.Required = true
	}
	return field
}

func promptGeneratedBodyComposite(
	s *Session,
	info appcli.GeneratedCommandInfo,
	field appcli.GeneratedCommandBodyFormField,
	resolvedValues map[string]string,
	label string,
	prompter generatedBodyFormPrompter,
) (interface{}, bool, error) {
	canGuide := field.Type != "" && len(field.Properties) > 0 &&
		(field.Type == "object" || field.Type == "array" && field.ItemType == "object")
	modes := []string{"json"}
	defaultMode := "json"
	if canGuide {
		modes = []string{"guided", "json"}
		defaultMode = "guided"
	}
	if !field.Required {
		modes = append(modes, "skip")
		defaultMode = "skip"
	}
	mode, err := prompter.Choice(label+" input", modes, defaultMode)
	if err != nil {
		return nil, false, err
	}
	switch mode {
	case "skip":
		return nil, false, nil
	case "json":
		return promptGeneratedJSONField(field, label, prompter)
	case "guided":
		if !canGuide {
			return nil, false, fmt.Errorf("guided input is unavailable; use JSON input")
		}
	default:
		return nil, false, fmt.Errorf("unsupported input mode %q", mode)
	}

	if field.Type == "object" {
		nestedResolved := copyGeneratedBodyResolvedValues(resolvedValues)
		value, err := buildGeneratedBodyFormProperties(
			s,
			info,
			field.Properties,
			nestedResolved,
			prompter,
		)
		if err != nil {
			return nil, false, err
		}
		return value, true, nil
	}

	var items []map[string]interface{}
	for {
		nestedResolved := copyGeneratedBodyResolvedValues(resolvedValues)
		item, err := buildGeneratedBodyFormProperties(
			s,
			info,
			field.Properties,
			nestedResolved,
			prompter,
		)
		if err != nil {
			return nil, false, err
		}
		items = append(items, item)
		more, err := prompter.Confirm("Add another " + label + " item?")
		if err != nil {
			return nil, false, err
		}
		if !more {
			break
		}
	}
	return items, true, nil
}

func copyGeneratedBodyResolvedValues(values map[string]string) map[string]string {
	copied := make(map[string]string, len(values))
	for key, value := range values {
		copied[key] = value
	}
	return copied
}

func generatedBodyFieldNeedsOptIn(
	field appcli.GeneratedCommandBodyFormField,
	isResource bool,
) bool {
	if isResource || len(field.Enum) > 0 || field.Type == "boolean" {
		return true
	}
	if field.Type == "array" && (len(field.ItemEnum) > 0 || field.ItemType == "boolean") {
		return true
	}
	return false
}

func generatedBodyFieldResourceDescriptor(
	s *Session,
	info appcli.GeneratedCommandInfo,
	jsonName string,
	resolvedValues map[string]string,
) (GeneratedResourceDescriptor, bool) {
	lower := strings.ToLower(jsonName)
	if !strings.HasSuffix(lower, "id") && !strings.HasSuffix(lower, "ids") {
		return GeneratedResourceDescriptor{}, false
	}
	// A root property named only "id" is an authored identity for the object
	// being created or updated, not a reference to an existing object.
	if lower == "id" {
		if info.Name == "expected-machine batch-update" {
			descriptor := GeneratedPathResourceDescriptor(info.Name, "expectedMachineId")
			descriptor.ResourceType = "expected-machine"
			return descriptor, s != nil && s.Resolver != nil &&
				s.Resolver.HasFetcher(descriptor.ResourceType)
		}
		return GeneratedResourceDescriptor{}, false
	}
	// Expected-inventory rackId values are authored inventory coordinates
	// (for example "rack-01"), not UUID references to the rack list surface.
	if strings.HasPrefix(info.Name, "expected-") && lower == "rackid" {
		return GeneratedResourceDescriptor{}, false
	}
	descriptor := GeneratedPathResourceDescriptor(info.Name, jsonName)
	switch lower {
	case "ipv4blockid", "ipv6blockid":
		descriptor.ResourceType = "ip-block"
	case "partitionid":
		if strings.HasPrefix(info.Name, "instance ") {
			descriptor.ResourceType = "infiniband-partition"
		}
	case "resourcetypeid":
		if info.Name != "allocation create" {
			return descriptor, false
		}
		switch resolvedValues["resourceType"] {
		case "InstanceType":
			descriptor.ResourceType = "instance-type"
		case "IPBlock":
			descriptor.ResourceType = "ip-block"
		default:
			return descriptor, false
		}
	}
	if s == nil || s.Resolver == nil {
		return descriptor, false
	}
	if _, ok := generatedBodySessionResourceTypes[descriptor.ResourceType]; ok {
		return descriptor, true
	}
	return descriptor, s.Resolver.HasFetcher(descriptor.ResourceType)
}

func promptGeneratedBodyScalar(
	s *Session,
	field appcli.GeneratedCommandBodyFormField,
	descriptor GeneratedResourceDescriptor,
	isResource bool,
	resolvedValues map[string]string,
	label string,
	prompter generatedBodyFormPrompter,
) (interface{}, bool, error) {
	if isResource {
		items, supported, err := generatedBodyResourceItems(
			context.Background(),
			s,
			descriptor,
			resolvedValues,
		)
		if err != nil {
			return nil, false, err
		}
		if !supported {
			return nil, false, fmt.Errorf("no selector available for %s", descriptor.ResourceType)
		}
		item, err := s.Resolver.SelectFromItems(label, items)
		if err != nil {
			return nil, false, err
		}
		persistGeneratedBodyScopeSelection(s, field.JSONName, item)
		return item.ID, true, nil
	}

	if field.Type == "boolean" {
		options := field.Enum
		if len(options) == 0 {
			options = []string{"true", "false"}
		}
		value, err := prompter.Choice(label, options, "")
		if err != nil {
			return nil, false, err
		}
		parsed, err := strconv.ParseBool(value)
		return parsed, err == nil, err
	}
	if len(field.Enum) > 0 {
		value, err := prompter.Choice(label, field.Enum, "")
		if err != nil {
			return nil, false, err
		}
		coerced, err := coerceGeneratedBodyValue(value, field.Type)
		return coerced, err == nil, err
	}

	promptLabel := label
	if !field.Required {
		promptLabel += " (optional)"
	}
	var (
		value string
		err   error
	)
	if generatedSensitiveName(field.JSONName) {
		value, err = prompter.Secret(promptLabel, field.Required)
	} else {
		value, err = prompter.Text(promptLabel, field.Required)
	}
	if err != nil {
		return nil, false, err
	}
	value = strings.TrimSpace(value)
	if value == "" && !field.Required {
		return nil, false, nil
	}
	coerced, err := coerceGeneratedBodyValue(value, field.Type)
	return coerced, err == nil, err
}

func promptGeneratedBodyArray(
	s *Session,
	field appcli.GeneratedCommandBodyFormField,
	descriptor GeneratedResourceDescriptor,
	isResource bool,
	resolvedValues map[string]string,
	label string,
	prompter generatedBodyFormPrompter,
) (interface{}, bool, error) {
	if field.ItemType == "object" || field.ItemType == "array" {
		return promptGeneratedJSONField(field, label, prompter)
	}
	if isResource {
		values, err := selectGeneratedBodyResources(s, descriptor, resolvedValues, label, prompter)
		return values, err == nil, err
	}
	if len(field.ItemEnum) > 0 || field.ItemType == "boolean" {
		options := field.ItemEnum
		if field.ItemType == "boolean" && len(options) == 0 {
			options = []string{"true", "false"}
		}
		values, err := chooseGeneratedBodyArrayValues(label, options, field.ItemType, prompter)
		return values, err == nil, err
	}

	promptLabel := label + " (comma-separated"
	if !field.Required {
		promptLabel += ", optional"
	}
	promptLabel += ")"
	value, err := prompter.Text(promptLabel, field.Required)
	if err != nil {
		return nil, false, err
	}
	value = strings.TrimSpace(value)
	if value == "" && !field.Required {
		return nil, false, nil
	}
	parts := strings.Split(value, ",")
	values := make([]interface{}, 0, len(parts))
	for _, part := range parts {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		coerced, err := coerceGeneratedBodyValue(part, field.ItemType)
		if err != nil {
			return nil, false, err
		}
		values = append(values, coerced)
	}
	if len(values) == 0 && field.Required {
		return nil, false, fmt.Errorf("at least one value is required")
	}
	return values, len(values) > 0, nil
}

func selectGeneratedBodyResources(
	s *Session,
	descriptor GeneratedResourceDescriptor,
	resolvedValues map[string]string,
	label string,
	prompter generatedBodyFormPrompter,
) ([]string, error) {
	items, supported, err := generatedBodyResourceItems(
		context.Background(),
		s,
		descriptor,
		resolvedValues,
	)
	if err != nil {
		return nil, err
	}
	if !supported {
		return nil, fmt.Errorf("no selector available for %s", descriptor.ResourceType)
	}
	available := append([]NamedItem(nil), items...)
	if len(available) == 0 {
		return nil, fmt.Errorf("no %s available", label)
	}

	var ids []string
	for len(available) > 0 {
		item, err := s.Resolver.SelectFromItems(label, available)
		if err != nil {
			return nil, err
		}
		ids = append(ids, item.ID)
		for i := range available {
			if available[i].ID == item.ID {
				available = append(available[:i], available[i+1:]...)
				break
			}
		}
		if len(available) == 0 {
			break
		}
		more, err := prompter.Confirm("Add another " + label + "?")
		if err != nil {
			return nil, err
		}
		if !more {
			break
		}
	}
	return ids, nil
}

func generatedBodyResourceItems(
	ctx context.Context,
	s *Session,
	descriptor GeneratedResourceDescriptor,
	resolvedValues map[string]string,
) ([]NamedItem, bool, error) {
	if s == nil || s.Resolver == nil {
		return nil, false, nil
	}
	if _, ok := generatedBodySessionResourceTypes[descriptor.ResourceType]; ok {
		return s.GeneratedResourceItems(ctx, descriptor, resolvedValues)
	}
	fetcher, supported := s.Resolver.fetchers[descriptor.ResourceType]
	if !supported {
		return nil, false, nil
	}

	originalScope := s.Scope
	if siteID := strings.TrimSpace(resolvedValues["siteId"]); siteID != "" {
		s.Scope.SiteID = siteID
		if siteID != originalScope.SiteID {
			s.Scope.SiteName = ""
		}
	}
	if vpcID := strings.TrimSpace(resolvedValues["vpcId"]); vpcID != "" {
		s.Scope.VpcID = vpcID
		if vpcID != originalScope.VpcID {
			s.Scope.VpcName = ""
		}
	}
	defer func() {
		s.Scope = originalScope
	}()

	items, err := fetcher(ctx)
	return items, true, err
}

func persistGeneratedBodyScopeSelection(s *Session, jsonName string, item *NamedItem) {
	if s == nil || item == nil {
		return
	}
	switch jsonName {
	case "siteId":
		changed := s.Scope.SiteID != item.ID || s.Scope.VpcID != ""
		s.Scope.SiteID = item.ID
		s.Scope.SiteName = item.Name
		s.Scope.VpcID = ""
		s.Scope.VpcName = ""
		if changed && s.Cache != nil {
			s.Cache.InvalidateFiltered()
		}
	case "vpcId":
		changed := s.Scope.VpcID != item.ID
		s.Scope.VpcID = item.ID
		s.Scope.VpcName = item.Name
		if siteID := strings.TrimSpace(item.Extra["siteId"]); siteID != "" && s.Scope.SiteID == "" {
			s.Scope.SiteID = siteID
			s.Scope.SiteName = s.Resolver.ResolveID("site", siteID)
			changed = true
		}
		if changed && s.Cache != nil {
			s.Cache.InvalidateFiltered()
		}
	}
}

func chooseGeneratedBodyArrayValues(
	label string,
	options []string,
	itemType appcli.SchemaType,
	prompter generatedBodyFormPrompter,
) ([]interface{}, error) {
	var values []interface{}
	remaining := append([]string(nil), options...)
	for len(remaining) > 0 {
		value, err := prompter.Choice(label, remaining, "")
		if err != nil {
			return nil, err
		}
		coerced, err := coerceGeneratedBodyValue(value, itemType)
		if err != nil {
			return nil, err
		}
		values = append(values, coerced)
		for i, option := range remaining {
			if strings.EqualFold(option, value) {
				remaining = append(remaining[:i], remaining[i+1:]...)
				break
			}
		}
		if len(remaining) == 0 {
			break
		}
		more, err := prompter.Confirm("Add another " + label + "?")
		if err != nil {
			return nil, err
		}
		if !more {
			break
		}
	}
	return values, nil
}

func promptGeneratedJSONField(
	field appcli.GeneratedCommandBodyFormField,
	label string,
	prompter generatedBodyFormPrompter,
) (interface{}, bool, error) {
	promptLabel := label + " as JSON"
	if !field.Required {
		promptLabel += " (optional)"
	}
	var (
		raw string
		err error
	)
	if generatedSensitiveName(field.JSONName) {
		raw, err = prompter.Secret(promptLabel, field.Required)
	} else {
		raw, err = prompter.Text(promptLabel, field.Required)
	}
	if err != nil {
		return nil, false, err
	}
	raw = strings.TrimSpace(raw)
	if raw == "" && !field.Required {
		return nil, false, nil
	}

	var value interface{}
	if err := json.Unmarshal([]byte(raw), &value); err != nil {
		return nil, false, fmt.Errorf("invalid JSON: %w", err)
	}
	switch field.Type {
	case "object":
		if _, ok := value.(map[string]interface{}); !ok {
			return nil, false, fmt.Errorf("expected a JSON object")
		}
	case "array":
		if _, ok := value.([]interface{}); !ok {
			return nil, false, fmt.Errorf("expected a JSON array")
		}
	}
	return value, true, nil
}

func coerceGeneratedBodyValue(value string, schemaType appcli.SchemaType) (interface{}, error) {
	switch schemaType {
	case "integer":
		parsed, err := strconv.Atoi(value)
		if err != nil {
			return nil, fmt.Errorf("expected integer, got %q", value)
		}
		return parsed, nil
	case "number":
		parsed, err := strconv.ParseFloat(value, 64)
		if err != nil {
			return nil, fmt.Errorf("expected number, got %q", value)
		}
		return parsed, nil
	case "boolean":
		parsed, err := strconv.ParseBool(value)
		if err != nil {
			return nil, fmt.Errorf("expected boolean, got %q", value)
		}
		return parsed, nil
	default:
		return value, nil
	}
}
