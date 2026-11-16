/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/*
 * Client-side, per-table search box.
 *
 * Automatically attaches a live filter above every list table
 * (`table.sortable.overview`) that has at least MIN_ROWS data rows. Typing
 * hides rows whose text does not contain every whitespace-separated term
 * (case-insensitive AND match), restoring browser find-style searching that
 * pagination otherwise breaks up across pages. Pairs naturally with the "All"
 * pagination option: choose "All", then filter the whole list at once.
 *
 * Pages that already ship their own row filter for a table can opt that table
 * out (to avoid a duplicate, conflicting control) by adding either the
 * `data-no-auto-filter` attribute or the `no-auto-filter` class to the table.
 *
 * A `<details class="table-filters">` panel of server-side filters placed
 * before the table gets a "Filters" button beside the search box; the panel
 * then opens below that row instead of behind its own summary toggle.
 */
(function () {
	"use strict";

	// Don't clutter short tables that are already easy to scan.
	const MIN_ROWS = 5;

	function findFilterPanel(table) {
		for (let el = table.previousElementSibling; el; el = el.previousElementSibling) {
			if (el.matches("details.table-filters")) {
				// With several tables after one panel, only the first claims it.
				return el.classList.contains("table-filters-enhanced") ? null : el;
			}
		}
		return null;
	}

	function filterToggle(panel) {
		const button = document.createElement("button");
		button.type = "button";
		button.className = "btn-secondary table-filter-toggle";
		button.innerHTML =
			'<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M22 3H2l8 9.46V19l4 2v-8.54L22 3z"/></svg><span>Filters</span>';
		function sync() {
			button.setAttribute("aria-expanded", String(panel.open));
			button.classList.toggle("active", panel.open);
		}
		button.addEventListener("click", function () {
			panel.open = !panel.open;
		});
		panel.addEventListener("toggle", sync);
		panel.classList.add("table-filters-enhanced");
		sync();
		return button;
	}

	function attachFilter(table) {
		const tbody = table.tBodies && table.tBodies[0];
		if (!tbody) {
			return;
		}

		const rows = Array.from(tbody.rows);
		const panel = findFilterPanel(table);
		if (rows.length < MIN_ROWS) {
			if (panel) {
				const wrapper = document.createElement("div");
				wrapper.className = "table-filter";
				wrapper.appendChild(filterToggle(panel));
				panel.before(wrapper);
			}
			return;
		}

		const wrapper = document.createElement("div");
		wrapper.className = "table-filter";

		const input = document.createElement("input");
		input.type = "search";
		input.className = "table-filter-input";
		input.placeholder = "Filter " + rows.length + " rows\u2026";
		input.setAttribute("aria-label", "Filter table rows");
		input.autocomplete = "off";
		input.spellcheck = false;

		const count = document.createElement("span");
		count.className = "table-filter-count";
		// Announce filter results (e.g. "Showing 12 of 500", "No matching rows")
		// to screen readers as the user types.
		count.setAttribute("aria-live", "polite");
		count.setAttribute("aria-atomic", "true");

		const search = document.createElement("div");
		search.className = "table-filter-search";
		search.innerHTML =
			'<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>';
		search.appendChild(input);
		wrapper.appendChild(search);
		if (panel) {
			wrapper.appendChild(filterToggle(panel));
		}
		wrapper.appendChild(count);
		if (panel) {
			panel.before(wrapper);
		} else {
			table.parentNode.insertBefore(wrapper, table);
		}

		const total = rows.length;

		function apply() {
			const query = input.value.trim().toLowerCase();
			const terms = query.length ? query.split(/\s+/) : [];

			let visible = 0;
			for (const row of rows) {
				if (terms.length === 0) {
					row.hidden = false;
					visible++;
					continue;
				}
				const text = row.textContent.toLowerCase();
				const match = terms.every(function (t) {
					return text.indexOf(t) !== -1;
				});
				row.hidden = !match;
				if (match) {
					visible++;
				}
			}

			if (terms.length === 0) {
				count.textContent = "";
				count.classList.remove("table-filter-empty");
			} else if (visible === 0) {
				count.textContent = "No matching rows";
				count.classList.add("table-filter-empty");
			} else {
				count.textContent = "Showing " + visible + " of " + total;
				count.classList.remove("table-filter-empty");
			}
		}

		input.addEventListener("input", apply);
		input.addEventListener("keydown", function (e) {
			if (e.key === "Escape") {
				input.value = "";
				apply();
			}
		});
	}

	// List tables sit in a card that scrolls sideways on its own, so a wide
	// table never widens the page.
	function wrapInCard(table) {
		if (table.parentElement.classList.contains("overview-card")) {
			return;
		}
		const card = document.createElement("div");
		card.className = "overview-card";
		table.before(card);
		card.appendChild(table);
	}

	// Shortened cell content (state names, machine IDs, alert targets) expands
	// when the card has room: try wider machine IDs plus full text, then only the text, and
	// keep whichever widest level still fits without scrolling sideways.
	const FIT_LEVELS = ["fit-full", "fit-text"];

	function fitCard(card) {
		card.classList.remove(...FIT_LEVELS);
		for (const level of FIT_LEVELS) {
			card.classList.add(level);
			if (card.scrollWidth <= card.clientWidth) {
				return;
			}
			card.classList.remove(level);
		}
	}

	function fitAllCards() {
		document.querySelectorAll(".overview-card").forEach(fitCard);
	}

	// Enhances the list tables under `root`. tabs.js calls this again for
	// content it swaps in without a page load.
	function enhanceTables(root) {
		// Skip tables that opt out (pages that already render their own row
		// filter), so we don't add a second, conflicting control.
		root
			.querySelectorAll(
				"table.sortable.overview:not([data-no-auto-filter]):not(.no-auto-filter)"
			)
			.forEach(attachFilter);
		root.querySelectorAll("table.overview").forEach(wrapInCard);
		root.querySelectorAll(".overview-card").forEach(fitCard);
	}
	window.enhanceTables = enhanceTables;

	document.addEventListener("DOMContentLoaded", function () {
		enhanceTables(document);

		let pending = false;
		window.addEventListener("resize", function () {
			if (!pending) {
				pending = true;
				requestAnimationFrame(function () {
					pending = false;
					fitAllCards();
				});
			}
		});
	});
})();
