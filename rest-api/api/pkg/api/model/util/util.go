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

// Removes cloud-init phone-home blocks from the document root and, for
// autoinstall configurations, from the target system's user-data.
// If `url` is nil, then any phone-home block found will be removed.
// If `url` is non-nil, then the phone-home block will only be removed if
// the URL matches the value of `url`.
// RemovePhoneHomeFromUserData reports whether it removed any phone-home block,
// so callers can tell an untouched document from a modified one even when the
// removal happened in a nested autoinstall.user-data mapping.
func RemovePhoneHomeFromUserData(documentRoot *yaml.Node, url *string) (bool, error) {
	if documentRoot == nil {
		return false, fmt.Errorf("node must be non-nil for user-data removal")
	}

	// A #cloud-config-archive is a YAML sequence: phone-home lives in its own
	// cloud-config entry rather than at the document root.
	if isCloudConfigArchive(documentRoot) {
		return removePhoneHomeFromArchive(documentRoot, url)
	}

	if !isCloudConfig(documentRoot) {
		return false, fmt.Errorf("node must be a #cloud-config mapping or a #cloud-config-archive for user-data removal")
	}

	removed := removePhoneHomeFromMapping(documentRoot, url)

	autoinstallNode := mappingValue(documentRoot, autoinstallName)
	if autoinstallNode == nil || autoinstallNode.Kind != yaml.MappingNode {
		return removed, nil
	}

	targetUserDataNode := mappingValue(autoinstallNode, autoinstallUserData)
	if targetUserDataNode != nil && targetUserDataNode.Kind == yaml.MappingNode {
		if removePhoneHomeFromMapping(targetUserDataNode, url) {
			removed = true
		}
	}

	return removed, nil
}

// removePhoneHomeFromMapping removes phone-home from a mapping node and reports
// whether it removed anything.
func removePhoneHomeFromMapping(mappingNode *yaml.Node, url *string) bool {
	removed := false
	contentLen := len(mappingNode.Content)

	// If phone-home is being disabled, then delete
	// any phone-home data that might exist.
	// Go through the YAML nodes and look for our target.
	// We've previously determined that mappingNode is a
	// valid MappingNode, so the contents will be pairs of nodes
	// representing key/value pairs of the map.
	//
	// Note there are no breaks or early returns from outer loop because a user
	// could have submitted valid but nonsensical YAML with
	// multiple phone-home blocks.
	for i := 0; i < contentLen; i += 2 {
		mapKeyNode := mappingNode.Content[i]
		mapValueNode := mappingNode.Content[i+1]

		// No breaks or early-returns here from outer loop because the user could have submitted
		// valid but nonsensical YAML that includes a phone-home block multiple times.
		if mapKeyNode.Kind == yaml.ScalarNode && mapKeyNode.Value == SitePhoneHomeName {
			// Check if the next node is a map, which will be the phone_home map itself.
			if mapValueNode.Kind == yaml.MappingNode {

				if url == nil {
					// Snip out the target while preserving the order of the nodes.
					// We have to snip out the key (phone_home) and the value
					// (the actual map node), so +2
					// We're working with pairs here, so the second slice-expression
					// won't violate bounds.
					mappingNode.Content = append(mappingNode.Content[:i], mappingNode.Content[i+2:]...)

					// Shift the "pointer" backwards since we
					// just modified mappingNode.Content "in-place"
					i -= 2

					// Reduce the loop limit since the
					// list being worked on is shorter now.
					contentLen = len(mappingNode.Content)
					removed = true
					continue
				}

				// Get the nodes in the map.
				phoneHomeMapSubNodes := mapValueNode.Content

				// Go through the map nodes and look for the URL key.
				// Again, MappingNode, so we can expect k/v node pairs.
				for j := 0; j < len(phoneHomeMapSubNodes); j += 2 {

					phoneHomeMapKeyNode := phoneHomeMapSubNodes[j]
					phoneHomeMapValueNode := phoneHomeMapSubNodes[j+1]
					if phoneHomeMapKeyNode.Kind == yaml.ScalarNode && phoneHomeMapKeyNode.Value == SitePhoneHomeUrl {
						if phoneHomeMapValueNode.Value == *url {
							mappingNode.Content = append(mappingNode.Content[:i], mappingNode.Content[i+2:]...)
							i -= 2
							contentLen = len(mappingNode.Content)
							removed = true
							break
						}
					}
				}

			}
		}
	}

	return removed
}

