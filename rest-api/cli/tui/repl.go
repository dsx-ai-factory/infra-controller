// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"
	"time"

	appcli "github.com/NVIDIA/infra-controller/rest-api/cli/pkg"
)

const (
	maxSuggestions           = 6
	maxHistory               = 100
	autocompleteFetchTimeout = 2 * time.Second
)

// argResourceMap maps command names to the resource type whose names should
// be offered as argument completions.
var argResourceMap = map[string]string{
	"site get":                      "site",
	"site update":                   "site",
	"site delete":                   "site",
	"vpc get":                       "vpc",
	"vpc update":                    "vpc",
	"vpc virtualization update":     "vpc",
	"vpc delete":                    "vpc",
	"domain get":                    "domain",
	"domain delete":                 "domain",
	"subnet get":                    "subnet",
	"subnet attach-vpc":             "subnet",
	"subnet update":                 "subnet",
	"subnet delete":                 "subnet",
	"instance-type get":             "instance-type",
	"instance get":                  "instance",
	"instance delete":               "instance",
	"allocation get":                "allocation",
	"allocation update":             "allocation",
	"allocation delete":             "allocation",
	"audit get":                     "audit",
	"machine get":                   "machine",
	"machine dpu get":               "machine",
	"ip-block get":                  "ip-block",
	"ip-block update":               "ip-block",
	"ip-block delete":               "ip-block",
	"operating-system get":          "operating-system",
	"operating-system update":       "operating-system",
	"operating-system delete":       "operating-system",
	"ssh-key-group get":             "ssh-key-group",
	"ssh-key-group update":          "ssh-key-group",
	"ssh-key-group delete":          "ssh-key-group",
	"ssh-key get":                   "ssh-key",
	"ssh-key update":                "ssh-key",
	"ssh-key delete":                "ssh-key",
	"sku get":                       "sku",
	"rack get":                      "rack",
	"rack bringup":                  "rack",
	"rack power":                    "rack",
	"rack firmware":                 "rack",
	"rack validate":                 "rack",
	"tray get":                      "tray",
	"tray power":                    "tray",
	"tray firmware":                 "tray",
	"tray validate":                 "tray",
	"vpc-prefix get":                "vpc-prefix",
	"vpc-prefix update":             "vpc-prefix",
	"vpc-prefix delete":             "vpc-prefix",
	"tenant-account get":            "tenant-account",
	"tenant-account update":         "tenant-account",
	"tenant-account delete":         "tenant-account",
	"expected-machine get":          "expected-machine",
	"expected-rack get":             "expected-rack",
	"expected-switch get":           "expected-switch",
	"expected-power-shelf get":      "expected-power-shelf",
	"dpu-extension-service get":     "dpu-extension-service",
	"infiniband-partition get":      "infiniband-partition",
	"nvlink-logical-partition get":  "nvlink-logical-partition",
	"network-security-group get":    "network-security-group",
	"network-security-group update": "network-security-group",
	"network-security-group delete": "network-security-group",

	"tenant-identity get":                      "site",
	"tenant-identity update":                   "site",
	"tenant-identity delete":                   "site",
	"tenant-identity token-delegation get":     "site",
	"tenant-identity token-delegation update":  "site",
	"tenant-identity token-delegation delete":  "site",
	"tenant-identity jwks get":                 "site",
	"tenant-identity spiffe-jwks get":          "site",
	"tenant-identity openid-configuration get": "site",
}

var history []string

