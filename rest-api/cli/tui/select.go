// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"errors"
	"fmt"
	"strings"
)

// SelectItem represents one option in a select menu.
type SelectItem struct {
	Label string
	ID    string
	Extra map[string]string
}

const selectWindowSize = 12

var errSelectionCancelled = errors.New("selection cancelled")

// Select displays an interactive arrow-key menu and returns the selected item.
// For large lists it shows a scrolling window of selectWindowSize items.
func Select(label string, items []SelectItem) (selectedItem *SelectItem, err error) {
	if len(items) == 0 {
		return nil, fmt.Errorf("no items to select from")
	}

	restore, err := RawMode()
	if err != nil {
		return nil, err
	}
	defer func() {
		restoreErr := restore()
		if restoreErr != nil {
			if err == nil {
				err = restoreErr
			} else {
				err = errors.Join(err, restoreErr)
			}
		}
	}()

	cursor := 0
	windowStart := 0
	filter := ""
	filtered := filterItems(items, filter)

	prevRenderedLines := 0

	doRender := func() {
		if prevRenderedLines > 0 {
			MoveUp(prevRenderedLines)
			MoveToColumn(1)
			ClearDown()
		}
		prevRenderedLines = renderWindowed(label, filtered, cursor, windowStart, filter)
	}

	doRender()

	for {
		key, err := ReadKey()
		if err != nil {
			ShowCursor()
			return nil, err
		}

		switch {
		case key.Char == KeyCtrlC || key.Char == KeyCtrlD || key.Char == KeyEscape:
			MoveUp(prevRenderedLines)
			MoveToColumn(1)
			ClearDown()
			ShowCursor()
			return nil, errSelectionCancelled

		case key.Special == KeyUp:
			if cursor > 0 {
				cursor--
				if cursor < windowStart {
					windowStart = cursor
				}
			}

		case key.Special == KeyDown:
			if cursor < len(filtered)-1 {
				cursor++
				if cursor >= windowStart+selectWindowSize {
					windowStart = cursor - selectWindowSize + 1
				}
			}

		case key.Char == KeyEnter || key.Char == KeyNewline:
			if len(filtered) > 0 {
				selected := filtered[cursor]
				MoveUp(prevRenderedLines)
				MoveToColumn(1)
				ClearDown()
				ShowCursor()
				fmt.Printf("%s %s\r\n", label, Green(selected.Label))
				return &selected, nil
			}

		case key.Char == KeyBackspace:
			if len(filter) > 0 {
				filter = filter[:len(filter)-1]
				filtered = filterItems(items, filter)
				cursor = 0
				windowStart = 0
			}

		case key.Char >= 32 && key.Char < 127:
			filter += string(key.Char)
			filtered = filterItems(items, filter)
			cursor = 0
			windowStart = 0

		default:
			continue
		}

		if cursor >= len(filtered) {
			cursor = max(0, len(filtered)-1)
		}
		if windowStart > cursor {
			windowStart = cursor
		}

		doRender()
	}
}

// renderWindowed draws a fixed-height window of items and returns the number of lines rendered.
func renderWindowed(label string, items []SelectItem, cursor, windowStart int, filter string) int {
	HideCursor()
	lines := 0

	// Header
	total := len(items)
	if filter != "" {
		fmt.Printf("%s %s\r\n", Bold(label), Dim(fmt.Sprintf("(filter: %s, %d matches)", filter, total)))
	} else {
		fmt.Printf("%s %s\r\n", Bold(label), Dim(fmt.Sprintf("(%d items, type to filter, arrows to move, enter to select)", total)))
	}
	lines++

	if total == 0 {
		fmt.Printf("    %s\r\n", Dim("(no matches)"))
		return lines + 1
	}

	windowEnd := windowStart + selectWindowSize
	if windowEnd > total {
		windowEnd = total
	}

	for i := windowStart; i < windowEnd; i++ {
		if i == cursor {
			fmt.Printf("  %s %s\r\n", Cyan(">"), Reverse(" "+items[i].Label+" "))
		} else {
			fmt.Printf("    %s\r\n", items[i].Label)
		}
		lines++
	}

	// Scroll indicator
	if total > selectWindowSize {
		shown := windowEnd - windowStart
		scrollInfo := fmt.Sprintf("%d-%d of %d", windowStart+1, windowStart+shown, total)
		if windowStart > 0 && windowEnd < total {
			scrollInfo += " (↑↓ scroll)"
		} else if windowStart > 0 {
			scrollInfo += " (↑ more above)"
		} else {
			scrollInfo += " (↓ more below)"
		}
		fmt.Printf("    %s\r\n", Dim(scrollInfo))
		lines++
	}

	return lines
}

func filterItems(items []SelectItem, filter string) []SelectItem {
	if filter == "" {
		return items
	}
	lowerFilter := strings.ToLower(filter)
	var result []SelectItem
	for _, item := range items {
		if strings.Contains(strings.ToLower(item.Label), lowerFilter) {
			result = append(result, item)
		}
	}
	return result
}
