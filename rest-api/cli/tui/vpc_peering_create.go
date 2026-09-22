// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"
)

const vpcPeeringCreateCommand = "vpc-peering create"

type vpcPeeringPair struct {
	first  NamedItem
	second NamedItem
}

func (p vpcPeeringPair) label() string {
	return fmt.Sprintf("%s (%s) <-> %s (%s)", p.first.Name, p.first.ID, p.second.Name, p.second.ID)
}

type vpcPeeringPairKey struct {
	first  string
	second string
}

func newVPCPeeringPairKey(first, second string) vpcPeeringPairKey {
	first = strings.TrimSpace(first)
	second = strings.TrimSpace(second)
	if first > second {
		first, second = second, first
	}
	return vpcPeeringPairKey{first: first, second: second}
}

type vpcPeeringPlan struct {
	create []vpcPeeringPair
	skip   []vpcPeeringPair
}

func buildVPCPeeringPlan(selected, existing []NamedItem) vpcPeeringPlan {
	unique := make([]NamedItem, 0, len(selected))
	seen := make(map[string]struct{}, len(selected))
	for _, vpc := range selected {
		vpc.ID = strings.TrimSpace(vpc.ID)
		if vpc.ID == "" {
			continue
		}
		if _, duplicate := seen[vpc.ID]; duplicate {
			continue
		}
		seen[vpc.ID] = struct{}{}
		unique = append(unique, vpc)
	}

	existingPairs := make(map[vpcPeeringPairKey]struct{}, len(existing))
	for _, peering := range existing {
		first := strings.TrimSpace(peering.Extra["vpc1Id"])
		second := strings.TrimSpace(peering.Extra["vpc2Id"])
		if first == "" || second == "" || first == second {
			continue
		}
		existingPairs[newVPCPeeringPairKey(first, second)] = struct{}{}
	}

	plan := vpcPeeringPlan{}
	for i, first := range unique {
		for _, second := range unique[i+1:] {
			pair := vpcPeeringPair{first: first, second: second}
			if _, exists := existingPairs[newVPCPeeringPairKey(first.ID, second.ID)]; exists {
				plan.skip = append(plan.skip, pair)
				continue
			}
			plan.create = append(plan.create, pair)
		}
	}
	return plan
}

func cmdVPCPeeringCreate(s *Session, args []string) error {
	if len(args) > 0 {
		info, ok := generatedAutocompleteInfo(vpcPeeringCreateCommand)
		if !ok {
			return fmt.Errorf("generated command %q is unavailable", vpcPeeringCreateCommand)
		}
		return runGeneratedTUICommand(s, info, args)
	}

	siteID, err := requireSiteScope(s, "VPC peering creation requires a site. Select a site.")
	if err != nil {
		return err
	}
	selected, err := promptVPCPeeringVPCs(s, siteID)
	if err != nil {
		return err
	}
	existing, err := fetchVPCPeeringsForSite(s, siteID)
	if err != nil {
		return fmt.Errorf("listing existing VPC peerings: %w", err)
	}

	plan := buildVPCPeeringPlan(selected, existing)
	printVPCPeeringPlan(os.Stdout, selected, plan)
	if len(plan.create) > 0 {
		confirmed, confirmErr := PromptConfirm(fmt.Sprintf("Create %d VPC peering(s)?", len(plan.create)))
		if confirmErr != nil {
			return confirmErr
		}
		if !confirmed {
			return nil
		}
	}
	return executeVPCPeeringPlan(s, siteID, plan, os.Stdout)
}

