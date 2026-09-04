// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"sort"
	"strconv"
	"strings"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
	"github.com/NVIDIA/infra-controller/rest-api/openapi"
)

// generatedCommandAliases records generated REST leaves already covered by a
// specialized interactive command with a different name. The specialized
// handlers keep their richer prompts and output.
var generatedCommandAliases = map[string]string{
	"machine dpu-machines get":                                          "machine dpu get",
	"rack bringup bringup-rack":                                         "rack bringup",
	"rack firmware-update firmware-update-rack":                         "rack firmware",
	"rack power-control power-control-rack":                             "rack power",
	"rack validate validate-rack":                                       "rack validate",
	"task get":                                                          "rack task get",
	"task cancel cancel-task":                                           "rack task cancel",
	"tray firmware-update firmware-update-tray":                         "tray firmware",
	"tray power-control power-control-tray":                             "tray power",
	"tray validate validate-tray":                                       "tray validate",
	"tenant-identity or-update create":                                  "tenant-identity update",
	"tenant-identity or-update-tenant-identity-token-delegation create": "tenant-identity token-delegation update",
	"tenant-identity tenant-identity-token-delegation get":              "tenant-identity token-delegation get",
	"tenant-identity tenant-identity-token-delegation delete":           "tenant-identity token-delegation delete",
	"user get": "user current",
}

// generatedCommandExclusions maps a generated REST command to its reviewed
// exclusion reason. It is intentionally empty: every current REST operation
// is available in the interactive interface.
var generatedCommandExclusions = map[string]string{}

var embeddedGeneratedSpec = func() *appcli.Spec {
	spec, err := appcli.ParseSpec(openapi.Spec)
	if err != nil {
		panic(fmt.Sprintf("parsing embedded OpenAPI spec for TUI commands: %v", err))
	}
	return spec
}()

var embeddedGeneratedCommandInfos = appcli.GeneratedCommandInfos(embeddedGeneratedSpec)

func appendGeneratedCommands(commands []Command) []Command {
	return appendGeneratedCommandInfos(commands, embeddedGeneratedCommandInfos)
}

func appendGeneratedCommandInfos(commands []Command, infos []appcli.GeneratedCommandInfo) []Command {
	registered := make(map[string]struct{}, len(commands))
	for _, command := range commands {
		registered[command.Name] = struct{}{}
	}

	generated := make([]Command, 0, len(infos))
	for _, info := range infos {
		if _, exists := registered[info.Name]; exists {
			continue
		}
		if _, excluded := generatedCommandExclusions[info.Name]; excluded {
			continue
		}

		info := info
		generated = append(generated, Command{
			Name:        info.Name,
			Description: info.Description,
			Sensitive:   generatedCommandHasSensitiveInput(info),
			Run: func(s *Session, args []string) error {
				return runGeneratedTUICommand(s, info, args)
			},
		})
	}
	sort.Slice(generated, func(i, j int) bool {
		return generated[i].Name < generated[j].Name
	})
	return append(commands, generated...)
}

func runGeneratedTUICommand(s *Session, info appcli.GeneratedCommandInfo, args []string) error {
	if s == nil || s.Client == nil {
		return fmt.Errorf("interactive session client is required")
	}

	flagArgs, positionalArgs, err := splitGeneratedArguments(info, args)
	if err != nil {
		return err
	}
	if generatedBoolOptionEnabled(info, flagArgs, "help", "h") {
		return appcli.RunGeneratedCommand(embeddedGeneratedSpec, s.Client, info.Name, args)
	}
	if err := validateGeneratedBodyArguments(info, flagArgs); err != nil {
		return err
	}
	persistGeneratedSiteScopeFromArgs(s, info, flagArgs)
	flagArgs, err = addRequiredGeneratedQueryFlags(s, info, flagArgs)
	if err != nil {
		return err
	}
	if generatedCommandRequiresSiteScope(info, flagArgs) &&
		strings.TrimSpace(s.Scope.SiteID) == "" &&
		s.Resolver != nil {
		if _, err = requireSiteScope(s, fmt.Sprintf("%s requires a site. Select a site.", info.Name)); err != nil {
			return err
		}
	}
	positionalArgs, err = resolveGeneratedPathParameters(s, info, positionalArgs)
	if err != nil {
		return err
	}
	var proceed bool
	flagArgs, proceed, err = addMachineDeleteForceFlag(info, flagArgs, positionalArgs)
	if err != nil || !proceed {
		return err
	}

	flagArgs, err = addGeneratedBodyForm(s, info, flagArgs)
	if err != nil {
		return err
	}

	flagArgs, err = addGeneratedScopeFlags(s, info, flagArgs)
	if err != nil {
		return err
	}
	if generatedCommandSupportsPagination(info) &&
		!hasGeneratedOption(info, flagArgs, "all", "page-number", "page-size") {
		flagArgs = append(flagArgs, "--all")
	}
	preparedArgs := append(flagArgs, positionalArgs...)

	if info.Method != http.MethodGet {
		ok, confirmErr := PromptConfirm(fmt.Sprintf("Run %s (%s)?", info.Name, info.Method))
		if confirmErr != nil || !ok {
			return confirmErr
		}
	}

	logGeneratedCommand(s, info, preparedArgs)
	if err := appcli.RunGeneratedCommand(embeddedGeneratedSpec, s.Client, info.Name, preparedArgs); err != nil {
		return err
	}
	if info.Method != http.MethodGet && s.Cache != nil {
		s.Cache.InvalidateAll()
	}
	return nil
}

