# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import json
import sys
import os
from unittest.mock import MagicMock, patch

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'plugins'))

from module_utils.client import NicoClient


def make_module(api_url='https://api.example.com', api_token='test-token', org='test-org', api_path_prefix=None):
    module = MagicMock()
    module.params = {
        'api_url': api_url,
        'api_token': api_token,
        'org': org,
        'api_path_prefix': api_path_prefix,
    }
    return module


class TestNicoClientInit:
    def test_url_stripping(self):
        module = make_module(api_url='https://api.example.com/')
        client = NicoClient(module)
        assert client.api_url == 'https://api.example.com'

    def test_org_stored(self):
        module = make_module(org='my-org')
        client = NicoClient(module)
        assert client.org == 'my-org'


class TestNicoClientUrl:
    def test_url_building(self):
        module = make_module()
        client = NicoClient(module)
        url = client._url('/v2/org/{org}/nico/vpc')
        assert url == 'https://api.example.com/v2/org/test-org/nico/vpc'

    def test_url_no_org(self):
        module = make_module()
        client = NicoClient(module)
        url = client._url('/v2/health')
        assert url == 'https://api.example.com/v2/health'

    def test_url_forge_prefix(self):
        module = make_module(api_path_prefix='forge')
        client = NicoClient(module)
        url = client._url('/v2/org/{org}/nico/vpc')
        assert url == 'https://api.example.com/v2/org/test-org/forge/vpc'

    def test_url_forge_prefix_with_item_path(self):
        module = make_module(api_path_prefix='forge')
        client = NicoClient(module)
        url = client._url('/v2/org/{org}/nico/instance/type/{instanceTypeId}')
        assert url == 'https://api.example.com/v2/org/test-org/forge/instance/type/{instanceTypeId}'

    def test_url_default_prefix_unchanged(self):
        module = make_module(api_path_prefix='nico')
        client = NicoClient(module)
        url = client._url('/v2/org/{org}/nico/vpc')
        assert url == 'https://api.example.com/v2/org/test-org/nico/vpc'

    def test_url_none_prefix_defaults_to_nico(self):
        module = make_module(api_path_prefix=None)
        client = NicoClient(module)
        url = client._url('/v2/org/{org}/nico/vpc')
        assert url == 'https://api.example.com/v2/org/test-org/nico/vpc'


class TestNicoClientHeaders:
    def test_headers(self):
        module = make_module(api_token='my-jwt-token')
        client = NicoClient(module)
        headers = client._headers()
        assert headers['Authorization'] == 'Bearer my-jwt-token'
        assert headers['Content-Type'] == 'application/json'
        assert headers['Accept'] == 'application/json'


