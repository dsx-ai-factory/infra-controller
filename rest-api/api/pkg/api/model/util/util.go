// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"errors"
	"fmt"
	"slices"
	"strings"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/types/known/fieldmaskpb"
	"gopkg.in/yaml.v3"
)

const (
	// configuration for phone home
	SitePhoneHomeName      = "phone_home"
	SitePhoneHomePost      = "post"
	SitePhoneHomePostAll   = "all"
	SitePhoneHomeUrl       = "url"
	SiteCloudConfig        = "#cloud-config"
	SiteCloudConfigArchive = "#cloud-config-archive"
	autoinstallName        = "autoinstall"
	autoinstallUserData    = "user-data"
	archiveEntryType       = "type"
	archiveEntryContent    = "content"
	mergeKey               = "<<"
	archiveContentType     = "text/cloud-config"

	// jinjaTemplateHeader marks user-data as a Jinja template. cloud-init reads
	// the format from the line below it, so templated user-data opens with a
	// two-line header:
	//
	//	## template: jinja
	//	#cloud-config
	//
	// See https://docs.cloud-init.io/en/25.1/explanation/format.html#jinja-template
	jinjaTemplateHeader = "## template: jinja"

	// userDataIndentSpaces matches the indent cloud-config is conventionally
	// written with. The encoder's default of 4 re-indents every nested line,
	// which grew realistic cloud-config documents by 9% to 31% and pushed
	// requests that were under MaxUserDataBytes past it. A sequence written
	// flush against its parent key still grows, because the encoder always
	// indents a sequence under the key that owns it.
	userDataIndentSpaces = 2
)

// MarshalUserData renders a cloud-init document back to YAML. Use it rather
// than yaml.Marshal, whose fixed 4-space indent inflates the stored value.
func MarshalUserData(document *yaml.Node) ([]byte, error) {
	var out bytes.Buffer

	encoder := yaml.NewEncoder(&out)
	encoder.SetIndent(userDataIndentSpaces)

	err := encoder.Encode(document)
	if err != nil {
		return nil, err
	}

	err = encoder.Close()
	if err != nil {
		return nil, err
	}

	return out.Bytes(), nil
}

// ErrUnsupportedUserData reports user-data phone-home cannot live in: not a
// #cloud-config mapping or a #cloud-config-archive sequence, but a script, a
// template cloud-init ignores, or text that is not YAML. Enabling phone-home
// rejects it; disabling leaves it alone, since the block cannot be in there.
var ErrUnsupportedUserData = errors.New("userData is not a #cloud-config or #cloud-config-archive document")

// EnablePhoneHomeInUserData returns userData with a phone-home block reporting
// to url, replacing any block already in there. Nil or empty user-data yields a
// #cloud-config document holding just the block.
func EnablePhoneHomeInUserData(userData *string, url string) (*string, error) {
	header, documentRoot, err := parseUserData(userData)
	if err != nil {
		return nil, err
	}
	if header == "" {
		// cloud-init needs a header, and user-data written without one is
		// #cloud-config.
		header = SiteCloudConfig + "\n"
	}

	// cloud-init merges archive parts independently, so phone-home is delivered
	// as a part of its own rather than folded into somebody else's.
	if documentRoot.Kind == yaml.SequenceNode {
		if _, err := removePhoneHomeParts(documentRoot, nil); err != nil {
			return nil, err
		}
		err = appendPhoneHomePart(documentRoot, url)
	} else {
		err = insertPhoneHome(documentRoot, url)
	}
	if err != nil {
		return nil, err
	}

	inlineDanglingAliases(documentRoot)

	return renderUserData(header, documentRoot)
}

// DisablePhoneHomeInUserData returns userData without the phone-home blocks
// reporting to url. Blocks reporting elsewhere are somebody else's: user-data
// with none of ours is handed back exactly as authored. Nil is returned when
// there is nothing to store: user-data that was empty to begin with.
func DisablePhoneHomeInUserData(userData *string, url string) (*string, error) {
	return disablePhoneHome(userData, &url)
}

// DisableAllPhoneHomeInUserData is DisablePhoneHomeInUserData for every
// phone-home block, whatever it reports to. It is for user-data we authored the
// block in: the URL frozen into it may predate a change to the configured one.
func DisableAllPhoneHomeInUserData(userData *string) (*string, error) {
	return disablePhoneHome(userData, nil)
}