// RunREPL starts the interactive REPL loop with inline autocomplete.
func RunREPL(s *Session) error {
	commands := AllCommands()
	cmdNames := make([]string, len(commands))
	cmdMap := make(map[string]Command, len(commands))
	for i, cmd := range commands {
		cmdNames[i] = cmd.Name
		cmdMap[cmd.Name] = cmd
	}
	cmdNames = append(cmdNames, "org", "org list", "org set",
		"scope", "scope site", "scope vpc", "scope label", "scope label clear", "scope clear",
		"exit", "quit")

	fmt.Printf("\n%s\n", Bold("NICo Interactive Mode"))
	fmt.Printf("Org: %s\n", Cyan(s.Org))
	if s.ConfigPath != "" {
		fmt.Printf("Config: %s\n", Dim(s.ConfigPath))
	}
	fmt.Printf("Type a command or %s. %s clears line, %s cancels selections, %s quits.\n\n",
		Bold("help"), Bold("Ctrl+C"), Bold("Esc"), Bold("Ctrl+D"))

	for {
		line, err := readLineWithSuggestions(s, cmdNames)
		if err != nil {
			if err == io.EOF {
				fmt.Println("\nGoodbye.")
				return nil
			}
			return err
		}

		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}

		historyLine := commandHistoryLine(line, cmdMap, commands)
		if len(history) == 0 || history[len(history)-1] != historyLine {
			history = append(history, historyLine)
			if len(history) > maxHistory {
				history = history[1:]
			}
		}

		if line == "exit" || line == "quit" {
			fmt.Println("Goodbye.")
			return nil
		}

		if line == "org" {
			fmt.Printf("Current org: %s\n\n", Cyan(s.Org))
			continue
		}
		if line == "org list" {
			runOrgList(s)
			fmt.Println()
			continue
		}
		if strings.HasPrefix(line, "org set ") {
			newOrg := strings.TrimSpace(line[len("org set "):])
			if newOrg != "" {
				s.Org = newOrg
				s.Client.Org = newOrg
				s.Cache.InvalidateAll()
				fmt.Printf("Org set to: %s\n\n", Cyan(s.Org))
			} else {
				fmt.Fprintf(os.Stderr, "%s org name required\n\n", Red("Error:"))
			}
			continue
		}

		if line == "scope" {
			if s.Scope.SiteID == "" && s.Scope.VpcID == "" && len(s.Scope.LabelFilters) == 0 {
				fmt.Println("No scope set. All list commands return unfiltered results.")
			} else {
				if s.Scope.SiteName != "" {
					fmt.Printf("  site:   %s (%s)\n", Cyan(s.Scope.SiteName), s.Scope.SiteID)
				}
				if s.Scope.VpcName != "" {
					fmt.Printf("  vpc:    %s (%s)\n", Cyan(s.Scope.VpcName), s.Scope.VpcID)
				}
				for k, v := range s.Scope.LabelFilters {
					fmt.Printf("  label:  %s=%s\n", k, Cyan(v))
				}
			}
			fmt.Println()
			continue
		}
		if line == "scope clear" {
			s.Scope = Scope{}
			s.Cache.InvalidateFiltered()
			fmt.Println("Scope cleared.")
			fmt.Println()
			continue
		}
		if line == "scope label clear" {
			s.Scope.LabelFilters = nil
			fmt.Println("Label filters cleared.")
			fmt.Println()
			continue
		}
		if strings.HasPrefix(line, "scope label clear ") {
			key := strings.TrimSpace(line[len("scope label clear "):])
			if key != "" {
				delete(s.Scope.LabelFilters, key)
				fmt.Printf("Label filter %q removed.\n\n", key)
			}
			continue
		}
		if strings.HasPrefix(line, "scope label ") {
			kv := strings.TrimSpace(line[len("scope label "):])
			if k, v, ok := strings.Cut(kv, "="); ok && k != "" {
				if s.Scope.LabelFilters == nil {
					s.Scope.LabelFilters = map[string]string{}
				}
				s.Scope.LabelFilters[k] = v
				fmt.Printf("Label filter set: %s=%s\n\n", k, Cyan(v))
			} else {
				fmt.Fprintf(os.Stderr, "%s expected format: scope label key=value\n\n", Red("Error:"))
			}
			continue
		}
		if line == "scope site" || strings.HasPrefix(line, "scope site ") {
			runScopeSet(s, "site", strings.TrimSpace(strings.TrimPrefix(line, "scope site")))
			continue
		}
		if line == "scope vpc" || strings.HasPrefix(line, "scope vpc ") {
			runScopeSet(s, "vpc", strings.TrimSpace(strings.TrimPrefix(line, "scope vpc")))
			continue
		}

		command, rest, matched := matchCommandLine(line, cmdMap, commands)
		if !matched {
			fmt.Fprintf(os.Stderr, "%s unknown command: %s\n", Red("Error:"), line)
			fmt.Println()
			continue
		}

		args, parseErr := splitCommandArguments(rest)
		if parseErr != nil {
			fmt.Fprintf(os.Stderr, "%s %v\n\n", Red("Error:"), parseErr)
			continue
		}
		if err := command.Run(s, args); err != nil {
			fmt.Fprintf(os.Stderr, "%s %v\n", Red("Error:"), err)
		}
		fmt.Println()
	}
}

