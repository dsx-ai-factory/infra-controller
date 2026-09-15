package main

import (
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/rs/zerolog/log"
)

const (
	LicenseHeader = `
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
`

	goPackageOption = `option go_package = "github.com/NVIDIA/infra-controller/rest-api/proto/core";`

	additionalExpectedMachineAttributes = `
// WARNING: Following fields are not present in Core, but added directly in REST snapshot
optional string name = 21;
optional string manufacturer = 22;
optional string model = 23;
optional string description = 24;
optional string firmware_version = 25;
optional int32 slot_id = 26;
optional int32 tray_idx = 27;
optional int32 host_id = 28;`

	additionalPowerShelfAttributes = `
// WARNING: Following fields are not present in Core, but added directly in REST snapshot
optional string name = 21;
optional string manufacturer = 22;
optional string model = 23;
optional string description = 24;
optional string firmware_version = 25;
optional int32 slot_id = 26;
optional int32 tray_idx = 27;
optional int32 host_id = 28;`

	additionalExpectedSwitchAttributes = `
// WARNING: Following fields are not present in Core, but added directly in REST snapshot
optional string name = 21;
optional string manufacturer = 22;
optional string model = 23;
optional string description = 24;
optional string firmware_version = 25;
optional int32 slot_id = 26;
optional int32 tray_idx = 27;
optional int32 host_id = 28;`
)

func normalizeProtoFile(protoFile string) {
	protoFileContent, err := os.ReadFile(protoFile)
	if err != nil {
		log.Err(err).Str("protoFile", protoFile).Msg("Failed to read proto file")
		return
	}

	log.Info().Str("ProtoFile", protoFile).Int("ContentLength", len(protoFileContent)).Msg("Normalizing proto file")

	content := string(protoFileContent)
	content = addOrReplaceLicenseHeader(content)
	content = removeCodegenAnnotations(content)
	content = addGoPackageOption(content)
	content = updateImports(content)
	content = renameCarbideReference(content)

	baseName := filepath.Base(protoFile)
	switch baseName {
	case "site_explorer_nico.proto":
		content = normalizeSiteExplorer(content)
	case "dns_nico.proto":
		content = normalizeDns(content)
	case "nico_nico.proto":
		content = normalizeNICo(content)
	}

	content = trimWhitespace(content)

	if err := os.WriteFile(protoFile, []byte(content), 0644); err != nil {
		log.Err(err).Str("protoFile", protoFile).Msg("Failed to write normalized proto file")
	}
}

// removeCodegenAnnotations removes Rust-only code generation metadata before
// the Core protobuf snapshot is passed to Buf and the Go protobuf compiler.
func removeCodegenAnnotations(content string) string {
	codegenImport := regexp.MustCompile(`(?m)^[ \t]*import "codegen/v1/(?:derive|extern_path|rust_type)\.proto";[ \t]*\n(?:[ \t]*\n)?`)
	content = codegenImport.ReplaceAllString(content, "")
	fileOption := regexp.MustCompile(`(?ms)^[ \t]*option \(carbide\.codegen\.v1\.imported_extern_path\) = \{.*?^[ \t]*\};[ \t]*\n(?:[ \t]*\n)?`)
	content = fileOption.ReplaceAllString(content, "")
	declarationOption := regexp.MustCompile(`(?m)^[ \t]*option \(carbide\.codegen\.v1\.(?:message|enum)_(?:derive|extern_path)\) = "[^"]+";[ \t]*\n?`)
	content = declarationOption.ReplaceAllString(content, "")
	methodOverride := regexp.MustCompile(`(?ms)(^[ \t]*rpc\b[^{;]*?\breturns[ \t]*\([^)]*\))[ \t]*\{\n(?:[ \t]*option \(carbide\.codegen\.v1\.method_rust_(?:input|output)_type\) = "[^"]+";[ \t]*\n)+[ \t]*\}`)
	content = methodOverride.ReplaceAllString(content, "${1};")
	fieldOverrideWithOtherOptions := regexp.MustCompile(`,[ \t]*\(carbide\.codegen\.v1\.field_rust_type\)[ \t]*=[ \t]*"[^"]+"`)
	content = fieldOverrideWithOtherOptions.ReplaceAllString(content, "")
	fieldOverride := regexp.MustCompile(`[ \t]*\[[ \t]*\(carbide\.codegen\.v1\.field_rust_type\)[ \t]*=[ \t]*"[^"]+"[ \t]*\]`)
	return fieldOverride.ReplaceAllString(content, "")
}