func addMachineDeleteForceFlag(
	info appcli.GeneratedCommandInfo,
	args []string,
	positionalArgs []string,
) ([]string, bool, error) {
	if info.OperationID != "delete-machine" {
		return args, true, nil
	}

	forceProvided := hasGeneratedOption(info, args, "force")
	if forceProvided && !generatedBoolOptionEnabled(info, args, "force") {
		return args, true, nil
	}

	machineID := "the selected Machine"
	if len(positionalArgs) > 0 {
		machineID = positionalArgs[0]
	}
	force, err := PromptConfirm(fmt.Sprintf(
		"Force delete Machine %s? This overrides the assigned Instance Type restriction and removes the Machine's host, DPU, and BMC interfaces. Machines with attached Instances are rejected.",
		machineID,
	))
	if err != nil {
		return args, false, err
	}
	if !force {
		return args, !forceProvided, nil
	}
	if forceProvided {
		return args, true, nil
	}
	return append(args, "--force=true"), true, nil
}

func splitGeneratedArguments(info appcli.GeneratedCommandInfo, args []string) ([]string, []string, error) {
	flagTakesValue := map[string]bool{
		"help": false,
		"h":    false,
	}
	for _, flag := range info.Flags {
		flagTakesValue[flag.Name] = flag.TakesValue
	}

	var flagArgs []string
	for i := 0; i < len(args); {
		token := args[i]
		if token == "--" {
			return flagArgs, append([]string(nil), args[i+1:]...), nil
		}
		if !isGeneratedFlagToken(token) {
			return flagArgs, append([]string(nil), args[i:]...), nil
		}

		name, hasInlineValue := generatedFlagName(token)
		takesValue, exists := flagTakesValue[name]
		if !exists {
			return nil, nil, fmt.Errorf("unknown option --%s for %s", name, info.Name)
		}
		flagArgs = append(flagArgs, token)
		i++
		if !takesValue || hasInlineValue {
			continue
		}
		if i >= len(args) {
			return nil, nil, fmt.Errorf("option --%s requires a value", name)
		}
		flagArgs = append(flagArgs, args[i])
		i++
	}
	return flagArgs, nil, nil
}

func isGeneratedFlagToken(token string) bool {
	return len(token) > 1 && strings.HasPrefix(token, "-")
}

func generatedFlagName(token string) (string, bool) {
	name := strings.TrimLeft(token, "-")
	if before, _, ok := strings.Cut(name, "="); ok {
		return before, true
	}
	return name, false
}