func matchCommandLine(line string, commandMap map[string]Command, commands []Command) (Command, string, bool) {
	if command, ok := commandMap[line]; ok {
		return command, "", true
	}

	bestIndex := -1
	for i, command := range commands {
		if !strings.HasPrefix(line, command.Name+" ") {
			continue
		}
		if bestIndex == -1 || len(command.Name) > len(commands[bestIndex].Name) {
			bestIndex = i
		}
	}
	if bestIndex == -1 {
		return Command{}, "", false
	}
	command := commands[bestIndex]
	return command, strings.TrimSpace(line[len(command.Name):]), true
}

func commandHistoryLine(line string, commandMap map[string]Command, commands []Command) string {
	command, rest, matched := matchCommandLine(line, commandMap, commands)
	if !matched || !command.Sensitive || rest == "" {
		return line
	}
	return command.Name + " <redacted>"
}

func splitCommandArguments(input string) ([]string, error) {
	var args []string
	var current strings.Builder
	var quote byte
	escaped := false
	started := false

	flush := func() {
		if !started {
			return
		}
		args = append(args, current.String())
		current.Reset()
		started = false
	}

	for i := 0; i < len(input); i++ {
		char := input[i]
		if escaped {
			current.WriteByte(char)
			started = true
			escaped = false
			continue
		}
		if quote != 0 {
			if char == quote {
				quote = 0
				started = true
				continue
			}
			if char == '\\' && quote == '"' {
				value, multibyte, tail, err := strconv.UnquoteChar(input[i:], quote)
				if err != nil {
					return nil, fmt.Errorf("invalid escape sequence: %w", err)
				}
				if multibyte {
					current.WriteRune(value)
				} else {
					current.WriteByte(byte(value))
				}
				i += len(input[i:]) - len(tail) - 1
				started = true
				continue
			}
			current.WriteByte(char)
			started = true
			continue
		}

		switch char {
		case '\'', '"':
			quote = char
			started = true
		case '\\':
			escaped = true
			started = true
		case ' ', '\t', '\r', '\n':
			flush()
		default:
			current.WriteByte(char)
			started = true
		}
	}
	if escaped {
		return nil, fmt.Errorf("unfinished escape sequence")
	}
	if quote != 0 {
		return nil, fmt.Errorf("unterminated %q quote", string(quote))
	}
	flush()
	return args, nil
}