func promptVPCPeeringVPCs(s *Session, siteID string) ([]NamedItem, error) {
	savedVpcID, savedVpcName := s.Scope.VpcID, s.Scope.VpcName
	s.Scope.VpcID, s.Scope.VpcName = "", ""
	s.Cache.InvalidateFiltered()
	defer func() {
		s.Scope.VpcID, s.Scope.VpcName = savedVpcID, savedVpcName
		s.Cache.InvalidateFiltered()
	}()

	vpcs, err := s.Resolver.Fetch(context.Background(), "vpc")
	if err != nil {
		return nil, fmt.Errorf("listing VPCs: %w", err)
	}
	siteVPCs := readyVPCsForSite(vpcs, siteID)
	if len(siteVPCs) < 2 {
		return nil, fmt.Errorf("site requires at least two Ready VPCs to create peerings")
	}

	mode, err := PromptChoice(
		"VPC selection",
		[]string{"Choose VPCs", "Select all"},
		"Choose VPCs",
	)
	if err != nil {
		return nil, err
	}
	if mode == "Select all" {
		return siteVPCs, nil
	}

	selected := make([]NamedItem, 0, len(siteVPCs))
	selectedIDs := make(map[string]struct{}, len(siteVPCs))
	for len(selected) < len(siteVPCs) {
		available := make([]NamedItem, 0, len(siteVPCs)-len(selected))
		for _, vpc := range siteVPCs {
			if _, picked := selectedIDs[vpc.ID]; !picked {
				available = append(available, vpc)
			}
		}
		vpc, selectErr := s.Resolver.SelectFromItems("VPC", available)
		if selectErr != nil {
			return nil, selectErr
		}
		selected = append(selected, *vpc)
		selectedIDs[vpc.ID] = struct{}{}
		if len(selected) < 2 {
			continue
		}
		if len(selected) == len(siteVPCs) {
			break
		}
		more, promptErr := PromptConfirm(fmt.Sprintf("Add another VPC (selected %d)?", len(selected)))
		if promptErr != nil {
			return nil, promptErr
		}
		if !more {
			break
		}
	}
	return selected, nil
}

func readyVPCsForSite(vpcs []NamedItem, siteID string) []NamedItem {
	ready := make([]NamedItem, 0, len(vpcs))
	for _, vpc := range vpcs {
		if strings.TrimSpace(vpc.Extra["siteId"]) == siteID &&
			strings.EqualFold(strings.TrimSpace(vpc.Status), "Ready") {
			ready = append(ready, vpc)
		}
	}
	return ready
}

func fetchVPCPeeringsForSite(s *Session, siteID string) ([]NamedItem, error) {
	items, err := s.fetchAll(apiPath(s, "vpc-peering"), map[string]string{"siteId": siteID})
	if err != nil {
		return nil, err
	}
	peerings := make([]NamedItem, 0, len(items))
	for _, item := range items {
		peerings = append(peerings, NamedItem{Extra: map[string]string{
			"vpc1Id": str(item, "vpc1Id"),
			"vpc2Id": str(item, "vpc2Id"),
		}})
	}
	return peerings, nil
}

func printVPCPeeringPlan(w io.Writer, selected []NamedItem, plan vpcPeeringPlan) {
	fmt.Fprintf(w, "Selected VPCs (%d):\n", len(selected))
	for _, vpc := range selected {
		fmt.Fprintf(w, "  - %s (%s)\n", vpc.Name, vpc.ID)
	}
	fmt.Fprintf(w, "Peerings to create (%d):\n", len(plan.create))
	for _, pair := range plan.create {
		fmt.Fprintf(w, "  - %s\n", pair.label())
	}
	fmt.Fprintf(w, "Existing peerings to skip (%d):\n", len(plan.skip))
	for _, pair := range plan.skip {
		fmt.Fprintf(w, "  - %s\n", pair.label())
	}
}

type vpcPeeringFailure struct {
	pair vpcPeeringPair
	err  error
}

func executeVPCPeeringPlan(s *Session, siteID string, plan vpcPeeringPlan, w io.Writer) error {
	created := make([]vpcPeeringPair, 0, len(plan.create))
	failed := make([]vpcPeeringFailure, 0)
	for _, pair := range plan.create {
		body, _ := json.Marshal(map[string]string{
			"siteId": siteID,
			"vpc1Id": pair.first.ID,
			"vpc2Id": pair.second.ID,
		})
		_, _, err := s.Client.Do("POST", apiPath(s, "vpc-peering"), nil, nil, body)
		if err != nil {
			failed = append(failed, vpcPeeringFailure{pair: pair, err: err})
			continue
		}
		created = append(created, pair)
	}
	if len(plan.create) > 0 && s.Cache != nil {
		s.Cache.Invalidate("vpc-peering")
	}

	fmt.Fprintln(w, "VPC peering results:")
	for _, pair := range created {
		fmt.Fprintf(w, "  CREATED %s\n", pair.label())
	}
	for _, pair := range plan.skip {
		fmt.Fprintf(w, "  SKIPPED %s (already exists)\n", pair.label())
	}
	for _, failure := range failed {
		fmt.Fprintf(w, "  FAILED %s: %v\n", failure.pair.label(), failure.err)
	}
	fmt.Fprintf(
		w,
		"Summary: created %d, skipped %d, failed %d\n",
		len(created),
		len(plan.skip),
		len(failed),
	)
	if len(failed) > 0 {
		return fmt.Errorf("%d VPC peering(s) failed", len(failed))
	}
	return nil
}