func InsertPhoneHomeIntoUserData(documentRoot *yaml.Node, url string) error {
	if documentRoot == nil {
		return fmt.Errorf("node must be non-nil for user-data insertion")
	}

	// A #cloud-config-archive is a YAML sequence: append phone-home as a new
	// cloud-config entry instead of inserting into the document root.
	if isCloudConfigArchive(documentRoot) {
		return insertPhoneHomeIntoArchive(documentRoot, url)
	}

	if !isCloudConfig(documentRoot) {
		return fmt.Errorf("node must be a #cloud-config mapping or a #cloud-config-archive for user-data insertion")
	}

	if documentRoot.Content == nil {
		documentRoot.Content = []*yaml.Node{}
	}

	insertionNode := documentRoot
	autoinstallNode := mappingValue(documentRoot, autoinstallName)
	if autoinstallNode != nil {
		if autoinstallNode.Kind != yaml.MappingNode {
			return errors.New("autoinstall must be a mapping to insert phone-home")
		}

		insertionNode = mappingValue(autoinstallNode, autoinstallUserData)
		if insertionNode == nil {
			insertionNode = &yaml.Node{
				Kind: yaml.MappingNode,
				Tag:  "!!map",
			}
			targetUserDataKeyNode := &yaml.Node{}
			targetUserDataKeyNode.SetString(autoinstallUserData)
			autoinstallNode.Content = append(autoinstallNode.Content, targetUserDataKeyNode, insertionNode)
		} else if insertionNode.Kind != yaml.MappingNode {
			return errors.New("autoinstall user-data must be a mapping to insert phone-home")
		}
	}

	// Remove existing phone-home blocks from both supported locations before
	// inserting the canonical block.
	if _, err := RemovePhoneHomeFromUserData(documentRoot, nil); err != nil {
		return err
	}

	// Build the PhoneHome user-data section.
	phoneHomeConfigMap := map[string]string{}
	phoneHomeConfigMap[SitePhoneHomeUrl] = url
	phoneHomeConfigMap[SitePhoneHomePost] = SitePhoneHomePostAll

	// Encode it into a new YAML node so we can
	// add it to the selected content later.
	phoneHomeValueNode := &yaml.Node{}
	if err := phoneHomeValueNode.Encode(phoneHomeConfigMap); err != nil {
		return errors.New("failed to insert phone-home into userData")
	}
	phoneHomeKeyNode := &yaml.Node{}
	phoneHomeKeyNode.SetString(SitePhoneHomeName)

	// Append the node that we can marshal it back out later.
	insertionNode.Content = append(insertionNode.Content, phoneHomeKeyNode, phoneHomeValueNode)

	// Ensure #cloud-config is present as a head comment
	foundCloudConfig := false
	for _, node := range documentRoot.Content {
		if node.HeadComment == SiteCloudConfig {
			foundCloudConfig = true
			break
		}
	}

	if !foundCloudConfig {
		if documentRoot.Kind == yaml.MappingNode {
			if documentRoot.HeadComment == "" {
				documentRoot.HeadComment = SiteCloudConfig
			}
		}
	}

	return nil
}

// PhoneHomeSupportsUserDataRoot reports whether a cloud-init user-data document
// root can carry a phone-home block: a #cloud-config document (mapping) has the
// block inserted at its root, while a #cloud-config-archive (sequence) gets a
// dedicated phone-home cloud-config appended as a new archive entry.
func PhoneHomeSupportsUserDataRoot(documentRoot *yaml.Node) bool {
	return isCloudConfig(documentRoot) || isCloudConfigArchive(documentRoot)
}

// isCloudConfig reports whether documentRoot is #cloud-config user-data: a
// mapping whose header, if present, is the #cloud-config marker. A header-less
// mapping is accepted (the header is added on output), but a mapping carrying a
// different header - e.g. a #!/bin/bash script that happens to parse as a map -
// is rejected.
func isCloudConfig(documentRoot *yaml.Node) bool {
	if documentRoot == nil || documentRoot.Kind != yaml.MappingNode {
		return false
	}

	header := userDataHeader(documentRoot)

	return header == "" || header == SiteCloudConfig
}

// isCloudConfigArchive reports whether documentRoot is a #cloud-config-archive:
// a YAML sequence whose first line carries the cloud-init archive header. A
// header-less list is not valid cloud-init user-data, so it is not treated as
// an archive.
func isCloudConfigArchive(documentRoot *yaml.Node) bool {
	return documentRoot != nil && documentRoot.Kind == yaml.SequenceNode &&
		userDataHeader(documentRoot) == SiteCloudConfigArchive
}

// userDataHeader returns the cloud-init format header that yaml attaches as the
// head comment of the document's first child (e.g. #cloud-config or
// #cloud-config-archive), or "" when there is none.
func userDataHeader(documentRoot *yaml.Node) string {
	if documentRoot == nil {
		return ""
	}

	// yaml attaches the header to the node itself for an empty archive, and to
	// the first child otherwise.
	comment := documentRoot.HeadComment
	if comment == "" && len(documentRoot.Content) > 0 {
		comment = documentRoot.Content[0].HeadComment
	}

	firstLine, _, _ := strings.Cut(comment, "\n")

	return strings.TrimSpace(firstLine)
}