func readLineWithSuggestions(s *Session, cmdNames []string) (_ string, err error) {
	restore, err := RawMode()
	if err != nil {
		return "", err
	}
	defer func() {
		if restore != nil {
			restoreErr := restore()
			if restoreErr != nil {
				if err == nil {
					err = restoreErr
				} else {
					err = errors.Join(err, restoreErr)
				}
			}
		}
		ShowCursor()
	}()

	prompt := s.PromptString()
	line := ""
	selectedSuggestion := -1
	prevSuggestionCount := 0

	allSuggestions := func() []string {
		return getAllSuggestions(s, line, cmdNames)
	}

	renderInput := func() {
		suggestions := allSuggestions()
		if len(suggestions) > maxSuggestions {
			suggestions = suggestions[:maxSuggestions]
		}
		clearSuggestionLines(prevSuggestionCount)
		ClearLine()
		fmt.Print("\r" + prompt + line)
		if selectedSuggestion >= len(suggestions) {
			selectedSuggestion = len(suggestions) - 1
		}
		if len(line) > 0 && len(suggestions) > 0 {
			for i, sg := range suggestions {
				fmt.Print("\r\n")
				ClearLine()
				if i == selectedSuggestion {
					fmt.Print("  " + Reverse(" "+sg+" "))
				} else {
					fmt.Print("  " + Dim(sg))
				}
			}
			MoveUp(len(suggestions))
			MoveToColumn(len(stripAnsi(prompt)) + len(line) + 1)
		}
		prevSuggestionCount = len(suggestions)
		if len(line) == 0 {
			prevSuggestionCount = 0
		}
	}

	ShowCursor()
	renderInput()

	for {
		key, err := ReadKey()
		if err != nil {
			return "", err
		}

		switch {
		case key.Char == KeyCtrlC:
			line = ""
			selectedSuggestion = -1
			clearSuggestionLines(prevSuggestionCount)
			prevSuggestionCount = 0
			renderInput()

		case key.Char == KeyCtrlD:
			clearSuggestionLines(prevSuggestionCount)
			return "", io.EOF

		case key.Char == KeyEnter || key.Char == KeyNewline:
			suggestions := allSuggestions()
			if len(suggestions) > maxSuggestions {
				suggestions = suggestions[:maxSuggestions]
			}
			if selectedSuggestion >= 0 && selectedSuggestion < len(suggestions) {
				line = suggestions[selectedSuggestion]
				selectedSuggestion = -1
				clearSuggestionLines(prevSuggestionCount)
				prevSuggestionCount = 0
				renderInput()
				continue
			}
			clearSuggestionLines(prevSuggestionCount)
			ClearLine()
			fmt.Print("\r" + prompt + line + "\r\n")
			return line, nil

		case key.Char == '\t':
			suggestions := allSuggestions()
			if len(suggestions) > 0 {
				idx := selectedSuggestion
				if idx < 0 {
					idx = 0
				}
				if idx < len(suggestions) {
					line = suggestions[idx]
					selectedSuggestion = -1
				}
			}
			renderInput()

		case key.Special == KeyUp:
			suggestions := allSuggestions()
			if len(suggestions) > maxSuggestions {
				suggestions = suggestions[:maxSuggestions]
			}
			// If suggestions are visible, navigate them.
			if len(line) > 0 && len(suggestions) > 0 {
				selectedSuggestion--
				if selectedSuggestion < 0 {
					selectedSuggestion = len(suggestions) - 1
				}
				renderInput()
				continue
			}
			// Otherwise open the history selector.
			if len(history) > 0 {
				// Clear suggestions before entering raw select mode.
				clearSuggestionLines(prevSuggestionCount)
				prevSuggestionCount = 0
				ClearLine()
				fmt.Print("\r" + prompt + line + "\r\n")
				restoreErr := restore()
				restore = nil
				if restoreErr != nil {
					return "", restoreErr
				}
				chosen, historyErr := selectFromHistory()
				if historyErr != nil {
					return "", historyErr
				}
				restore, err = RawMode()
				if err != nil {
					return "", err
				}
				if chosen != "" {
					line = chosen
				}
				selectedSuggestion = -1
			}
			renderInput()

		case key.Special == KeyDown:
			suggestions := allSuggestions()
			if len(suggestions) > maxSuggestions {
				suggestions = suggestions[:maxSuggestions]
			}
			if len(line) > 0 && len(suggestions) > 0 {
				selectedSuggestion++
				if selectedSuggestion >= len(suggestions) {
					selectedSuggestion = 0
				}
				renderInput()
				continue
			}
			renderInput()

		case key.Char == KeyBackspace:
			if len(line) > 0 {
				line = line[:len(line)-1]
				selectedSuggestion = -1
			}
			renderInput()

		case key.Char >= 32 && key.Char < 127:
			line += string(key.Char)
			selectedSuggestion = -1
			renderInput()

		default:
			continue
		}
	}
}