// addOrReplaceLicenseHeader strips any existing comment/blank-line preamble
// before the first proto directive (e.g. `syntax`) and prepends LicenseHeader.
// Handles both // line comments and /* ... */ block comments (asterisk-formatted).
func addOrReplaceLicenseHeader(content string) string {
	lines := strings.Split(content, "\n")
	idx := 0
	inBlock := false
	for idx < len(lines) {
		trimmed := strings.TrimSpace(lines[idx])
		switch {
		case inBlock:
			if strings.Contains(trimmed, "*/") {
				inBlock = false
			}
			idx++
		case trimmed == "" || strings.HasPrefix(trimmed, "//"):
			idx++
		case strings.HasPrefix(trimmed, "/*"):
			inBlock = true
			if strings.Contains(trimmed, "*/") {
				inBlock = false
			}
			idx++
		default:
			goto done
		}
	}
done:
	return strings.TrimSpace(LicenseHeader) + "\n\n" + strings.Join(lines[idx:], "\n")
}

func addGoPackageOption(content string) string {
	if strings.Contains(content, "go_package") {
		return content
	}
	// Insert after the last import line, or after the package line if there are no imports.
	lastImport := regexp.MustCompile(`(?m)(^import "[^"]+";)`)
	matches := lastImport.FindAllStringIndex(content, -1)
	if len(matches) > 0 {
		pos := matches[len(matches)-1][1]
		return content[:pos] + "\n\n" + goPackageOption + content[pos:]
	}
	re := regexp.MustCompile(`(?m)(^package\s+\w+;)`)
	return re.ReplaceAllString(content, "${1}\n\n"+goPackageOption)
}

// updateImports rewrites local proto imports (those without a path separator)
// to use the _nico.proto suffix, leaving google/protobuf imports untouched.
func updateImports(content string) string {
	re := regexp.MustCompile(`import "([^"]+)\.proto"`)
	return re.ReplaceAllStringFunc(content, func(match string) string {
		if strings.Contains(match, "google/") || strings.Contains(match, "_nico.proto") {
			return match
		}
		return strings.Replace(match, `.proto"`, `_nico.proto"`, 1)
	})
}

// replaceOutsideComments applies re.ReplaceAllString(line, repl) only on
// lines that are not proto comments (i.e. lines not starting with "//").
func replaceOutsideComments(content string, re *regexp.Regexp, repl string) string {
	lines := strings.Split(content, "\n")
	for i, line := range lines {
		if !strings.HasPrefix(strings.TrimSpace(line), "//") {
			lines[i] = re.ReplaceAllString(line, repl)
		}
	}
	return strings.Join(lines, "\n")
}

// replaceInComments applies re.ReplaceAllString only to comment segments:
// - line comments introduced by //
// - block comments wrapped in /* ... */
// This includes inline comments on non-comment lines.
func replaceInComments(content string, re *regexp.Regexp, repl string) string {
	lines := strings.Split(content, "\n")
	inBlockComment := false
	for i, line := range lines {
		lines[i], inBlockComment = replaceInCommentSegments(line, inBlockComment, re, repl)
	}
	return strings.Join(lines, "\n")
}

func replaceInCommentSegments(line string, inBlockComment bool, re *regexp.Regexp, repl string) (string, bool) {
	var out strings.Builder
	cursor := 0

	for cursor < len(line) {
		if inBlockComment {
			blockEndRel := strings.Index(line[cursor:], "*/")
			if blockEndRel == -1 {
				out.WriteString(re.ReplaceAllString(line[cursor:], repl))
				return out.String(), true
			}

			blockEnd := cursor + blockEndRel + len("*/")
			out.WriteString(re.ReplaceAllString(line[cursor:blockEnd], repl))
			cursor = blockEnd
			inBlockComment = false
			continue
		}

		lineCommentRel := strings.Index(line[cursor:], "//")
		blockStartRel := strings.Index(line[cursor:], "/*")

		switch {
		case lineCommentRel == -1 && blockStartRel == -1:
			out.WriteString(line[cursor:])
			cursor = len(line)

		case lineCommentRel != -1 && (blockStartRel == -1 || lineCommentRel < blockStartRel):
			lineCommentStart := cursor + lineCommentRel
			out.WriteString(line[cursor:lineCommentStart])
			out.WriteString(re.ReplaceAllString(line[lineCommentStart:], repl))
			cursor = len(line)

		default:
			blockStart := cursor + blockStartRel
			out.WriteString(line[cursor:blockStart])

			blockEndRel := strings.Index(line[blockStart+len("/*"):], "*/")
			if blockEndRel == -1 {
				out.WriteString(re.ReplaceAllString(line[blockStart:], repl))
				return out.String(), true
			}

			blockEnd := blockStart + len("/*") + blockEndRel + len("*/")
			out.WriteString(re.ReplaceAllString(line[blockStart:blockEnd], repl))
			cursor = blockEnd
		}
	}

	return out.String(), inBlockComment
}