func resolveGeneratedPathParameters(s *Session, info appcli.GeneratedCommandInfo, args []string) ([]string, error) {
	resolved := append([]string(nil), args...)
	resolvedValues := map[string]string{
		"siteId": strings.TrimSpace(s.Scope.SiteID),
		"vpcId":  strings.TrimSpace(s.Scope.VpcID),
	}
	for i, parameter := range info.PathParameters {
		value := ""
		if i < len(resolved) {
			value = resolved[i]
		}
		descriptor := GeneratedPathResourceDescriptor(info.Name, parameter)

		if value == "" {
			switch descriptor.ResourceType {
			case "site":
				value = strings.TrimSpace(s.Scope.SiteID)
			case "vpc":
				value = strings.TrimSpace(s.Scope.VpcID)
			}
		}

		item, supported, err := s.ResolveGeneratedResource(
			context.Background(),
			descriptor,
			resolvedValues,
			generatedParameterLabel(parameter),
			value,
		)
		if err != nil {
			return nil, err
		}
		if supported {
			value = item.ID
		} else if value == "" && descriptor.FreeFormReason == "" && s.Resolver != nil {
			return nil, fmt.Errorf(
				"%s has no interactive selector for path parameter %s",
				info.Name,
				parameter,
			)
		} else if value == "" {
			value, err = PromptText(
				generatedParameterLabel(parameter),
				true,
			)
			if err != nil {
				return nil, err
			}
		}

		if i < len(resolved) {
			resolved[i] = value
		} else {
			resolved = append(resolved, value)
		}
		resolvedValues[parameter] = value
	}
	return resolved, nil
}

func addRequiredGeneratedQueryFlags(s *Session, info appcli.GeneratedCommandInfo, args []string) ([]string, error) {
	out := append([]string(nil), args...)
	for _, parameter := range info.QueryParameters {
		if !parameter.Required {
			continue
		}

		value := ""
		resourceType := generatedResourceType(info, parameter.Name)
		if providedValue, provided := generatedOptionValue(info, out, parameter.FlagName); provided {
			if resourceType == "site" {
				setGeneratedSiteScopeFromID(s, providedValue)
			}
			continue
		}
		switch resourceType {
		case "site":
			value = strings.TrimSpace(s.Scope.SiteID)
		case "vpc":
			value = strings.TrimSpace(s.Scope.VpcID)
		}
		if value == "" && s.Resolver != nil && s.Resolver.HasFetcher(resourceType) {
			item, err := s.Resolver.Resolve(
				context.Background(),
				resourceType,
				generatedParameterLabel(parameter.Name),
			)
			if err != nil {
				return nil, err
			}
			value = item.ID
		}
		if value == "" {
			label := strings.TrimSpace(parameter.Description)
			if label == "" {
				label = generatedParameterLabel(parameter.Name)
			}
			var err error
			switch {
			case len(parameter.Enum) > 0:
				value, err = PromptChoice(label, parameter.Enum, "")
			case parameter.Type == "boolean":
				value, err = PromptChoice(label, []string{"true", "false"}, "")
			default:
				value, err = PromptText(label, true)
			}
			if err != nil {
				return nil, err
			}
		}

		if resourceType == "site" {
			setGeneratedSiteScopeFromID(s, value)
		}
		if parameter.Type == "boolean" {
			out = append(out, "--"+parameter.FlagName+"="+value)
		} else {
			out = append(out, "--"+parameter.FlagName, value)
		}
	}
	return out, nil
}

func generatedOptionValue(
	info appcli.GeneratedCommandInfo,
	args []string,
	name string,
) (string, bool) {
	for _, option := range generatedArgumentOptions(info, args) {
		if option.name == name {
			return strings.TrimSpace(option.value), true
		}
	}
	return "", false
}

func persistGeneratedSiteScopeFromArgs(
	s *Session,
	info appcli.GeneratedCommandInfo,
	args []string,
) {
	if siteID, ok := generatedOptionValue(info, args, "site-id"); ok {
		setGeneratedSiteScopeFromID(s, siteID)
	}
}

func setGeneratedSiteScopeFromID(s *Session, siteID string) {
	siteID = strings.TrimSpace(siteID)
	if s == nil || siteID == "" || s.Scope.SiteID == siteID {
		return
	}
	if s.Resolver != nil && s.Cache != nil {
		setSiteScopeFromID(s, siteID)
		return
	}
	s.Scope.SiteID = siteID
	s.Scope.SiteName = siteID
	s.Scope.VpcID = ""
	s.Scope.VpcName = ""
	if s.Cache != nil {
		s.Cache.InvalidateFiltered()
	}
}