func getAllSuggestions(s *Session, input string, cmdNames []string) []string {
	if input == "" {
		return nil
	}

	commandName, argPart, matched := matchAutocompleteCommand(input, cmdNames)
	if !matched {
		return getCommandSuggestions(input, cmdNames)
	}
	canonicalCommandName := commandName
	if target, ok := generatedCommandAliases[commandName]; ok {
		canonicalCommandName = target
	}
	if canonicalCommandName == "subnet attach-vpc" {
		if strings.TrimSpace(s.Scope.SiteID) == "" {
			return nil
		}
		tenantID, err := s.getTenantID(context.Background())
		if err != nil {
			return nil
		}
		items, err := s.subnetAttachSources(context.Background(), s.Scope.SiteID, tenantID)
		if err != nil {
			return nil
		}
		return resourceItemSuggestions(commandName, nil, items, argPart)
	}
	if info, ok := generatedAutocompleteInfo(commandName); ok && len(info.PathParameters) > 0 {
		return getGeneratedResourceSuggestions(s, info, argPart)
	}
	if resourceType, ok := argResourceMap[commandName]; ok {
		return getResourceSuggestions(s, commandName, resourceType, argPart)
	}
	return getCommandSuggestions(input, cmdNames)
}

func matchAutocompleteCommand(input string, cmdNames []string) (string, string, bool) {
	lowerInput := strings.ToLower(input)
	commandName := ""
	for _, name := range cmdNames {
		withSpace := strings.ToLower(name) + " "
		if strings.HasPrefix(lowerInput, withSpace) && len(name) > len(commandName) {
			commandName = name
		}
	}
	if commandName == "" {
		return "", "", false
	}
	return commandName, input[len(commandName)+1:], true
}

func generatedAutocompleteInfo(commandName string) (appcli.GeneratedCommandInfo, bool) {
	for _, info := range embeddedGeneratedCommandInfos {
		if info.Name == commandName {
			return info, true
		}
	}
	return appcli.GeneratedCommandInfo{}, false
}

func getGeneratedResourceSuggestions(
	s *Session,
	info appcli.GeneratedCommandInfo,
	argPart string,
) []string {
	prefixArgs, completedPaths, argFilter, ok := generatedAutocompleteArguments(info, argPart)
	if !ok || len(completedPaths) >= len(info.PathParameters) ||
		strings.HasPrefix(argFilter, "-") {
		return nil
	}

	ctx, cancel := context.WithTimeout(context.Background(), autocompleteFetchTimeout)
	defer cancel()

	resolvedValues := map[string]string{
		"siteId": strings.TrimSpace(s.Scope.SiteID),
		"vpcId":  strings.TrimSpace(s.Scope.VpcID),
	}
	for i, value := range completedPaths {
		parameter := info.PathParameters[i]
		descriptor := GeneratedPathResourceDescriptor(info.Name, parameter)
		items, supported, err := s.GeneratedResourceItems(
			ctx,
			descriptor,
			resolvedValues,
		)
		if err != nil {
			return nil
		}
		if !supported {
			resolvedValues[parameter] = value
			continue
		}
		item, found := matchGeneratedAutocompleteItem(items, value)
		if !found {
			return nil
		}
		resolvedValues[parameter] = item.ID
	}

	parameter := info.PathParameters[len(completedPaths)]
	descriptor := GeneratedPathResourceDescriptor(info.Name, parameter)
	items, supported, err := s.GeneratedResourceItems(
		ctx,
		descriptor,
		resolvedValues,
	)
	if err != nil || !supported {
		return nil
	}
	return resourceItemSuggestions(info.Name, prefixArgs, items, argFilter)
}

