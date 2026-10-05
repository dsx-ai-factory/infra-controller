// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cmd

import (
	"context"
	"fmt"

	"github.com/google/uuid"
	"github.com/spf13/cobra"

	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/client"
	"github.com/dsx-ai-factory/infra-controller/rest-api/flow/pkg/types"
)

var ruleDisassociateCmd = &cobra.Command{
	Use:   "disassociate",
	Short: "Remove rule association from a rack",
	Long:  `Remove the operation rule association from a specific rack for a given operation type.`,
	RunE:  runRuleDisassociate,
}

var (
	disassocRackID    string
	disassocOpType    string
	disassocOperation string
)

func init() {
	ruleCmd.AddCommand(ruleDisassociateCmd)

	ruleDisassociateCmd.Flags().StringVar(&disassocRackID, "rack-id", "", "Rack ID (required)")
	ruleDisassociateCmd.Flags().StringVar(&disassocOpType, "operation-type", "", "Operation type: power_control or firmware_control (required)")
	ruleDisassociateCmd.Flags().StringVar(&disassocOperation, "operation", "", "Operation code: power_on, power_off, upgrade, etc. (required)")

	ruleDisassociateCmd.MarkFlagRequired("rack-id")
	ruleDisassociateCmd.MarkFlagRequired("operation-type")
	ruleDisassociateCmd.MarkFlagRequired("operation")
}

// runRuleDisassociate is the RunE handler for ruleDisassociateCmd. It parses
// the rack ID, operation type, and operation code from flags and calls
// DisassociateRuleFromRack via the client.
func runRuleDisassociate(cmd *cobra.Command, args []string) error {
	var opType types.OperationType
	switch disassocOpType {
	case "power_control":
		opType = types.OperationTypePowerControl
	case "firmware_control":
		opType = types.OperationTypeFirmwareControl
	default:
		return fmt.Errorf("invalid operation type: %s (must be power_control or firmware_control)", disassocOpType)
	}

	rackID, err := uuid.Parse(disassocRackID)
	if err != nil {
		return fmt.Errorf("invalid rack ID: %w", err)
	}

	flowClient, err := client.New(newGlobalClientConfig())
	if err != nil {
		return fmt.Errorf("failed to create client: %w", err)
	}
	defer flowClient.Close()

	err = flowClient.DisassociateRuleFromRack(context.Background(), rackID, opType, disassocOperation)
	if err != nil {
		return fmt.Errorf("failed to disassociate rule from rack: %w", err)
	}

	fmt.Printf("Successfully removed rule association from rack %s for %s/%s\n",
		disassocRackID, disassocOpType, disassocOperation)

	return nil
}