func generatedCommandRequiresSiteScope(
	info appcli.GeneratedCommandInfo,
	args []string,
) bool {
	resource, _, _ := strings.Cut(info.Name, " ")
	switch resource {
	case "rack", "rule", "task-run", "tray":
		// Guided forms and scalar body flags need a persisted scope so their
		// resource selectors use the correct Site. Opaque JSON already
		// carries its own siteId and remains an explicit escape hatch unless
		// a path or required query still needs scoped resource resolution.
		if !hasGeneratedOption(info, args, "data", "data-file") {
			return true
		}
		if len(info.PathParameters) > 0 {
			return true
		}
		for _, parameter := range info.QueryParameters {
			if parameter.Required &&
				CanonicalGeneratedResourceType(info.Name, parameter.Name) == "site" {
				return true
			}
		}
		return false
	default:
		return false
	}
}

func generatedResourceType(info appcli.GeneratedCommandInfo, parameter string) string {
	return CanonicalGeneratedResourceType(info.Name, parameter)
}

func camelToKebab(value string) string {
	var result strings.Builder
	for i, current := range []byte(value) {
		if current >= 'A' && current <= 'Z' {
			previousIsLower := i > 0 && value[i-1] >= 'a' && value[i-1] <= 'z'
			nextIsLower := i+1 < len(value) && value[i+1] >= 'a' && value[i+1] <= 'z'
			previousIsUpper := i > 0 && value[i-1] >= 'A' && value[i-1] <= 'Z'
			if result.Len() > 0 && (previousIsLower || previousIsUpper && nextIsLower) {
				result.WriteByte('-')
			}
			result.WriteByte(current - 'A' + 'a')
			continue
		}
		result.WriteByte(current)
	}
	return result.String()
}

func generatedParameterLabel(parameter string) string {
	label := strings.ReplaceAll(camelToKebab(parameter), "-", " ")
	if label == "" {
		return "Resource ID"
	}
	return strings.ToUpper(label[:1]) + label[1:]
}

func hasGeneratedBodyInput(info appcli.GeneratedCommandInfo, args []string) bool {
	if hasGeneratedOption(info, args, "data", "data-file") {
		return true
	}
	for _, field := range info.BodyFields {
		if hasGeneratedOption(info, args, field.FlagName) {
			return true
		}
	}
	return false
}

func validateGeneratedBodyArguments(info appcli.GeneratedCommandInfo, args []string) error {
	hasData := hasGeneratedOption(info, args, "data")
	hasDataFile := hasGeneratedOption(info, args, "data-file")
	if hasData && hasDataFile {
		return fmt.Errorf("--data and --data-file cannot be combined")
	}

	sourceFlag := ""
	switch {
	case hasData:
		sourceFlag = "data"
	case hasDataFile:
		sourceFlag = "data-file"
	default:
		return nil
	}
	for _, field := range info.BodyFields {
		if hasGeneratedOption(info, args, field.FlagName) {
			return fmt.Errorf("--%s cannot be combined with body field flag --%s", sourceFlag, field.FlagName)
		}
	}
	return nil
}

type generatedArgumentOption struct {
	index      int
	valueIndex int
	name       string
	value      string
	inline     bool
}

func generatedArgumentOptions(info appcli.GeneratedCommandInfo, args []string) []generatedArgumentOption {
	flagTakesValue := map[string]bool{
		"help": false,
		"h":    false,
	}
	for _, flag := range info.Flags {
		flagTakesValue[flag.Name] = flag.TakesValue
	}

	var options []generatedArgumentOption
	for i := 0; i < len(args); {
		token := args[i]
		if token == "--" || !isGeneratedFlagToken(token) {
			break
		}
		name, inline := generatedFlagName(token)
		takesValue, exists := flagTakesValue[name]
		if !exists {
			i++
			continue
		}
		option := generatedArgumentOption{
			index:      i,
			valueIndex: -1,
			name:       name,
			inline:     inline,
		}
		if inline {
			_, option.value, _ = strings.Cut(token, "=")
		} else if takesValue && i+1 < len(args) {
			option.valueIndex = i + 1
			option.value = args[i+1]
			i++
		}
		options = append(options, option)
		i++
	}
	return options
}

func hasGeneratedOption(info appcli.GeneratedCommandInfo, args []string, names ...string) bool {
	wanted := make(map[string]struct{}, len(names))
	for _, name := range names {
		wanted[name] = struct{}{}
	}
	for _, option := range generatedArgumentOptions(info, args) {
		if _, exists := wanted[option.name]; exists {
			return true
		}
	}
	return false
}