class TestNicoClientGet:
    @patch('module_utils.client.open_url')
    def test_get_success(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 200
        mock_resp.read.return_value = json.dumps({'id': '123', 'name': 'test'}).encode()
        mock_open_url.return_value = mock_resp

        result = client.get('/v2/org/{org}/nico/vpc/123')
        assert result == {'id': '123', 'name': 'test'}

    @patch('module_utils.client.open_url')
    def test_get_404(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        error = Exception('HTTP Error 404')
        error.code = 404
        error.read = MagicMock(return_value=b'Not found')
        mock_open_url.side_effect = error

        result = client.get('/v2/org/{org}/nico/vpc/nonexistent')
        assert result is None


class TestNicoClientCreate:
    @patch('module_utils.client.open_url')
    def test_create(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        created = {'id': '456', 'name': 'new-vpc', 'status': 'Pending'}
        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 201
        mock_resp.read.return_value = json.dumps(created).encode()
        mock_open_url.return_value = mock_resp

        result = client.create('/v2/org/{org}/nico/vpc', {'name': 'new-vpc'})
        assert result == created

        # Verify the call
        call_args = mock_open_url.call_args
        assert call_args[1]['method'] == 'POST'
        body = json.loads(call_args[1]['data'])
        assert body == {'name': 'new-vpc'}


class TestNicoClientUpdate:
    @patch('module_utils.client.open_url')
    def test_update(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        updated = {'id': '123', 'name': 'updated-vpc'}
        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 200
        mock_resp.read.return_value = json.dumps(updated).encode()
        mock_open_url.return_value = mock_resp

        result = client.update('/v2/org/{org}/nico/vpc/123', {'name': 'updated-vpc'})
        assert result == updated


class TestNicoClientDelete:
    @patch('module_utils.client.open_url')
    def test_delete_204(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 204
        mock_resp.read.return_value = b''
        mock_open_url.return_value = mock_resp

        result = client.delete('/v2/org/{org}/nico/vpc/123')
        assert result is None


class TestNicoClientListAll:
    @patch('module_utils.client.open_url')
    def test_single_page(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        items = [{'id': '1'}, {'id': '2'}]
        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 200
        mock_resp.read.return_value = json.dumps(items).encode()
        mock_resp.headers = {'X-Pagination': json.dumps({'pageNumber': 1, 'pageSize': 100, 'total': 2})}
        mock_open_url.return_value = mock_resp

        result = client.list_all('/v2/org/{org}/nico/vpc')
        assert len(result) == 2
        assert result[0]['id'] == '1'

    @patch('module_utils.client.open_url')
    def test_multi_page(self, mock_open_url):
        module = make_module()
        client = NicoClient(module)

        page1 = [{'id': str(i)} for i in range(100)]
        page2 = [{'id': str(i)} for i in range(100, 150)]

        resp1 = MagicMock()
        resp1.getcode.return_value = 200
        resp1.read.return_value = json.dumps(page1).encode()
        resp1.headers = {'X-Pagination': json.dumps({'pageNumber': 1, 'pageSize': 100, 'total': 150})}

        resp2 = MagicMock()
        resp2.getcode.return_value = 200
        resp2.read.return_value = json.dumps(page2).encode()
        resp2.headers = {'X-Pagination': json.dumps({'pageNumber': 2, 'pageSize': 100, 'total': 150})}

        mock_open_url.side_effect = [resp1, resp2]

        result = client.list_all('/v2/org/{org}/nico/vpc')
        assert len(result) == 150


class TestNicoClientListAllExtended:
    @patch('module_utils.client.open_url')
    def test_proxy_mode_no_pagination_params(self, mock_open_url):
        """Non-nico prefix uses proxy mode: no pageNumber/pageSize in request."""
        module = MagicMock()
        module.params = {
            'api_url': 'https://api.example.com',
            'api_token': 'tok',
            'org': 'myorg',
            'api_path_prefix': 'carbide',
        }
        client = NicoClient(module)

        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 200
        mock_resp.read.return_value = json.dumps([{'id': '1'}, {'id': '2'}]).encode()
        mock_resp.headers = {}
        mock_open_url.return_value = mock_resp

        result = client.list_all('/v2/org/{org}/nico/vpc')

        # Proxy mode: exactly one call, no pagination query params
        assert mock_open_url.call_count == 1
        called_url = mock_open_url.call_args[0][0]
        assert 'pageNumber' not in called_url
        assert 'pageSize' not in called_url
        assert len(result) == 2

    @patch('module_utils.client.open_url')
    def test_proxy_mode_client_side_filter(self, mock_open_url):
        """Proxy mode filters results client-side when params are given."""
        module = MagicMock()
        module.params = {
            'api_url': 'https://api.example.com',
            'api_token': 'tok',
            'org': 'myorg',
            'api_path_prefix': 'carbide',
        }
        client = NicoClient(module)

        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 200
        mock_resp.read.return_value = json.dumps([
            {'id': '1', 'siteId': 'site-a'},
            {'id': '2', 'siteId': 'site-b'},
        ]).encode()
        mock_resp.headers = {}
        mock_open_url.return_value = mock_resp

        result = client.list_all('/v2/org/{org}/nico/vpc', params={'siteId': 'site-a'})

        assert len(result) == 1
        assert result[0]['id'] == '1'

    @patch('module_utils.client.open_url')
    def test_invalid_json_response_calls_fail_json(self, mock_open_url):
        """Non-JSON 200 response must fail the module, not silently return []."""
        module = MagicMock()
        module.params = {
            'api_url': 'https://api.example.com',
            'api_token': 'tok',
            'org': 'myorg',
            'api_path_prefix': 'nico',
        }
        client = NicoClient(module)

        mock_resp = MagicMock()
        mock_resp.getcode.return_value = 200
        mock_resp.read.return_value = b'<html>502 Bad Gateway</html>'
        mock_resp.headers = {}
        mock_open_url.return_value = mock_resp

        client.list_all('/v2/org/{org}/nico/vpc')

        module.fail_json.assert_called_once()
        assert 'invalid JSON' in module.fail_json.call_args[1]['msg'].lower() \
            or 'json' in module.fail_json.call_args[1]['msg'].lower()

    @patch('module_utils.client.open_url')
    def test_post_404_is_not_silent(self, mock_open_url):
        """POST 404 must propagate as fail_json, not return None silently."""
        module = MagicMock()
        module.params = {
            'api_url': 'https://api.example.com',
            'api_token': 'tok',
            'org': 'myorg',
            'api_path_prefix': 'nico',
        }
        client = NicoClient(module)

        error = Exception('HTTP Error 404')
        error.code = 404
        error.read = MagicMock(return_value=b'Not found')
        mock_open_url.side_effect = error

        client.create('/v2/org/{org}/nico/allocation/missing/constraint', {})

        module.fail_json.assert_called_once()

    @patch('module_utils.client.open_url')
    def test_patch_404_is_not_silent(self, mock_open_url):
        """PATCH 404 must propagate as fail_json, not return None silently."""
        module = MagicMock()
        module.params = {
            'api_url': 'https://api.example.com',
            'api_token': 'tok',
            'org': 'myorg',
            'api_path_prefix': 'nico',
        }
        client = NicoClient(module)

        error = Exception('HTTP Error 404')
        error.code = 404
        error.read = MagicMock(return_value=b'Not found')
        mock_open_url.side_effect = error

        client.update('/v2/org/{org}/nico/vpc/missing', {'name': 'x'})

        module.fail_json.assert_called_once()