// insertPhoneHomeIntoArchive appends a dedicated phone-home cloud-config entry to
// a #cloud-config-archive. cloud-init merges each archive part independently, so a
// standalone phone_home part takes effect on its own. The entry content is built
// by the same InsertPhoneHomeIntoUserData path used for #cloud-config documents,
// so both formats share one source of truth for the phone-home block. Any
// existing phone-home entry is removed first to keep re-enabling idempotent.
func insertPhoneHomeIntoArchive(archiveRoot *yaml.Node, url string) error {
	if _, err := removePhoneHomeFromArchive(archiveRoot, nil); err != nil {
		return err
	}

	content := &yaml.Node{Kind: yaml.MappingNode, Tag: "!!map"}
	if err := InsertPhoneHomeIntoUserData(content, url); err != nil {
		return err
	}

	rendered, err := yaml.Marshal(content)
	if err != nil {
		return errors.New("failed to render phone-home cloud-config")
	}

	archiveRoot.Content = append(archiveRoot.Content, newCloudConfigArchiveEntry(string(rendered)))

	return nil
}

// removePhoneHomeFromArchive strips phone-home from every cloud-config entry of a
// #cloud-config-archive using the same RemovePhoneHomeFromUserData path as
// #cloud-config documents. Entries left empty are dropped, entries with no
// phone-home are left untouched, and the archive header comment yaml attaches to
// the first element is preserved even when that element is removed.
func removePhoneHomeFromArchive(archiveRoot *yaml.Node, url *string) (bool, error) {
	// Capture the archive header so it survives even when its carrier entry is
	// removed. yaml attaches it to the first entry, or to the sequence node
	// itself for an empty archive.
	header := archiveRoot.HeadComment
	if header == "" && len(archiveRoot.Content) > 0 {
		header = archiveRoot.Content[0].HeadComment
	}

	archiveRemoved := false
	kept := archiveRoot.Content[:0]
	for _, entry := range archiveRoot.Content {
		content := cloudConfigArchiveContent(entry)
		if content == nil {
			kept = append(kept, entry)
			continue
		}

		document := &yaml.Node{}
		if err := yaml.Unmarshal([]byte(content.Value), document); err != nil || len(document.Content) == 0 {
			kept = append(kept, entry)
			continue
		}

		root := document.Content[0]
		if !PhoneHomeSupportsUserDataRoot(root) {
			// e.g. a script whose content parses as a map; leave it untouched.
			kept = append(kept, entry)
			continue
		}

		removed, err := RemovePhoneHomeFromUserData(root, url)
		if err != nil {
			return false, err
		}

		switch {
		case !removed:
			// Nothing removed here; keep the entry as authored.
			kept = append(kept, entry)
		case len(root.Content) == 0:
			// The entry held only phone-home, so drop it entirely.
			archiveRemoved = true
		default:
			// A nested block was removed (e.g. under autoinstall.user-data);
			// re-render so the change is persisted.
			rendered, err := yaml.Marshal(document)
			if err != nil {
				return false, errors.New("failed to re-render archive entry after removing phone-home")
			}
			content.SetString(string(rendered))
			content.Style = yaml.LiteralStyle
			kept = append(kept, entry)
			archiveRemoved = true
		}
	}
	archiveRoot.Content = kept

	// Restore the header onto whichever node now carries it: the sequence node
	// itself once the last entry is removed, otherwise the first remaining
	// entry - prepending it when that entry already has its own comment so the
	// header is not lost.
	switch {
	case header == "":
	case len(archiveRoot.Content) == 0:
		archiveRoot.HeadComment = header
	case strings.HasPrefix(archiveRoot.Content[0].HeadComment, SiteCloudConfigArchive):
		// already carries the archive header
	case archiveRoot.Content[0].HeadComment == "":
		archiveRoot.Content[0].HeadComment = header
	default:
		archiveRoot.Content[0].HeadComment = header + "\n" + archiveRoot.Content[0].HeadComment
	}

	return archiveRemoved, nil
}

// newCloudConfigArchiveEntry builds a {type: text/cloud-config, content: ...}
// mapping node for inclusion in a #cloud-config-archive sequence.
func newCloudConfigArchiveEntry(content string) *yaml.Node {
	typeKey := &yaml.Node{}
	typeKey.SetString(archiveEntryType)
	typeValue := &yaml.Node{}
	typeValue.SetString(archiveContentType)

	contentKey := &yaml.Node{}
	contentKey.SetString(archiveEntryContent)
	contentValue := &yaml.Node{}
	contentValue.SetString(content)
	contentValue.Style = yaml.LiteralStyle

	return &yaml.Node{
		Kind:    yaml.MappingNode,
		Tag:     "!!map",
		Content: []*yaml.Node{typeKey, typeValue, contentKey, contentValue},
	}
}

// cloudConfigArchiveContent returns the content scalar of a cloud-config archive
// entry, or nil if the entry is not a cloud-config part (e.g. a shell script) or
// carries no string content. An entry with no explicit type is treated as
// cloud-config, matching cloud-init's default handling.
func cloudConfigArchiveContent(entry *yaml.Node) *yaml.Node {
	if entry == nil || entry.Kind != yaml.MappingNode {
		return nil
	}

	if typeNode := mappingValue(entry, archiveEntryType); typeNode != nil &&
		typeNode.Value != "" && typeNode.Value != archiveContentType {
		return nil
	}

	contentNode := mappingValue(entry, archiveEntryContent)
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
