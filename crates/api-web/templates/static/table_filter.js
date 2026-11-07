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
 */
(function () {
	"use strict";

	// Don't clutter short tables that are already easy to scan.
	const MIN_ROWS = 5;

	function attachFilter(table) {
		const tbody = table.tBodies && table.tBodies[0];
		if (!tbody) {
			return;
		}

		const rows = Array.from(tbody.rows);
		if (rows.length < MIN_ROWS) {
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

		wrapper.appendChild(input);
		wrapper.appendChild(count);
		table.parentNode.insertBefore(wrapper, table);

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

	document.addEventListener("DOMContentLoaded", function () {
		// Skip tables that opt out (pages that already render their own row
		// filter), so we don't add a second, conflicting control.
		document
			.querySelectorAll(
				"table.sortable.overview:not([data-no-auto-filter]):not(.no-auto-filter)"
			)
			.forEach(attachFilter);
	});
})();