func generatedBoolOptionEnabled(info appcli.GeneratedCommandInfo, args []string, names ...string) bool {
	wanted := make(map[string]struct{}, len(names))
	for _, name := range names {
		wanted[name] = struct{}{}
	}
	for _, option := range generatedArgumentOptions(info, args) {
		if _, exists := wanted[option.name]; !exists {
			continue
		}
		if !option.inline {
			return true
		}
		enabled, err := strconv.ParseBool(option.value)
		if err == nil && enabled {
			return true
		}
	}
	return false
}

func generatedCommandAcceptsFlag(info appcli.GeneratedCommandInfo, name string) bool {
	for _, flag := range info.Flags {
		if flag.Name == name {
			return true
		}
	}
	return false
}

func generatedCommandSupportsPagination(info appcli.GeneratedCommandInfo) bool {
	if !generatedCommandAcceptsFlag(info, "all") {
		return false
	}
	hasPageNumber := false
	hasPageSize := false
	for _, parameter := range info.QueryParameters {
		switch parameter.FlagName {
		case "page-number":
			hasPageNumber = true
		case "page-size":
			hasPageSize = true
		}
	}
	return hasPageNumber && hasPageSize
}

func generatedCommandHasSensitiveBody(info appcli.GeneratedCommandInfo) bool {
	if generatedSensitiveName(info.OperationID) {
		return true
	}
	for _, name := range info.BodyPropertyNames {
		if generatedSensitiveName(name) {
			return true
		}
	}
	return false
}

func generatedCommandHasSensitiveInput(info appcli.GeneratedCommandInfo) bool {
	if generatedCommandHasSensitiveBody(info) {
		return true
	}
	for _, flag := range info.Flags {
		if generatedSensitiveName(flag.Name) {
			return true
		}
	}
	return false
}

func generatedSensitiveName(name string) bool {
	normalized := strings.NewReplacer("-", "", "_", "").Replace(strings.ToLower(name))
	for _, fragment := range []string{"credential", "password", "secret", "token", "privatekey"} {
		if strings.Contains(normalized, fragment) {
			return true
		}
	}
	return false
}

func addGeneratedScopeFlags(s *Session, info appcli.GeneratedCommandInfo, args []string) ([]string, error) {
	out := append([]string(nil), args...)
	queryFlags := make(map[string]struct{}, len(info.QueryParameters))
	for _, parameter := range info.QueryParameters {
		queryFlags[parameter.FlagName] = struct{}{}
	}

	scopes := []struct {
		flagName string
		jsonName string
		value    string
	}{
		{flagName: "site-id", jsonName: "siteId", value: strings.TrimSpace(s.Scope.SiteID)},
		{flagName: "vpc-id", jsonName: "vpcId", value: strings.TrimSpace(s.Scope.VpcID)},
	}
	for _, scope := range scopes {
		if scope.value == "" {
			continue
		}
		if _, accepted := queryFlags[scope.flagName]; accepted && !hasGeneratedOption(info, out, scope.flagName) {
			out = append(out, "--"+scope.flagName, scope.value)
		}

		jsonName, accepted := generatedRootBodyProperty(info, scope.flagName)
		if !accepted || hasGeneratedOption(info, out, scope.flagName) {
			continue
		}
		if hasGeneratedOption(info, out, "data-file") {
			var err error
			out, err = materializeGeneratedDataFile(info, out)
			if err != nil {
				return nil, err
			}
		}
		if hasGeneratedOption(info, out, "data") {
			var err error
			out, err = mergeGeneratedDataField(info, out, jsonName, scope.value)
			if err != nil {
				return nil, err
			}
			continue
		}
		if bodyField, scalar := generatedBodyField(info, scope.flagName); scalar {
			out = append(out, "--"+bodyField.FlagName, scope.value)
		}
	}
	return out, nil
}

func generatedRootBodyProperty(info appcli.GeneratedCommandInfo, flagName string) (string, bool) {
	for _, name := range info.RootBodyProperties {
		if camelToKebab(name) == flagName {
			return name, true
		}
	}
	return "", false
}