// disablePhoneHome is DisablePhoneHomeInUserData; a nil url matches every block.
func disablePhoneHome(userData *string, url *string) (*string, error) {
	if userData == nil || *userData == "" {
		return nil, nil
	}

	header, documentRoot, err := parseUserData(userData)
	if err != nil {
		return nil, err
	}

	removed := false
	if documentRoot.Kind == yaml.SequenceNode {
		if removed, err = removePhoneHomeParts(documentRoot, url); err != nil {
			return nil, err
		}
	} else {
		removed = removePhoneHome(documentRoot, url)
	}

	switch {
	case !removed:
		return userData, nil
	case documentRoot.Kind == yaml.MappingNode && len(documentRoot.Content) == 0:
		// Nothing but phone-home was in there, so no user-data is left. An
		// emptied archive renders instead, keeping its header.
		return new(""), nil
	}

	inlineDanglingAliases(documentRoot)

	return renderUserData(header, documentRoot)
}

// parseUserData splits the cloud-init header off user-data and parses the YAML
// body below it. Only a #cloud-config mapping or a #cloud-config-archive
// sequence can carry phone-home; user-data with no header is #cloud-config, and
// no user-data at all is an empty one.
func parseUserData(userData *string) (string, *yaml.Node, error) {
	if userData == nil || *userData == "" {
		return "", &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}, nil
	}

	header, body := splitUserDataHeader(*userData)

	document := &yaml.Node{}
	if err := yaml.Unmarshal([]byte(body), document); err != nil || len(document.Content) == 0 {
		return "", nil, ErrUnsupportedUserData
	}

	documentRoot := document.Content[0]

	switch format := headerFormat(header); {
	case declaresFormat(header, jinjaTemplateHeader) && format == SiteCloudConfigArchive:
		// cloud-init's jinja handler dispatches cloud-config, scripts and
		// boothooks only, so a jinja archive is never run.
		return "", nil, ErrUnsupportedUserData
	case documentRoot.Kind == yaml.SequenceNode && format == SiteCloudConfigArchive:
	case documentRoot.Kind == yaml.MappingNode && (format == "" || format == SiteCloudConfig):
	default:
		return "", nil, ErrUnsupportedUserData
	}

	return header, documentRoot, nil
}

// renderUserData renders a document below its header. The header is text, the
// way cloud-config is written from scratch, so no edit to the document can lose
// it - not even removing the first key or part, which is where yaml would
// otherwise keep it.
func renderUserData(header string, documentRoot *yaml.Node) (*string, error) {
	rendered, err := MarshalUserData(documentRoot)
	if err != nil {
		return nil, fmt.Errorf("failed to render userData: %w", err)
	}

	// An edit can still strip an anchor an alias elsewhere points at, and yaml
	// cannot read that back. Not ErrUnsupportedUserData: the block was in there,
	// so failing beats reporting phone-home disabled over user-data holding it.
	if err := yaml.Unmarshal(rendered, &yaml.Node{}); err != nil {
		return nil, fmt.Errorf("rendered userData is not valid yaml: %w", err)
	}

	return new(header + string(rendered)), nil
}

// splitUserDataHeader splits user-data into its cloud-init header, newline
// included, and the YAML body below it. The header is the leading comment line,
// plus the line after it when that leading line is the jinja marker.
func splitUserDataHeader(userData string) (header, body string) {
	line, body, found := strings.Cut(userData, "\n")
	if !found || !strings.HasPrefix(line, "#") {
		return "", userData
	}
	header = line + "\n"

	if !declaresFormat(line, jinjaTemplateHeader) {
		return header, body
	}

	line, rest, found := strings.Cut(body, "\n")
	if !found || !strings.HasPrefix(line, "#") {
		return header, body
	}

	return header + line + "\n", rest
}

// headerFormat returns the format a cloud-init header declares, as the marker
// naming it, or "" when there is no header. The format is on the header's last
// line, since a jinja template declares it below the template marker.
func headerFormat(header string) string {
	line := strings.TrimSpace(header)
	if _, lastLine, found := strings.Cut(line, "\n"); found {
		line = strings.TrimSpace(lastLine)
	}

	// The longer marker is tried first because #cloud-config prefixes
	// #cloud-config-archive.
	for _, marker := range []string{SiteCloudConfigArchive, SiteCloudConfig} {
		if declaresFormat(line, marker) {
			return marker
		}
	}

	// Some other format, e.g. a #!/bin/sh script - or no header at all.
	return line
}

// declaresFormat reports whether a header line declares marker, matched the way
// cloud-init matches it: on the start of the line, ignoring case, so a marker
// with a note after it still names the format.
func declaresFormat(line string, marker string) bool {
	return strings.HasPrefix(strings.ToLower(strings.TrimSpace(line)), marker)
}

