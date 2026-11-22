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
 * A URL fragment naming an element inside a tab (e.g. #sku_view) opens that
 * tab and scrolls to the element, so redirects and in-page links can target
 * sections that start hidden.
 */
(function() {
    function slideIndicator(nav, button) {
        const indicator = nav.querySelector('.tab-indicator');
        if (!indicator || !button) return;
        indicator.style.width = button.offsetWidth + 'px';
        indicator.style.transform = 'translateX(' + button.offsetLeft + 'px)';
    }

    function initTabs(nav) {
        function moveIndicator(button) {
            slideIndicator(nav, button);
        }

        // Tab semantics for assistive technology: each button controls the
        // panel named by its data-tab, and aria-selected follows the active tab.
        const tabButtons = Array.from(nav.querySelectorAll('.tab-button[data-tab]'));
        if (tabButtons.length > 0) nav.setAttribute('role', 'tablist');
        tabButtons.forEach(function(button) {
            const panel = document.getElementById('tab-' + button.getAttribute('data-tab'));
            if (!button.id) button.id = 'tab-button-' + button.getAttribute('data-tab');
            button.setAttribute('role', 'tab');
            button.setAttribute('aria-selected', String(button.classList.contains('active')));
            if (panel) {
                button.setAttribute('aria-controls', panel.id);
                panel.setAttribute('role', 'tabpanel');
                panel.setAttribute('aria-labelledby', button.id);
            }
        });

        function setActive(button, keepHash) {
            const currentBtn = nav.querySelector('.tab-button.active');
            if (currentBtn) {
                currentBtn.classList.remove('active');
                currentBtn.setAttribute('aria-selected', 'false');
            }
            button.classList.add('active');
            button.setAttribute('aria-selected', 'true');

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
            // A stale fragment would reopen its own tab on reload.
            if (!keepHash) url.hash = '';
            history.replaceState(null, '', url.toString());
        }

        function hashTarget() {
            let id = '';
            try {
                id = decodeURIComponent(window.location.hash.slice(1));
            } catch (e) {
                return null;
            }
            return id ? document.getElementById(id) : null;
        }

        // The tab button whose panel contains `target`, if this nav owns it.
        function buttonFor(target) {
            const panel = target ? target.closest('.tab-content') : null;
            if (!panel || !panel.id.startsWith('tab-')) return null;
            return nav.querySelector(
                '.tab-button[data-tab="' + CSS.escape(panel.id.slice('tab-'.length)) + '"]');
        }

        // Open the tab containing the fragment's element and point the user
        // at it; false if this nav does not own it.
        function showHashTarget() {
            const target = hashTarget();
            const button = buttonFor(target);
            if (!button) return false;
            if (button.classList.contains('active')) {
                moveIndicator(button);
            } else {
                setActive(button, true);
            }
            target.scrollIntoView({block: 'center'});
            if (target.matches('.config-card, .entity-summary')) {
                target.classList.remove('flash-highlight');
                // Reflow so re-adding the class restarts the animation.
                void target.offsetWidth;
                target.classList.add('flash-highlight');
            }
            return true;
        }

        // In-page links re-show their target even when the URL already ends in
        // that fragment, which fires no hashchange.
        document.addEventListener('click', function(e) {
            const link = e.target.closest('a[href^="#"]');
            if (!link || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
            const id = link.getAttribute('href').slice(1);
            if (!id || !buttonFor(document.getElementById(id))) return;
            e.preventDefault();
            history.replaceState(null, '', '#' + id);
            showHashTarget();
        });

        // Restore the active tab from the URL on initial load so refreshing
        // or following a bookmarked link lands on the same tab.
        const tabFromUrl = new URLSearchParams(window.location.search).get('tab');
        const urlButton = tabFromUrl
            ? nav.querySelector('.tab-button[data-tab="' + CSS.escape(tabFromUrl) + '"]')
            : null;

        // The fragment's tab takes precedence over ?tab=.
        if (!showHashTarget()) {
            if (urlButton && !urlButton.classList.contains('active')) {
                setActive(urlButton);
            } else {
                const initial = nav.querySelector('.tab-button.active')
                    || nav.querySelector('.tab-button');
                if (initial) {
                    moveIndicator(initial);
                }
            }
        }

        nav.addEventListener('click', function(e) {
            const button = e.target.closest('.tab-button');
            if (!button || !nav.contains(button)) return;
            // Link tabs (each a separate server-rendered page) are handled by initLinkTabs.
            if (!button.hasAttribute('data-tab')) return;
            setActive(button);
        });

        window.addEventListener('hashchange', showHashTarget);

        window.addEventListener('resize', function() {
            moveIndicator(nav.querySelector('.tab-button.active'));
        });
    }

    /*
     * Link tabs: <a class="tab-button" href="..."> without data-tab, each tab a
     * separate server-rendered page. A plain click fetches that page and swaps
     * in its [data-tab-swap] regions (matched by id) and tab labels, so the
     * indicator slides instead of the page reloading. Modified clicks, failed
     * loads, and browsers without JS fall back to ordinary navigation.
     */
    function initLinkTabs(nav) {
        const links = Array.from(nav.querySelectorAll('a.tab-button[href]:not([data-tab])'));
        if (links.length === 0 || !document.querySelector('[data-tab-swap]')) return;
        let latestLoad = 0;

        async function load(url, push) {
            const loadId = ++latestLoad;
            const regions = document.querySelectorAll('[data-tab-swap]');
            regions.forEach(function(region) { region.classList.add('tab-loading'); });
            let doc;
            try {
                const response = await fetch(url, {credentials: 'same-origin'});
                if (!response.ok) throw new Error('HTTP ' + response.status);
                doc = new DOMParser().parseFromString(await response.text(), 'text/html');
            } catch (err) {
                // A failed load that a later click superseded must not navigate away.
                if (loadId === latestLoad) window.location.href = url;
                return;
            }
            // A later click superseded this load.
            if (loadId !== latestLoad) return;

            const nextLinks = doc.querySelectorAll('.tab-nav a.tab-button[href]:not([data-tab])');
            const swaps = Array.from(regions, function(region) {
                return [region, doc.getElementById(region.id)];
            });
            if (nextLinks.length !== links.length || swaps.some(function(s) { return !s[1]; })) {
                window.location.href = url;
                return;
            }
            swaps.forEach(function(s) {
                s[0].replaceWith(s[1]);
                if (window.enhanceTables) window.enhanceTables(s[1]);
            });
            links.forEach(function(link, i) {
                link.className = nextLinks[i].className;
                link.textContent = nextLinks[i].textContent;
            });
            document.title = doc.title;
            if (push) history.pushState(null, '', url);
            slideIndicator(nav, nav.querySelector('.tab-button.active'));
        }

        nav.addEventListener('click', function(e) {
            const link = e.target.closest('a.tab-button[href]');
            if (!link || link.hasAttribute('data-tab') || e.button !== 0
                || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) {
                return;
            }
            e.preventDefault();
            if (link.classList.contains('active')) return;
            links.forEach(function(l) { l.classList.toggle('active', l === link); });
            slideIndicator(nav, link);
            load(link.href, true);
        });

        // Back/Forward between tabs restores that tab's content.
        window.addEventListener('popstate', function() {
            load(window.location.href, false);
        });
    }

    document.addEventListener('DOMContentLoaded', function() {
        document.querySelectorAll('.tab-nav').forEach(function(nav) {
            initTabs(nav);
            initLinkTabs(nav);
        });
    });
})();