// generatedAutocompleteArguments separates generated command flags from
// positional path values. Generated CLI flags must precede positional values,
// so autocomplete needs to retain complete flags in the suggested command
// while resolving only the path values against resource fetchers.
func generatedAutocompleteArguments(
	info appcli.GeneratedCommandInfo,
	input string,
) (prefixArgs []string, pathArgs []string, filter string, ok bool) {
	args, err := splitCommandArguments(input)
	if err != nil {
		return nil, nil, "", false
	}
	completed := args
	if len(args) > 0 && !endsWithWhitespace(input) {
		filter = args[len(args)-1]
		completed = args[:len(args)-1]
	}

	flagTakesValue := make(map[string]bool, len(info.Flags))
	for _, flag := range info.Flags {
		flagTakesValue[flag.Name] = flag.TakesValue
	}

	positionalStarted := false
	for i := 0; i < len(completed); i++ {
		token := completed[i]
		if isGeneratedFlagToken(token) {
			if positionalStarted {
				return nil, nil, "", false
			}
			name, inline := generatedFlagName(token)
			takesValue, exists := flagTakesValue[name]
			if !exists {
				return nil, nil, "", false
			}
			prefixArgs = append(prefixArgs, token)
			if takesValue && !inline {
				if i+1 >= len(completed) {
					// The current partial token is this flag's value, not a
					// resource path filter.
					return nil, nil, "", false
				}
				i++
				prefixArgs = append(prefixArgs, completed[i])
			}
			continue
		}

		positionalStarted = true
		prefixArgs = append(prefixArgs, token)
		pathArgs = append(pathArgs, token)
	}
	return prefixArgs, pathArgs, filter, true
}

func endsWithWhitespace(input string) bool {
	if input == "" {
		return false
	}
	switch input[len(input)-1] {
	case ' ', '\t', '\r', '\n':
		return true
	default:
		return false
	}
}

func matchGeneratedAutocompleteItem(items []NamedItem, value string) (NamedItem, bool) {
	matches := matchingGeneratedResourceItems(items, value)
	if len(matches) == 1 {
		return matches[0], true
	}
	return NamedItem{}, false
}

func getResourceSuggestions(s *Session, cmdPrefix, resourceType, argFilter string) []string {
	items := s.Cache.Get(resourceType)
	if items == nil {
		fetched, err := s.Resolver.Fetch(context.Background(), resourceType)
		if err != nil {
			return nil
		}
		items = fetched
	}
	return resourceItemSuggestions(cmdPrefix, nil, items, argFilter)
}

func resourceItemSuggestions(
	cmdPrefix string,
	completed []string,
	items []NamedItem,
	argFilter string,
) []string {
	lowerFilter := strings.ToLower(strings.TrimSpace(argFilter))
	prefixParts := []string{cmdPrefix}
	for _, argument := range completed {
		prefixParts = append(prefixParts, quoteCommandArgument(argument))
	}
	prefix := strings.Join(prefixParts, " ") + " "

	var matches []string
	for _, item := range items {
		name := item.Name
		if name == "" {
			name = item.ID
		}
		if lowerFilter == "" ||
			strings.Contains(strings.ToLower(name), lowerFilter) ||
			strings.Contains(strings.ToLower(item.ID), lowerFilter) {
			matches = append(matches, prefix+quoteCommandArgument(name))
		}
	}
	return matches
}

func quoteCommandArgument(value string) string {
	if !strings.ContainsAny(value, " \t\r\n'\"\\") &&
		strings.IndexFunc(value, func(char rune) bool { return !strconv.IsPrint(char) }) == -1 {
		return value
	}
	return strconv.Quote(value)
}

func getCommandSuggestions(input string, cmdNames []string) []string {
	lower := strings.ToLower(input)
	var matches []string
	for _, name := range cmdNames {
		if strings.HasPrefix(strings.ToLower(name), lower) {
			matches = append(matches, name)
		}
	}
	return matches
}

func clearSuggestionLines(count int) {
	if count == 0 {
		return
	}
	for range count {
		fmt.Print("\r\n")
		ClearLine()
	}
	MoveUp(count)
}

func stripAnsi(s string) string {
	var result strings.Builder
	inEscape := false
	for i := 0; i < len(s); i++ {
		if s[i] == '\033' {
			inEscape = true
			continue
		}
		if inEscape {
			if (s[i] >= 'a' && s[i] <= 'z') || (s[i] >= 'A' && s[i] <= 'Z') {
				inEscape = false
			}
			continue
		}
		result.WriteByte(s[i])
	}
	return result.String()
}

