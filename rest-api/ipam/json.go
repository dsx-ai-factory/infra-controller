/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"encoding/json"
	"fmt"
)

type prefixJSON struct {
	Prefix
	AvailableChildPrefixes map[string]bool // available child prefixes of this prefix
	// TODO remove this in the next release
	ChildPrefixLength int             // the length of the child prefixes. Legacy to migrate existing prefixes stored in the db to set the IsParent on reads.
	IsParent          bool            // set to true if there are child prefixes
	IPs               map[string]bool // The ips contained in this prefix
	Version           int64           // Version is used for optimistic locking
}

func (p prefixJSON) toPrefix() Prefix {
	// Legacy support only on reading from database, convert to isParent.
	// TODO remove this in the next release
	if p.ChildPrefixLength > 0 {
		p.IsParent = true
	}
	return Prefix{
		Cidr:                   p.Cidr,
		ParentCidr:             p.ParentCidr,
		availableChildPrefixes: p.AvailableChildPrefixes,
		childPrefixLength:      p.ChildPrefixLength,
		isParent:               p.IsParent,
		ips:                    p.IPs,
		version:                p.Version,
		Namespace:              p.Namespace,
	}
}

func (p Prefix) toPrefixJSON() prefixJSON {
	return prefixJSON{
		Prefix: Prefix{
			Cidr:       p.Cidr,
			ParentCidr: p.ParentCidr,
			Namespace:  p.Namespace,
		},
		AvailableChildPrefixes: p.availableChildPrefixes,
		IsParent:               p.isParent,
		// TODO remove this in the next release
		ChildPrefixLength: p.childPrefixLength,
		IPs:               p.ips,
		Version:           p.version,
	}
}

func (p Prefix) toJSON() ([]byte, error) {
	pj, err := json.Marshal(p.toPrefixJSON())
	if err != nil {
		return nil, fmt.Errorf("unable to marshal prefix:%w", err)
	}
	return pj, nil
}

func (ps Prefixes) toJSON() ([]byte, error) {
	var pfxjs []prefixJSON
	for _, p := range ps {
		pfxjs = append(pfxjs, p.toPrefixJSON())
	}
	pj, err := json.Marshal(pfxjs)
	if err != nil {
		return nil, fmt.Errorf("unable to marshal prefixes:%w", err)
	}
	return pj, nil
}

func fromJSON(js []byte) (Prefix, error) {
	var pre prefixJSON
	err := json.Unmarshal(js, &pre)
	if err != nil {
		return Prefix{}, fmt.Errorf("unable to unmarshal prefix:%w", err)
	}
	return pre.toPrefix(), nil
}

func fromJSONs(js []byte) (Prefixes, error) {
	var pres []prefixJSON
	err := json.Unmarshal(js, &pres)
	if err != nil {
		return Prefixes{}, fmt.Errorf("unable to unmarshal prefixes:%w", err)
	}
	var pfxs Prefixes
	for _, pj := range pres {
		pfxs = append(pfxs, pj.toPrefix())
	}
	return pfxs, nil
}
