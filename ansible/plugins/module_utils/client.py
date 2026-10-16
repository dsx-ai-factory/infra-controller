# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

import json

from ansible.module_utils.six.moves.urllib.parse import quote as urlquote
from ansible.module_utils.urls import open_url


class NicoClient(object):
    """HTTP client for the NVIDIA Infra Controller REST API."""

    def __init__(self, module):
        self.module = module
        self.api_url = module.params['api_url'].rstrip('/')
        self.api_token = module.params['api_token']
        self.org = module.params['org']
        self.path_prefix = module.params.get('api_path_prefix') or 'nico'

    def _url(self, path):
        """Build full URL, substituting {org} placeholder and path prefix."""
        resolved = path.replace('{org}', self.org)
        if self.path_prefix != 'nico':
            resolved = resolved.replace('/nico/', '/%s/' % self.path_prefix, 1)
        return self.api_url + resolved

    def _headers(self):
        return {
            'Authorization': 'Bearer %s' % self.api_token,
            'Content-Type': 'application/json',
            'Accept': 'application/json',
        }

    def _request(self, method, path, data=None, expected_status=None):
        """Make an HTTP request and return (response_body, status_code).

        Returns None for response_body on 204 or empty body.
        Raises an error via fail_json on unexpected status codes.
        """
        url = self._url(path)
        body = json.dumps(data) if data is not None else None

        if expected_status is None:
            expected_status = [200, 201, 202, 204]

        try:
            resp = open_url(
                url,
                method=method,
                headers=self._headers(),
                data=body,
                timeout=30,
            )
            status = resp.getcode()
            raw = resp.read()
        except Exception as e:
            error_body = ''
            status = getattr(e, 'code', None)
            if status is not None:
                try:
                    error_body = e.read().decode('utf-8')
                except Exception:
                    pass
                if status == 404 and method in ('GET', 'DELETE'):
                    return None, 404
                self.module.fail_json(
                    msg='API request %s %s failed with status %d: %s' % (method, url, status, error_body),
                )
            else:
                self.module.fail_json(
                    msg='API request %s %s failed: %s' % (method, url, str(e)),
                )
            # fail_json raises SystemExit in Ansible; this is a safety net.
            return None, status

        if status not in expected_status:
            self.module.fail_json(
                msg='API request %s %s returned unexpected status %d: %s' % (
                    method, url, status,
                    raw.decode('utf-8', errors='replace') if isinstance(raw, bytes) else raw,
                ),
            )
            return None, status

        if status == 204 or not raw:
            return None, status

        try:
            return json.loads(raw), status
        except (ValueError, TypeError):
            return raw, status

    def get(self, path):
        """GET a resource. Returns the resource dict or None if 404."""
        body, _status = self._request('GET', path)
        return body

    def list_all(self, path, params=None):
        """GET a collection with automatic pagination.

        Returns the complete list of resources across all pages.
        When accessing through the NVIDIA proxy (path_prefix != 'nico'),
        query parameters and pagination are skipped because the proxy
        rejects unknown parameters.  Client-side filtering is applied
        after fetching results.
        """
        results = []
        direct_api = (self.path_prefix == 'nico')
        page = 1
        page_size = 100

        while True:
            query_params = {}
            if direct_api:
                query_params.update(params or {})
                query_params['pageNumber'] = page
                query_params['pageSize'] = page_size

            query_parts = []
            for k, v in sorted(query_params.items()):
                if v is None:
                    continue
                encoded_key = urlquote(str(k), safe='')
                # List values become repeated query parameters (e.g. siteId=a&siteId=b)
                values = v if isinstance(v, list) else [v]
                for item in values:
                    query_parts.append('%s=%s' % (encoded_key, urlquote(str(item), safe='')))
            query_string = '&'.join(query_parts)
            full_path = '%s?%s' % (path, query_string) if query_string else path

            url = self._url(full_path)
            try:
                resp = open_url(
                    url,
                    method='GET',
                    headers=self._headers(),
                    timeout=30,
                )
                status = resp.getcode()
                raw = resp.read()
            except Exception as e:
                error_body = ''
                error_status = getattr(e, 'code', None)
                if error_status is not None:
                    try:
                        error_body = e.read().decode('utf-8')
                    except Exception:
                        pass
                self.module.fail_json(
                    msg='API list request %s failed: %s%s' % (
                        url, str(e),
                        ' - %s' % error_body if error_body else '',
                    ),
                )
                return results

            if status != 200:
                self.module.fail_json(
                    msg='API list request %s returned status %d' % (url, status),
                )
                return results

            try:
                items = json.loads(raw)
            except (ValueError, TypeError) as e:
                self.module.fail_json(
                    msg='API list request %s returned invalid JSON: %s' % (url, str(e)),
                )
                return results

            if not isinstance(items, list):
                items = [items]

            results.extend(items)

            if not direct_api:
                break

            # Check pagination header
            pagination_header = resp.headers.get('X-Pagination') or ''
            if pagination_header:
                try:
                    pagination = json.loads(pagination_header)
                    total = pagination.get('total', 0)
                    if page * page_size >= total:
                        break
                except (ValueError, TypeError):
                    break
            else:
                break

            page += 1

        # When using the proxy, filter results client-side since query
        # parameters cannot be forwarded.
        if not direct_api and params:
            for key, value in params.items():
                if value is not None:
                    results = [r for r in results if r.get(key) == value]

        return results

    def create(self, path, data):
        """POST to create a resource. Returns the created resource."""
        body, _status = self._request('POST', path, data=data, expected_status=[200, 201, 202])
        return body

    def update(self, path, data):
        """PATCH to update a resource. Returns the updated resource."""
        body, _status = self._request('PATCH', path, data=data, expected_status=[200, 202, 204])
        return body

    def put(self, path, data):
        """PUT to replace a resource. Returns the updated resource."""
        body, _status = self._request('PUT', path, data=data, expected_status=[200, 201, 202])
        return body

    def delete(self, path, data=None):
        """DELETE a resource."""
        body, _status = self._request('DELETE', path, data=data, expected_status=[200, 202, 204])
        return body
