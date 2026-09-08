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
	"gopkg.in/yaml.v3"
)

const (
	// configuration for phone home
	SitePhoneHomeName    = "phone_home"
	SitePhoneHomePost    = "post"
	SitePhoneHomePostAll = "all"
	SitePhoneHomeUrl     = "url"
	SiteCloudConfig      = "#cloud-config"
	autoinstallName      = "autoinstall"
	autoinstallUserData  = "user-data"

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
func RemovePhoneHomeFromUserData(documentRoot *yaml.Node, url *string) error {
	if documentRoot == nil || documentRoot.Kind != yaml.MappingNode {
		return fmt.Errorf("node must be non-nil MappingNode for user-data removal")
	}

	removePhoneHomeFromMapping(documentRoot, url)

	autoinstallNode := mappingValue(documentRoot, autoinstallName)
	if autoinstallNode == nil || autoinstallNode.Kind != yaml.MappingNode {
		return nil
	}

	targetUserDataNode := mappingValue(autoinstallNode, autoinstallUserData)
	if targetUserDataNode != nil && targetUserDataNode.Kind == yaml.MappingNode {
		removePhoneHomeFromMapping(targetUserDataNode, url)
	}

	return nil
}

func removePhoneHomeFromMapping(mappingNode *yaml.Node, url *string) {
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
							break
						}
					}
				}

			}
		}
	}
}

func InsertPhoneHomeIntoUserData(documentRoot *yaml.Node, url string) error {
	if documentRoot == nil || documentRoot.Kind != yaml.MappingNode {
		return fmt.Errorf("node must be non-nil MappingNode for user-data insertion")
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
	if err := RemovePhoneHomeFromUserData(documentRoot, nil); err != nil {
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
