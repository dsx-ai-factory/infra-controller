// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"errors"
	"fmt"
	"io"
	"os"
	"strings"

	"golang.org/x/term"
)

// PromptText displays a label and reads a line of text input.
func PromptText(label string, required bool) (string, error) {
	for {
		fmt.Printf("%s: ", Bold(label))
		input, err := readPromptLine()
		if err != nil {
			return "", err
		}
		text := strings.TrimSpace(input)
		if text == "" && required {
			fmt.Println(Red("  (required)"))
			continue
		}
		return text, nil
	}
}

// PromptSecret reads one line without echo when stdin is a terminal. Piped
// input keeps the normal line-input path so commands remain scriptable and tests
// can supply deterministic input.
func PromptSecret(label string, required bool) (string, error) {
	if !term.IsTerminal(int(os.Stdin.Fd())) {
		return PromptText(label, required)
	}

	for {
		fmt.Printf("%s: ", Bold(label))
		input, err := term.ReadPassword(int(os.Stdin.Fd()))
		fmt.Println()
		if err != nil {
			return "", fmt.Errorf("reading secret input: %w", err)
		}
		text := strings.TrimSpace(string(input))
		if text == "" && required {
			fmt.Println(Red("  (required)"))
			continue
		}
		return text, nil
	}
}

// PromptConfirm displays a y/N confirmation prompt.
func PromptConfirm(label string) (bool, error) {
	fmt.Printf("%s [y/N] ", Bold(label))
	input, err := readPromptLine()
	if err != nil {
		return false, err
	}
	answer := strings.TrimSpace(strings.ToLower(input))
	return answer == "y" || answer == "yes", nil
}

// PromptOptionalBool displays a yes/no prompt where blank means no update.
func PromptOptionalBool(label string) (bool, bool, error) {
	for {
		fmt.Printf("%s [y/n, blank to keep] ", Bold(label))
		input, err := readPromptLine()
		if err != nil {
			return false, false, err
		}
		answer := strings.TrimSpace(strings.ToLower(input))
		switch answer {
		case "":
			return false, false, nil
		case "y", "yes":
			return true, true, nil
		case "n", "no":
			return false, true, nil
		default:
			fmt.Println(Red("  (enter y, n, or leave blank to keep)"))
		}
	}
}

// PromptChoice displays an interactive selector when attached to a terminal.
// Piped input uses a line-oriented prompt so commands remain scriptable and
// tests can supply deterministic input. Input matching is case-insensitive and
// returns the canonical option value. When provided, the default is selected
// initially in the menu and accepted for empty piped input.
func PromptChoice(label string, options []string, defaultValue string) (string, error) {
	if len(options) == 0 {
		return "", fmt.Errorf("no options provided")
	}
	defaultIndex := -1
	if defaultValue != "" {
		for index, opt := range options {
			if strings.EqualFold(defaultValue, opt) {
				defaultIndex = index
				defaultValue = opt
				break
			}
		}
		if defaultIndex < 0 {
			return "", fmt.Errorf("default value %q is not in allowed options %v", defaultValue, options)
		}
	}

	if term.IsTerminal(int(os.Stdin.Fd())) && term.IsTerminal(int(os.Stdout.Fd())) {
		menuOptions := append([]string(nil), options...)
		if defaultIndex > 0 {
			defaultOption := menuOptions[defaultIndex]
			copy(menuOptions[1:defaultIndex+1], menuOptions[:defaultIndex])
			menuOptions[0] = defaultOption
		}
		items := make([]SelectItem, len(menuOptions))
		for index, option := range menuOptions {
			items[index] = SelectItem{
				Label: option,
				ID:    option,
			}
		}
		selected, err := Select(label, items)
		if err != nil {
			return "", err
		}
		return selected.ID, nil
	}

	display := strings.Join(options, "/")
	suffix := fmt.Sprintf("[%s]", display)
	if defaultValue != "" {
		suffix = fmt.Sprintf("[%s, default %s]", display, defaultValue)
	}
	for {
		fmt.Printf("%s %s: ", Bold(label), suffix)
		input, err := readPromptLine()
		if err != nil {
			return "", err
		}
		text := strings.TrimSpace(input)
		if text == "" {
			if defaultValue != "" {
				return defaultValue, nil
			}
			fmt.Println(Red("  (required)"))
			continue
		}
		for _, opt := range options {
			if strings.EqualFold(text, opt) {
				return opt, nil
			}
		}
		fmt.Println(Red(fmt.Sprintf("  (must be one of %s)", display)))
	}
}

// readPromptLine reads exactly one logical line without buffering past its
// newline. Generated forms ask several questions in sequence; using a fresh
// bufio.Scanner per question can consume later piped lines into the first
// scanner's private buffer. Byte-wise reads preserve terminal behavior while
// keeping scripted input and tests deterministic.
func readPromptLine() (string, error) {
	var result strings.Builder
	var one [1]byte
	for {
		n, err := os.Stdin.Read(one[:])
		if n > 0 {
			switch one[0] {
			case '\n':
				return result.String(), nil
			case '\r':
				continue
			default:
				result.WriteByte(one[0])
			}
		}
		if err != nil {
			if errors.Is(err, io.EOF) {
				if result.Len() > 0 {
					return result.String(), nil
				}
				return "", fmt.Errorf("input cancelled")
			}
			return "", fmt.Errorf("reading input: %w", err)
		}
	}
}
