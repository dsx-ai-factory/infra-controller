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
		removePhoneHomeParts(documentRoot, nil)
		err = appendPhoneHomePart(documentRoot, url)
	} else {
		err = insertPhoneHome(documentRoot, url)
	}
	if err != nil {
		return nil, err
	}

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
		removed = removePhoneHomeParts(documentRoot, url)
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
		keyNode, valueNode := mappingNode.Content[i], mappingNode.Content[i+1]
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
func removePhoneHomeParts(archiveRoot *yaml.Node, url *string) bool {
	removed := false
	kept := archiveRoot.Content[:0]

	for _, part := range archiveRoot.Content {
		content := cloudConfigArchiveContent(part)
		if content == nil {
			kept = append(kept, part)
			continue
		}

		stripped, err := disablePhoneHome(&content.Value, url)
		switch {
		case err != nil, stripped == nil, *stripped == content.Value:
			// Not a part phone-home can live in, or nothing to strip from it:
			// leave it exactly as authored.
			kept = append(kept, part)
		case *stripped == "":
			// The part held nothing but phone-home, so drop it entirely.
			removed = true
		default:
			content.SetString(*stripped)
			content.Style = yaml.LiteralStyle
			kept = append(kept, part)
			removed = true
		}
	}
	archiveRoot.Content = kept

	return removed
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
	contentNode.Style = yaml.LiteralStyle

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

func mappingValue(mappingNode *yaml.Node, key string) *yaml.Node {
	for i := 0; i+1 < len(mappingNode.Content); i += 2 {
		keyNode := mappingNode.Content[i]
		if keyNode.Kind == yaml.ScalarNode && keyNode.Value == key {
			return mappingNode.Content[i+1]
		}
	}

	return nil
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