// insertPhoneHome adds a phone-home block reporting to url to a #cloud-config
// document: at the root, or under autoinstall.user-data when the document
// installs a target system, since that is the config the installed system runs.
// Existing blocks are removed first, so re-enabling is idempotent.
func insertPhoneHome(documentRoot *yaml.Node, url string) error {
	insertionNode := documentRoot

	if autoinstallNode := mappingValue(documentRoot, autoinstallName); autoinstallNode != nil {
		if autoinstallNode.Kind != yaml.MappingNode {
			return errors.New("autoinstall must be a mapping to insert phone-home")
		}

		insertionNode = mappingValue(autoinstallNode, autoinstallUserData)
		switch {
		case insertionNode == nil:
			insertionNode = &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
			autoinstallNode.Content = append(autoinstallNode.Content,
				scalarNode(autoinstallUserData), insertionNode)
		case insertionNode.Kind != yaml.MappingNode:
			return errors.New("autoinstall user-data must be a mapping to insert phone-home")
		}
	}

	removePhoneHome(documentRoot, nil)

	phoneHomeValueNode := &yaml.Node{}
	if err := phoneHomeValueNode.Encode(map[string]string{
		SitePhoneHomeUrl:  url,
		SitePhoneHomePost: SitePhoneHomePostAll,
	}); err != nil {
		return fmt.Errorf("failed to encode phone-home block: %w", err)
	}

	insertionNode.Content = append(insertionNode.Content,
		scalarNode(SitePhoneHomeName), phoneHomeValueNode)

	return nil
}

// removePhoneHome deletes the phone-home blocks reporting to url - every block
// when url is nil - from a #cloud-config root and from autoinstall.user-data,
// reporting whether it deleted any.
func removePhoneHome(documentRoot *yaml.Node, url *string) bool {
	removed := removePhoneHomeFromMapping(documentRoot, url)

	autoinstallNode := mappingValue(documentRoot, autoinstallName)
	if autoinstallNode == nil || autoinstallNode.Kind != yaml.MappingNode {
		return removed
	}

	targetUserDataNode := mappingValue(autoinstallNode, autoinstallUserData)
	if targetUserDataNode != nil && targetUserDataNode.Kind == yaml.MappingNode {
		removed = removePhoneHomeFromMapping(targetUserDataNode, url) || removed
	}

	return removed
}

// removePhoneHomeFromMapping deletes the phone_home keys of one mapping. Valid
// but nonsensical user-data can repeat the key, so the whole mapping is scanned.
func removePhoneHomeFromMapping(mappingNode *yaml.Node, url *string) bool {
	removed := false

	// Mapping content is a flat run of key/value pairs, so a key is dropped by
	// snipping two entries; the pairs that stay keep their authored order.
	for i := 0; i+1 < len(mappingNode.Content); {
		keyNode := resolveAlias(mappingNode.Content[i])
		valueNode := resolveAlias(mappingNode.Content[i+1])
		if keyNode.Value != SitePhoneHomeName || valueNode.Kind != yaml.MappingNode ||
			!phoneHomeReportsTo(valueNode, url) {
			i += 2
			continue
		}

		mappingNode.Content = append(mappingNode.Content[:i], mappingNode.Content[i+2:]...)
		removed = true
	}

	return removed
}

// phoneHomeReportsTo reports whether a phone-home block reports to url. A nil
// url matches any block.
func phoneHomeReportsTo(phoneHomeNode *yaml.Node, url *string) bool {
	if url == nil {
		return true
	}

	urlNode := mappingValue(phoneHomeNode, SitePhoneHomeUrl)

	return urlNode != nil && urlNode.Value == *url
}

// removePhoneHomeParts strips phone-home from every part of an archive, dropping
// parts left empty and reporting whether it changed anything. A part is
// user-data in its own right, so each goes through the same path as a whole
// document.
func removePhoneHomeParts(archiveRoot *yaml.Node, url *string) (bool, error) {
	removed := false
	kept := archiveRoot.Content[:0]
	var dropped []*yaml.Node

	for _, part := range archiveRoot.Content {
		content := cloudConfigArchiveContent(part)
		if content == nil {
			kept = append(kept, part)
			continue
		}

		stripped, err := disablePhoneHome(&content.Value, url)
		switch {
		case errors.Is(err, ErrUnsupportedUserData):
			// Not a part phone-home can live in: leave it exactly as authored.
			kept = append(kept, part)
		case err != nil:
			// Read before the cases below, since a part that failed carries no
			// content to compare - which would otherwise keep it, and report the
			// archive stripped over a part this could not read.
			return false, err
		case stripped == nil, *stripped == content.Value:
			// Nothing to strip from it: leave it exactly as authored.
			kept = append(kept, part)
		case *stripped == "":
			// The part held nothing but phone-home, so drop it entirely.
			dropped = append(dropped, part)
			removed = true
		default:
			kept = append(kept, partWithContent(part, *stripped))
			removed = true
		}
	}
	archiveRoot.Content = kept

	// A part merging a dropped one still reads it, so what it held is emptied -
	// after the loop, since the parts after it read it to strip themselves.
	for _, part := range dropped {
		setPartContent(resolveAlias(part), "")
	}

	return removed, nil
}