func runScopeSet(s *Session, resourceType, nameOrID string) {
	s.Cache.Invalidate(resourceType)

	var item *NamedItem
	var err error
	if nameOrID != "" {
		items, fetchErr := s.Resolver.Fetch(context.Background(), resourceType)
		if fetchErr != nil {
			fmt.Fprintf(os.Stderr, "%s %v\n\n", Red("Error:"), fetchErr)
			return
		}
		lower := strings.ToLower(nameOrID)
		for _, it := range items {
			if strings.ToLower(it.Name) == lower || strings.ToLower(it.ID) == lower {
				itCopy := it
				item = &itCopy
				break
			}
		}
		if item == nil {
			fmt.Fprintf(os.Stderr, "%s no %s matching %q\n\n", Red("Error:"), resourceType, nameOrID)
			return
		}
	} else {
		item, err = s.Resolver.Resolve(context.Background(), resourceType, generatedParameterLabel(resourceType))
		if err != nil {
			fmt.Fprintf(os.Stderr, "%s %v\n\n", Red("Error:"), err)
			return
		}
	}

	switch resourceType {
	case "site":
		s.Scope.SiteID = item.ID
		s.Scope.SiteName = item.Name
		s.Scope.VpcID = ""
		s.Scope.VpcName = ""
		s.Cache.InvalidateFiltered()
	case "vpc":
		s.Scope.VpcID = item.ID
		s.Scope.VpcName = item.Name
		if siteID := item.Extra["siteId"]; siteID != "" && s.Scope.SiteID == "" {
			siteName := s.Resolver.ResolveID("site", siteID)
			s.Scope.SiteID = siteID
			s.Scope.SiteName = siteName
			fmt.Printf("Scope set: site = %s (from VPC)\n", Cyan(siteName))
		}
		s.Cache.InvalidateFiltered()
	}
	fmt.Printf("Scope set: %s = %s\n\n", resourceType, Cyan(item.Name))
}

func runOrgList(s *Session) {
	if s.Token == "" {
		fmt.Printf("Current org: %s\n", Cyan(s.Org))
		fmt.Printf("%s No token available. Run %s first.\n", Yellow("Note:"), Bold("login"))
		return
	}
	orgs := extractOrgsFromJWT(s.Token)
	if len(orgs) == 0 {
		fmt.Printf("Current org: %s\n", Cyan(s.Org))
		fmt.Printf("Could not extract orgs from token. Switch manually: %s\n", Bold("org set <org-name>"))
		return
	}
	fmt.Printf("Current org: %s\n\n", Cyan(s.Org))
	for _, org := range orgs {
		marker := "  "
		if org == s.Org {
			marker = Cyan("> ")
		}
		fmt.Printf("%s%s\n", marker, org)
	}
	fmt.Printf("\nSwitch with: %s\n", Bold("org set <org-name>"))
}

type jwtAccessClaim struct {
	Type string `json:"type"`
	Name string `json:"name"`
}

// selectFromHistory opens a windowed Select picker with the command history.
// Returns the chosen command, or empty string if cancelled.
func selectFromHistory() (string, error) {
	if len(history) == 0 {
		return "", nil
	}
	// Show most recent first.
	items := make([]SelectItem, len(history))
	for i, cmd := range history {
		items[len(history)-1-i] = SelectItem{Label: cmd, ID: cmd}
	}
	selected, err := Select("History", items)
	if err == errSelectionCancelled {
		return "", nil
	}
	if err != nil {
		return "", err
	}
	return selected.ID, nil
}

func extractOrgsFromJWT(tokenStr string) []string {
	parts := strings.Split(tokenStr, ".")
	if len(parts) != 3 {
		return nil
	}
	payload := parts[1]
	switch len(payload) % 4 {
	case 2:
		payload += "=="
	case 3:
		payload += "="
	}
	decoded, err := base64.URLEncoding.DecodeString(payload)
	if err != nil {
		return nil
	}
	var claims struct {
		Access []jwtAccessClaim `json:"access"`
	}
	if err := json.Unmarshal(decoded, &claims); err != nil {
		return nil
	}
	var orgs []string
	seen := map[string]bool{}
	for _, c := range claims.Access {
		if strings.HasPrefix(c.Type, "group/ngc") && c.Name != "" && !seen[c.Name] {
			orgs = append(orgs, c.Name)
			seen[c.Name] = true
		}
	}
	return orgs
}