func generatedBodyField(info appcli.GeneratedCommandInfo, flagName string) (appcli.GeneratedCommandBodyField, bool) {
	for _, field := range info.BodyFields {
		if field.FlagName == flagName {
			return field, true
		}
	}
	return appcli.GeneratedCommandBodyField{}, false
}

func materializeGeneratedDataFile(info appcli.GeneratedCommandInfo, args []string) ([]string, error) {
	out := append([]string(nil), args...)
	for _, option := range generatedArgumentOptions(info, out) {
		if option.name != "data-file" {
			continue
		}
		if option.value == "" {
			return nil, fmt.Errorf("option --data-file requires a path")
		}
		if option.value == "-" {
			return nil, fmt.Errorf("--data-file - cannot be combined with active site or VPC scope in interactive mode; use the hidden JSON prompt or --data")
		}
		body, err := appcli.ReadBodyInput("", option.value)
		if err != nil {
			return nil, err
		}
		if option.inline {
			out[option.index] = "--data=" + string(body)
		} else {
			out[option.index] = "--data"
			out[option.valueIndex] = string(body)
		}
		return out, nil
	}
	return out, nil
}

func mergeGeneratedDataField(
	info appcli.GeneratedCommandInfo,
	args []string,
	jsonName string,
	value string,
) ([]string, error) {
	out := append([]string(nil), args...)
	for _, option := range generatedArgumentOptions(info, out) {
		if option.name != "data" {
			continue
		}

		var body interface{}
		decoder := json.NewDecoder(strings.NewReader(option.value))
		decoder.UseNumber()
		if err := decoder.Decode(&body); err != nil {
			return nil, fmt.Errorf("request body is not JSON: %w", err)
		}
		if err := decoder.Decode(&struct{}{}); err == nil {
			return nil, fmt.Errorf("request body is not JSON: multiple JSON values")
		} else if !errors.Is(err, io.EOF) {
			return nil, fmt.Errorf("request body is not JSON: %w", err)
		}
		switch typed := body.(type) {
		case map[string]interface{}:
			if _, exists := typed[jsonName]; !exists {
				typed[jsonName] = value
			}
		case []interface{}:
			for i, item := range typed {
				object, ok := item.(map[string]interface{})
				if !ok {
					return nil, fmt.Errorf("request body item %d must be a JSON object to apply scoped %s", i, jsonName)
				}
				if _, exists := object[jsonName]; !exists {
					object[jsonName] = value
				}
			}
		default:
			return nil, fmt.Errorf("request body must be a JSON object or array of objects to apply scoped %s", jsonName)
		}
		merged, err := json.Marshal(body)
		if err != nil {
			return nil, fmt.Errorf("adding scoped %s to request body: %w", jsonName, err)
		}
		if option.inline {
			out[option.index] = "--data=" + string(merged)
		} else {
			out[option.valueIndex] = string(merged)
		}
		return out, nil
	}
	return out, nil
}

func logGeneratedCommand(s *Session, info appcli.GeneratedCommandInfo, args []string) {
	parts := []string{"nicocli"}
	if strings.TrimSpace(s.ConfigPath) != "" {
		parts = append(parts, "--config", s.ConfigPath)
	}
	parts = append(parts, strings.Fields(info.Name)...)
	parts = append(parts, redactGeneratedCommandArgs(info, args)...)
	for i, part := range parts {
		parts[i] = quoteShellCommandArgument(part)
	}
	fmt.Printf("%s %s\n", Dim("INFO:"), strings.Join(parts, " "))
}

func quoteShellCommandArgument(value string) string {
	if value != "" && strings.IndexFunc(value, func(char rune) bool {
		return !strings.ContainsRune("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_@%+=:,./-", char)
	}) == -1 {
		return value
	}
	return "'" + strings.ReplaceAll(value, "'", "'\"'\"'") + "'"
}

func redactGeneratedCommandArgs(info appcli.GeneratedCommandInfo, args []string) []string {
	out := append([]string(nil), args...)
	for _, option := range generatedArgumentOptions(info, out) {
		sensitive := option.name == "data" || generatedSensitiveName(option.name)
		if !sensitive {
			continue
		}
		if option.inline {
			out[option.index] = strings.SplitN(out[option.index], "=", 2)[0] + "=<redacted>"
			continue
		}
		if option.valueIndex >= 0 {
			out[option.valueIndex] = "<redacted>"
		}
	}
	return out
}