// partWithContent returns the part to keep in place of one whose content was
// stripped. It is rewritten where it stands, so a part aliasing or merging it
// reads the rewrite - unless it is a scalar, which is its own content.
func partWithContent(part *yaml.Node, text string) *yaml.Node {
	authored := resolveAlias(part)
	if authored.Kind == yaml.ScalarNode {
		return rewrittenContent(authored, text)
	}

	setPartContent(authored, text)

	return part
}

// setPartContent points a part's content key - the one mappingValue reads it
// from - at a node holding text. A part with no content key holds none.
func setPartContent(part *yaml.Node, text string) {
	if index := mappingIndex(part, archiveEntryContent); index >= 0 {
		part.Content[index] = rewrittenContent(part.Content[index], text)
	}
}

// rewrittenContent returns a copy of the content node as authored, carrying text
// and no anchor of its own: a part of another type can alias that node, and
// keeps what it was authored with.
func rewrittenContent(authored *yaml.Node, text string) *yaml.Node {
	content := *authored
	content.Anchor, content.Alias = "", nil
	content.SetString(text)

	return &content
}

// appendPhoneHomePart appends the archive part that carries phone-home. Its
// content is built by the #cloud-config path, so both formats share one source
// of truth for the block.
func appendPhoneHomePart(archiveRoot *yaml.Node, url string) error {
	content := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
	if err := insertPhoneHome(content, url); err != nil {
		return err
	}

	rendered, err := renderUserData(SiteCloudConfig+"\n", content)
	if err != nil {
		return err
	}

	contentNode := scalarNode(*rendered)

	archiveRoot.Content = append(archiveRoot.Content, &yaml.Node{
		Kind: yaml.MappingNode,
		Tag:  "!!map",
		Content: []*yaml.Node{
			scalarNode(archiveEntryType), scalarNode(archiveContentType),
			scalarNode(archiveEntryContent), contentNode,
		},
	})

	return nil
}

// cloudConfigArchiveContent returns the content scalar of a cloud-config archive
// part, or nil for a part that is not cloud-config (a shell script, say) or
// carries no string content. A part with no explicit type is cloud-config,
// matching cloud-init's default.
func cloudConfigArchiveContent(part *yaml.Node) *yaml.Node {
	part = resolveAlias(part)
	if part == nil {
		return nil
	}

	// cloud-init reads a scalar part as the content of a part with no type.
	if part.Kind == yaml.ScalarNode {
		return part
	}

	if part.Kind != yaml.MappingNode {
		return nil
	}

	// cloud-init resolves `<<` at load, so what a part that merges another ends
	// up holding - its type included - cannot be told from here.
	if mappingValue(part, mergeKey) != nil {
		return nil
	}

	// A structured type is malformed, not an omitted one, so the part is left
	// alone rather than read as cloud-config.
	if typeNode := mappingValue(part, archiveEntryType); typeNode != nil &&
		(typeNode.Kind != yaml.ScalarNode ||
			(typeNode.Value != "" && typeNode.Value != archiveContentType)) {
		return nil
	}

	contentNode := mappingValue(part, archiveEntryContent)
	if contentNode == nil || contentNode.Kind != yaml.ScalarNode {
		return nil
	}

	return contentNode
}

// inlineDanglingAliases puts the values whose anchor a removal took away back in
// the document, at the first alias pointing at each. An alias with no anchor
// cannot be read back at all, so leaving one behind would break the whole
// document. Aliases whose anchor is still in there are left exactly as authored.
func inlineDanglingAliases(documentRoot *yaml.Node) {
	inDocument := map[*yaml.Node]bool{}
	collectNodes(documentRoot, inDocument)
	inlineAliasesTo(documentRoot, inDocument, map[*yaml.Node]bool{})
}