// hasWarningBefore reports whether a "// WARNING:" comment already exists on
// the line immediately preceding the first occurrence of target. This is more
// robust than checking for a specific warning string because rename regexes
// may alter words inside existing warning comments.
func hasWarningBefore(content, target string) bool {
	idx := strings.Index(content, target)
	if idx <= 0 {
		return false
	}
	before := strings.TrimRight(content[:idx], " \t\n")
	if nl := strings.LastIndex(before, "\n"); nl >= 0 {
		before = before[nl+1:]
	}
	return strings.HasPrefix(strings.TrimSpace(before), "// WARNING:")
}

func normalizeSiteExplorer(content string) string {
	re := regexp.MustCompile(`\bPowerState\b`)
	content = replaceOutsideComments(content, re, "ComputerSystemPowerState")

	warning := "// WARNING: This enum conflicts with PowerState in nico_nico.proto and must be renamed to ComputerSystemPowerState\n"
	target := "enum ComputerSystemPowerState {"
	if !hasWarningBefore(content, target) {
		content = strings.Replace(content, target, warning+target, 1)
	}

	return content
}

func normalizeDns(content string) string {
	re := regexp.MustCompile(`\bMetadata\b`)
	content = replaceOutsideComments(content, re, "DomainMetadata")

	warning := "// WARNING: This type conflicts with Metadata in nico_nico.proto and must be renamed to DomainMetadata\n"
	target := "message DomainMetadata {"
	if !hasWarningBefore(content, target) {
		content = strings.Replace(content, target, warning+target, 1)
	}

	return content
}

func normalizeNICo(content string) string {
	content = nicoRenameMachineInventory(content)
	content = nicoUpdateInterfaceFunctionType(content)
	content = nicoMoveValidationEnums(content)
	content = nicoRemoveDomainTypes(content)
	content = nicoUpdatePxeDomain(content)
	content = nicoPreserveVpcNameFields(content)
	content = nicoExpandExpectedObject(content, "ExpectedPowerShelf", additionalPowerShelfAttributes)
	content = nicoExpandExpectedObject(content, "ExpectedSwitch", additionalExpectedSwitchAttributes)
	content = nicoExpandExpectedObject(content, "ExpectedMachine", additionalExpectedMachineAttributes)
	return content
}

func nicoRenameMachineInventory(content string) string {
	re := regexp.MustCompile(`\bMachineInventory\b`)
	content = replaceOutsideComments(content, re, "MachineComponentInventory")

	warning := "// WARNING: This type conflicts with MachineInventory in inventory.proto and must be renamed to MachineComponentInventory\n"
	target := "message MachineComponentInventory {"
	if !hasWarningBefore(content, target) {
		content = strings.Replace(content, target, warning+target, 1)
	}

	return content
}

func nicoUpdateInterfaceFunctionType(content string) string {
	warning := "// WARNING: This enum was changed in a non-backwards compatible way in nico_nico.proto to drop _FUNCTION suffix\n"
	target := "enum InterfaceFunctionType {"
	if !hasWarningBefore(content, target) {
		content = strings.Replace(content, target, warning+target, 1)
	}
	content = strings.Replace(content, "  PHYSICAL = 0;", "  PHYSICAL_FUNCTION = 0;", 1)
	content = strings.Replace(content, "  VIRTUAL = 1;", "  VIRTUAL_FUNCTION = 1;", 1)
	return content
}

// nicoMoveValidationEnums extracts the three enums nested inside
// MachineValidationStatus and places them at the top level immediately
// before the message so proto3 can compile them.
func nicoMoveValidationEnums(content string) string {
	warning := "// WARNING: Core proto declares these enums inside `MachineValidationStatus`. This is not compilable to protobuf so we move the enums to the top level"

	enumNames := []string{"MachineValidationStarted", "MachineValidationInProgress", "MachineValidationCompleted"}
	var extractedEnums strings.Builder

	for _, name := range enumNames {
		re := regexp.MustCompile(`\n {2,}enum\s+` + name + `\s*\{[^}]*\}`)
		match := re.FindString(content)
		if match != "" {
			content = strings.Replace(content, match, "", 1)
			extractedEnums.WriteString(warning + "\n")
			extractedEnums.WriteString(dedent(match) + "\n\n")
		}
	}

	content = strings.Replace(content, "message MachineValidationStatus {", extractedEnums.String()+"message MachineValidationStatus {", 1)

	return content
}

// trimWhitespace removes trailing whitespace from every line and ensures the
// file ends with exactly one newline.
func trimWhitespace(content string) string {
	lines := strings.Split(content, "\n")
	for i, line := range lines {
		lines[i] = strings.TrimRight(line, " \t")
	}
	return strings.TrimRight(strings.Join(lines, "\n"), "\n") + "\n"
}

