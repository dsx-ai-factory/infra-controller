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
 * Tab switching with a sliding indicator.
 *
 * Usage in any page that extends base.html:
 *
 *   <div class="tab-nav">
 *       <button class="tab-button active" data-tab="overview">Overview</button>
 *       <button class="tab-button" data-tab="details">Details</button>
 *       <div class="tab-indicator"></div>
 *   </div>
 *   <div class="tab-content active" id="tab-overview"> ... </div>
 *   <div class="tab-content" id="tab-details"> ... </div>
 *
 */
(function() {
    function initTabs(nav) {
        const indicator = nav.querySelector('.tab-indicator');

        function moveIndicator(button) {
            if (!indicator || !button) return;
            indicator.style.width = button.offsetWidth + 'px';
            indicator.style.transform = 'translateX(' + button.offsetLeft + 'px)';
        }

        function setActive(button) {
            const currentBtn = nav.querySelector('.tab-button.active');
            if (currentBtn) currentBtn.classList.remove('active');
            button.classList.add('active');

            const currentContent = document.querySelector('.tab-content.active');
            if (currentContent) currentContent.classList.remove('active');
            const targetId = 'tab-' + button.getAttribute('data-tab');
            const nextContent = document.getElementById(targetId);
            if (nextContent) nextContent.classList.add('active');

            moveIndicator(button);

            // Keep the URL in sync with the active tab so reload / bookmark
            // restore works.
            const url = new URL(window.location.href);
            url.searchParams.set('tab', button.getAttribute('data-tab'));
            history.replaceState(null, '', url.toString());
        }

        // Restore the active tab from the URL on initial load so refreshing
        // or following a bookmarked link lands on the same tab.
        const tabFromUrl = new URLSearchParams(window.location.search).get('tab');
        const urlButton = tabFromUrl
            ? nav.querySelector('.tab-button[data-tab="' + CSS.escape(tabFromUrl) + '"]')
            : null;

        if (urlButton && !urlButton.classList.contains('active')) {
            setActive(urlButton);
        } else {
            const initial = nav.querySelector('.tab-button.active')
                || nav.querySelector('.tab-button');
            if (initial) {
                moveIndicator(initial);
            }
        }

        nav.addEventListener('click', function(e) {
            const button = e.target.closest('.tab-button');
            if (!button || !nav.contains(button)) return;
            setActive(button);
        });

        window.addEventListener('resize', function() {
            moveIndicator(nav.querySelector('.tab-button.active'));
        });
    }

    document.addEventListener('DOMContentLoaded', function() {
        document.querySelectorAll('.tab-nav').forEach(initTabs);
    });
})();