// collectNodes records every node the document still holds. Aliases are not
// followed, so a value only a removed key or part pointed at stays out.
func collectNodes(node *yaml.Node, inDocument map[*yaml.Node]bool) {
	if node == nil || inDocument[node] {
		return
	}

	inDocument[node] = true
	for _, child := range node.Content {
		collectNodes(child, inDocument)
	}
}

// inlineAliasesTo replaces the first alias to a value the document no longer
// holds with a copy of the value, anchor included. The copy shares the value's
// children, which is safe because this is the last edit before rendering. walked
// bounds the walk, since a shared child can point back at the copy.
func inlineAliasesTo(node *yaml.Node, inDocument, walked map[*yaml.Node]bool) {
	if node == nil || walked[node] {
		return
	}
	walked[node] = true

	if node.Kind == yaml.AliasNode && node.Alias != nil && !inDocument[node.Alias] {
		// The value goes back in the document, so the aliases after this one stay
		// aliases rather than copies of it.
		collectNodes(node.Alias, inDocument)

		inlined := *node.Alias
		// Comments stay with the line they were written on.
		inlined.HeadComment, inlined.LineComment, inlined.FootComment =
			node.HeadComment, node.LineComment, node.FootComment
		*node = inlined
	}

	for _, child := range node.Content {
		inlineAliasesTo(child, inDocument, walked)
	}
}

// resolveAlias reads an alias through to the value it points at, the way
// cloud-init sees a document once yaml has loaded it. Anything else is itself.
func resolveAlias(node *yaml.Node) *yaml.Node {
	if node != nil && node.Kind == yaml.AliasNode && node.Alias != nil {
		return node.Alias
	}

	return node
}

// mappingValue returns the value a mapping holds for key, or nil when it holds
// none - a mapping that is not there holds nothing, so lookups chain. Aliases
// are read through, so every lookup sees what cloud-init sees.
func mappingValue(mappingNode *yaml.Node, key string) *yaml.Node {
	mappingNode = resolveAlias(mappingNode)
	if mappingNode == nil {
		return nil
	}

	index := mappingIndex(mappingNode, key)
	if index < 0 {
		return nil
	}

	return resolveAlias(mappingNode.Content[index])
}

// mappingIndex returns where a mapping holds its value for key, or -1 when it
// holds none. A key written twice is read the way cloud-init's loader reads it,
// where the last one wins.
func mappingIndex(mappingNode *yaml.Node, key string) int {
	index := -1
	for i := 0; i+1 < len(mappingNode.Content); i += 2 {
		keyNode := resolveAlias(mappingNode.Content[i])
		if keyNode.Kind == yaml.ScalarNode && keyNode.Value == key {
			index = i + 1
		}
	}

	return index
}

func scalarNode(value string) *yaml.Node {
	node := &yaml.Node{}
	node.SetString(value)

	return node
}

// ReverseMap swaps unique keys and values using Go generics. The input values
// must be unique; if two keys share a value the result keeps an arbitrary one.
func ReverseMap[K comparable, V comparable](m map[K]V) map[V]K {
	inverted := make(map[V]K, len(m))
	for k, v := range m {
		inverted[v] = k
	}
	return inverted
}

// SprintMapKeys renders a map's keys as a sorted, comma-separated string. It is
// handy for building deterministic "expected one of" error messages from an
// allow-list map so the wording cannot drift from the map it derives from.
func SprintMapKeys[K comparable, V any](m map[K]V) string {
	parts := make([]string, 0, len(m))
	for k := range m {
		parts = append(parts, fmt.Sprint(k))
	}
	slices.Sort(parts)
	return strings.Join(parts, ", ")
}

// ProtobufLabelsFromAPILabels converts API labels (map[string]string) to protobuf labels ([]*corev1.Label)
func ProtobufLabelsFromAPILabels(labels map[string]string) []*corev1.Label {
	if labels == nil {
		return nil
	}
	protoLabels := []*corev1.Label{}
	for k, v := range labels {
		protoLabels = append(protoLabels, &corev1.Label{
			Key:   k,
			Value: &v,
		})
	}
	return protoLabels
}

// ExpectedComponentUpdateField associates a Core field path with its request presence.
type ExpectedComponentUpdateField struct {
	Path    string
	Present bool
}

// ExpectedComponentUpdateMask returns a nonnil mask of present fields in caller order.
func ExpectedComponentUpdateMask(fields ...ExpectedComponentUpdateField) *fieldmaskpb.FieldMask {
	mask := &fieldmaskpb.FieldMask{}
	for _, field := range fields {
		if field.Present {
			mask.Paths = append(mask.Paths, field.Path)
		}
	}
	return mask
}