// dedent strips the leading/trailing whitespace from s and removes one level
// of 2-space indentation from each line (the nesting from the parent message).
func dedent(s string) string {
	lines := strings.Split(strings.TrimSpace(s), "\n")
	for i, line := range lines {
		lines[i] = strings.TrimPrefix(line, "  ")
	}
	return strings.Join(lines, "\n")
}

func nicoRemoveDomainTypes(content string) string {
	typesToRemove := []string{"DomainSearchQuery", "DomainDeletionResult", "DomainDeletion", "DomainList", "Domain"}

	for _, typeName := range typesToRemove {
		re := regexp.MustCompile(`(?m)^message ` + typeName + `\s*\{[^}]*\}\n*`)
		content = re.ReplaceAllString(content, "")
	}

	return content
}

func nicoUpdatePxeDomain(content string) string {
	warning := "    // WARNING: Updated to correct legacy type\n"
	content = strings.Replace(content, "    Domain legacy_domain = 2;", warning+"    DomainLegacy legacy_domain = 2;", 1)
	return content
}

func nicoPreserveVpcNameFields(content string) string {
	comment := "Removed in v0.9.x and above, retained here for backwards compatibility"
	content = nicoPreserveField(content, "Vpc", "reserved 2;", "string name = 2; // "+comment)
	content = nicoPreserveField(content, "VpcCreationRequest", "reserved 2;", "string name = 2; // "+comment)
	content = nicoPreserveField(content, "VpcUpdateRequest", "reserved 3;", "string name = 3; // "+comment)
	content = nicoPreserveField(content, "VpcPrefix", "reserved 3;", "string name = 3; // "+comment)
	content = nicoPreserveField(content, "VpcPrefixCreationRequest", "reserved 3;", "string name = 3; // "+comment)
	content = nicoPreserveField(content, "VpcPrefixUpdateRequest", "reserved 3;", "optional string name = 3; // "+comment)
	return content
}

func nicoPreserveField(content string, messageName string, reservedField string, legacyField string) string {
	re := regexp.MustCompile(`message ` + messageName + ` \{[^}]*\}`)
	loc := re.FindStringIndex(content)
	if loc == nil {
		return content
	}

	block := content[loc[0]:loc[1]]
	if strings.Contains(block, legacyField) {
		return content
	}

	reservedRe := regexp.MustCompile(`(?m)^(\s*)` + regexp.QuoteMeta(reservedField) + `.*$`)
	block = reservedRe.ReplaceAllString(block, "${1}"+legacyField)

	return content[:loc[0]] + block + content[loc[1]:]
}

func nicoExpandExpectedObject(content string, objectType string, additionalAttributes string) string {
	re := regexp.MustCompile(`message ` + objectType + ` \{[^}]*\}`)
	loc := re.FindStringIndex(content)
	if loc == nil {
		return content
	}

	block := content[loc[0]:loc[1]]
	if strings.Contains(block, "WARNING: Following fields are not present in Core") {
		return content
	}
	block = strings.TrimSuffix(block, "}") + "\n" + indentBlock(additionalAttributes) + "}"

	return content[:loc[0]] + block + content[loc[1]:]
}

// renameCarbideReference renames forge/carbide comments:
func renameCarbideReference(content string) string {
	re := regexp.MustCompile(`\bCarbide\b`)
	content = replaceInComments(content, re, "NICo")

	re = regexp.MustCompile(`\bForge\b`)
	content = replaceInComments(content, re, "NICo")

	re = regexp.MustCompile(`\bcarbide-api\b`)
	content = replaceInComments(content, re, "nico-core-api")

	re = regexp.MustCompile(`\bforge-\b`)
	content = replaceInComments(content, re, "nico-")

	re = regexp.MustCompile(`\bcarbide-\b`)
	content = replaceInComments(content, re, "nico-")

	re = regexp.MustCompile(`\bcarbide\b`)
	content = replaceInComments(content, re, "nico")

	re = regexp.MustCompile(`\bforge\b`)
	content = replaceInComments(content, re, "nico")

	return content
}

// indentBlock trims s, prefixes each line with 2 spaces, and returns the
// result with a trailing newline.
func indentBlock(s string) string {
	lines := strings.Split(strings.TrimSpace(s), "\n")
	for i, line := range lines {
		lines[i] = "  " + line
	}
	return strings.Join(lines, "\n") + "\n"
}

func main() {
	workflowsDir := filepath.Join("..", "..", "core", "src", "v1")
	nicoProtoFiles := filepath.Join(workflowsDir, "*_nico.proto")
	protoFiles, err := filepath.Glob(nicoProtoFiles)
	if err != nil {
		log.Panic().Err(err).Msg("Failed to get list of nico proto files")
	}
	for _, protoFile := range protoFiles {
		normalizeProtoFile(protoFile)
	}
}
