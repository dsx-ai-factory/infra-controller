// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
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

	// jinjaTemplateHeader marks user-data as a Jinja template, which is not a
	// document to render back: yaml reads `{{ x }}` as a mapping and would write
	// it back as one.
	jinjaTemplateHeader = "## template: jinja"

	// userDataIndentSpaces matches the indent cloud-config is conventionally
	// written with. The encoder's default of 4 re-indents every nested line,
	// which grew realistic cloud-config documents by 9% to 31% and pushed
	// requests that were under MaxUserDataBytes past it. A sequence written
	// flush against its parent key still grows, because the encoder always
	// indents a sequence under the key that owns it.
	userDataIndentSpaces = 2
)

// marshalUserData renders a cloud-init document back to YAML. Use it rather
// than yaml.Marshal, whose fixed 4-space indent inflates the stored value.
// renderUserData is its only caller: user-data is rendered below its header,
// never on its own.
func marshalUserData(document *yaml.Node) ([]byte, error) {
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

// ErrUnsupportedUserData reports user-data phone-home cannot be edited into:
// not a #cloud-config mapping or a #cloud-config-archive sequence, but a script,
// a jinja template, text that is not YAML, or more than one document, which
// cloud-init's loader will not read either. Enabling phone-home rejects it;
// disabling leaves it alone rather than rewriting it.
var ErrUnsupportedUserData = errors.New("userData is not a #cloud-config or #cloud-config-archive document")

// EnablePhoneHomeInUserData returns userData with a phone-home block reporting
// to url, replacing the blocks it can edit: one in an archive part left as
// authored stays, and the block delivered last is the one that takes effect.
// Nil or empty user-data yields a #cloud-config document holding just the
// block.
func EnablePhoneHomeInUserData(userData *string, url string) (*string, error) {
	header, documentRoot, err := parseUserData(userData, "")
	if err != nil {
		return nil, err
	}
	if header == "" {
		// The only user-data reaching here without a header is the empty document
		// parseUserData hands back for none at all, which is ours to write.
		header = SiteCloudConfig + "\n"
	}

	// cloud-init merges the parts into one config, later keys replacing earlier
	// ones, so phone-home is delivered as a part of its own rather than folded
	// into somebody else's.
	if documentRoot.Kind == yaml.SequenceNode {
		if _, err := removePhoneHomeParts(documentRoot, nil); err != nil {
			return nil, err
		}
		err = insertPhoneHomePart(documentRoot, url)
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
	return disablePhoneHome(userData, &url, "")
}

// DisableAllPhoneHomeInUserData is DisablePhoneHomeInUserData for every
// phone-home block, whatever it reports to. It is for user-data we authored the
// block in: the URL frozen into it may predate a change to the configured one.
func DisableAllPhoneHomeInUserData(userData *string) (*string, error) {
	return disablePhoneHome(userData, nil, "")
}

// disablePhoneHome is DisablePhoneHomeInUserData; a nil url matches every block.
// contentFormat is parseUserData's.
func disablePhoneHome(userData *string, url *string, contentFormat string) (*string, error) {
	if userData == nil || *userData == "" {
		return nil, nil
	}

	header, documentRoot, err := parseUserData(userData, contentFormat)
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
// sequence can carry phone-home. contentFormat is the format when something
// other than the header decides it, as an archive entry's type does; "" leaves
// it to the header, which is how top-level user-data declares it. No user-data
// at all is an empty document, which is ours to write.
func parseUserData(userData *string, contentFormat string) (string, *yaml.Node, error) {
	if userData == nil || *userData == "" {
		return "", &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}, nil
	}

	header, body := splitUserDataHeader(*userData)

	decoder := yaml.NewDecoder(strings.NewReader(body))

	document := &yaml.Node{}
	if err := decoder.Decode(document); err != nil || len(document.Content) == 0 {
		return "", nil, ErrUnsupportedUserData
	}

	// cloud-init reads one document out of user-data, and rendering back what we
	// read would drop the rest, so more than one is not user-data we can edit.
	if err := decoder.Decode(&yaml.Node{}); !errors.Is(err, io.EOF) {
		return "", nil, ErrUnsupportedUserData
	}

	documentRoot := document.Content[0]

	format := userDataFormat(header)

	switch {
	case format == jinjaTemplateHeader:
		return "", nil, ErrUnsupportedUserData
	case contentFormat != "":
		format = contentFormat
	case format == "":
		format = SiteCloudConfig
	}

	switch {
	case format == SiteCloudConfigArchive && documentRoot.Kind == yaml.SequenceNode:
	case format == SiteCloudConfig && documentRoot.Kind == yaml.MappingNode:
	default:
		// A template is not a document to render back - yaml reads `{{ x }}` as a
		// mapping and would write it back as one - every other format is one
		// cloud-init runs elsewhere, and a document declaring none where none is
		// read is one it runs no part of.
		return "", nil, ErrUnsupportedUserData
	}

	return header, documentRoot, nil
}

// renderUserData renders a document below its header. The header is text, the
// way cloud-config is written from scratch, so no edit to the document can lose
// it - not even removing the first key or part, which is where yaml would
// otherwise keep it.
func renderUserData(header string, documentRoot *yaml.Node) (*string, error) {
	rendered, err := marshalUserData(documentRoot)
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

// splitUserDataHeader splits user-data into its cloud-init header line, newline
// included, and the YAML body below it. The whitespace before the header is
// skipped to find it and returned with it, the way cloud-init reads past it, so
// header and body hold every byte of text. Only spaces and newlines are
// skipped: a document written behind a tab is not one yaml can read, whatever
// header cloud-init finds above it.
func splitUserDataHeader(userData string) (header, body string) {
	whitespace := len(userData) - len(strings.TrimLeft(userData, " \r\n"))

	line, body, found := strings.Cut(userData[whitespace:], "\n")
	if !found || !strings.HasPrefix(line, "#") {
		return "", userData
	}

	return userData[:len(userData)-len(body)], body
}

// userDataFormatMarkers are the format markers cloud-init matches, longest
// first because the shorter ones prefix the longer, the way it searches its own
// list. Every format it dispatches is named, not only the two we edit, so a
// header declaring one of the others is told apart from one declaring nothing -
// an author's comment - which an archive entry reads as cloud-config.
//
// See https://github.com/canonical/cloud-init/blob/main/cloudinit/handlers/__init__.py
var userDataFormatMarkers = []string{
	SiteCloudConfigArchive,
	"#cloud-config-jsonp",
	jinjaTemplateHeader,
	"#cloud-boothook",
	SiteCloudConfig,
	"#include-once",
	"#part-handler",
	"#include",
	"#!",
}

// userDataFormat reports the format a header declares, as the marker naming it,
// or "" when it declares none. cloud-init matches a marker on the start of the
// payload, ignoring case and reading past whatever follows it, so that is how
// these are matched.
func userDataFormat(header string) string {
	header = strings.ToLower(strings.TrimSpace(header))

	for _, marker := range userDataFormatMarkers {
		if strings.HasPrefix(header, marker) {
			return marker
		}
	}

	return ""
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

	if len(insertionNode.Content) == 0 {
		// An emptied mapping renders as `{}`, which reads back flow-styled, and an
		// entry rewritten through its text goes through exactly that. Nothing was
		// written in it to preserve, so the block goes in written out.
		insertionNode.Style = 0
	}

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
	return removePhoneHomeFromMerged(mappingNode, url, map[*yaml.Node]bool{})
}

// removePhoneHomeFromMerged deletes the phone_home keys of a mapping and of the
// mappings it merges in with `<<`: cloud-init reads a merged block as the
// mapping's own, so taking it out means taking it out where it is written. read
// bounds the walk, since a merge can point back at a mapping already scanned.
func removePhoneHomeFromMerged(mappingNode *yaml.Node, url *string, read map[*yaml.Node]bool) bool {
	mappingNode = resolveAlias(mappingNode)
	if mappingNode == nil || read[mappingNode] {
		return false
	}
	read[mappingNode] = true

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

	for _, merged := range mergedMappings(mappingNode) {
		removed = removePhoneHomeFromMerged(merged, url, read) || removed
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
	kept := make([]*yaml.Node, 0, len(archiveRoot.Content))

	for _, part := range archiveRoot.Content {
		content, declaredFormat := cloudConfigArchiveContent(part)
		if content == nil {
			kept = append(kept, part)
			continue
		}

		stripped, err := disablePhoneHome(&content.Value, url, declaredFormat)
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
			removed = true
		default:
			kept = append(kept, partWithContent(part, *stripped))
			removed = true
		}
	}
	archiveRoot.Content = kept

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

		return
	}

	// The content is one the part merges in, so the part takes a content key of
	// its own: a part's own keys win over the ones it merges.
	if merged := mappingValue(part, archiveEntryContent); merged != nil {
		part.Content = append(part.Content,
			scalarNode(archiveEntryContent), rewrittenContent(merged, text))
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

// insertPhoneHomePart adds phone-home to an archive: under the autoinstall of
// the part that installs a target system, since that is the config the installed
// system runs, and otherwise as a part of its own.
func insertPhoneHomePart(archiveRoot *yaml.Node, url string) error {
	for i, part := range archiveRoot.Content {
		content, declaredFormat := cloudConfigArchiveContent(part)
		if content == nil {
			continue
		}

		header, partRoot, err := parseUserData(&content.Value, declaredFormat)
		if err != nil || !installsATargetSystem(partRoot) {
			// An entry phone-home cannot be edited into - a template included - is
			// passed over rather than failing the request over somebody else's.
			continue
		}

		if err := insertPhoneHome(partRoot, url); err != nil {
			return err
		}
		inlineDanglingAliases(partRoot)

		rendered, err := renderUserData(header, partRoot)
		if err != nil {
			return err
		}
		archiveRoot.Content[i] = partWithContent(part, *rendered)

		return nil
	}

	return appendPhoneHomePart(archiveRoot, url)
}

// installsATargetSystem reports whether a document installs a system of its own,
// which is the document phone-home belongs in. The autoinstall's shape is
// insertPhoneHome's to check: an entry of our own would report from the
// installer, so a malformed one is reported rather than worked around.
func installsATargetSystem(documentRoot *yaml.Node) bool {
	return documentRoot.Kind == yaml.MappingNode &&
		mappingValue(documentRoot, autoinstallName) != nil
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

	if len(archiveRoot.Content) == 0 {
		// An emptied archive renders as `[]` and reads back flow-styled, the same
		// way an emptied mapping does.
		archiveRoot.Style = 0
	}

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
// carries no string content, along with the format the part's type declares -
// "" when it declares none and the content's own header says.
func cloudConfigArchiveContent(part *yaml.Node) (*yaml.Node, string) {
	part = resolveAlias(part)
	if part == nil {
		return nil, ""
	}

	// cloud-init reads a scalar part as the content of a part with no type.
	if part.Kind == yaml.ScalarNode {
		return part, ""
	}

	if part.Kind != yaml.MappingNode {
		return nil, ""
	}

	// A structured type is malformed, not an omitted one, so the part is left
	// alone rather than read as cloud-config. cloud-init dispatches on the type
	// lowercased, so the match is too, and reads the content of a part whose type
	// is null - which is what an empty one loads as.
	declaredFormat := ""
	if typeNode := mappingValue(part, archiveEntryType); typeNode != nil && typeNode.Tag != "!!null" {
		if typeNode.Kind != yaml.ScalarNode || strings.ToLower(typeNode.Value) != archiveContentType {
			return nil, ""
		}

		declaredFormat = SiteCloudConfig
	}

	contentNode := mappingValue(part, archiveEntryContent)
	if contentNode == nil || contentNode.Kind != yaml.ScalarNode {
		return nil, ""
	}

	return contentNode, declaredFormat
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
	return valueInMapping(mappingNode, key, map[*yaml.Node]bool{})
}

// valueInMapping reads key out of a mapping and, failing that, out of the
// mappings it merges in with `<<`, the way yaml's loader resolves a merge: a
// mapping's own keys win over the ones it merges, and the first mapping it
// merges wins over the ones after it. read bounds the walk, since a merge can
// point back at a mapping already read.
func valueInMapping(mappingNode *yaml.Node, key string, read map[*yaml.Node]bool) *yaml.Node {
	mappingNode = resolveAlias(mappingNode)
	if mappingNode == nil || read[mappingNode] {
		return nil
	}
	read[mappingNode] = true

	if index := mappingIndex(mappingNode, key); index >= 0 {
		return resolveAlias(mappingNode.Content[index])
	}

	for _, merged := range mergedMappings(mappingNode) {
		if value := valueInMapping(merged, key, read); value != nil {
			return value
		}
	}

	return nil
}

// mergedMappings returns the mappings a mapping merges in with `<<`, in the
// order yaml applies them: every merge key it holds, each naming one mapping or
// listing a sequence of them, the first of which wins.
func mergedMappings(mappingNode *yaml.Node) []*yaml.Node {
	if mappingNode.Kind != yaml.MappingNode {
		return nil
	}

	var merged []*yaml.Node
	for i := 0; i+1 < len(mappingNode.Content); i += 2 {
		if keyNode := resolveAlias(mappingNode.Content[i]); keyNode.Value != mergeKey {
			continue
		}

		sources := resolveAlias(mappingNode.Content[i+1])
		if sources.Kind != yaml.SequenceNode {
			merged = append(merged, sources)

			continue
		}

		for _, source := range sources.Content {
			merged = append(merged, resolveAlias(source))
		}
	}

	return merged
}

// mappingIndex returns where a mapping holds its value for key, or -1 when it
// holds none. A key written twice is read the way cloud-init's loader reads it,
// where the last one wins.
func mappingIndex(mappingNode *yaml.Node, key string) int {
	if mappingNode.Kind != yaml.MappingNode {
		return -1
	}

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
