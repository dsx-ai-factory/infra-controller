# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import sys
import os

# Add plugins to path so module_utils can be imported directly for testing
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'plugins'))

from module_utils.common import camel_to_snake, snake_to_camel, convert_keys


class TestCamelToSnake:
    def test_simple(self):
        assert camel_to_snake('name') == 'name'

    def test_single_word_camel(self):
        assert camel_to_snake('siteId') == 'site_id'

    def test_multi_word(self):
        assert camel_to_snake('instanceTypeId') == 'instance_type_id'

    def test_acronym(self):
        assert camel_to_snake('VPC') == 'vpc'

    def test_acronym_in_middle(self):
        assert camel_to_snake('vpcId') == 'vpc_id'

    def test_nv_link(self):
        assert camel_to_snake('nvLinkLogicalPartitionId') == 'nv_link_logical_partition_id'

    def test_pascal_case(self):
        assert camel_to_snake('InfrastructureProvider') == 'infrastructure_provider'

    def test_consecutive_caps(self):
        assert camel_to_snake('sshKeyGroupIds') == 'ssh_key_group_ids'

    def test_ip_block(self):
        assert camel_to_snake('ipBlockId') == 'ip_block_id'

    def test_already_snake(self):
        assert camel_to_snake('already_snake') == 'already_snake'


class TestSnakeToCamel:
    def test_simple(self):
        assert snake_to_camel('name') == 'name'

    def test_two_words(self):
        assert snake_to_camel('site_id') == 'siteId'

    def test_multi_word(self):
        assert snake_to_camel('instance_type_id') == 'instanceTypeId'

    def test_single_word(self):
        assert snake_to_camel('vpc') == 'vpc'


class TestConvertKeys:
    def test_dict(self):
        data = {'siteId': '123', 'vpcId': '456'}
        result = convert_keys(data, camel_to_snake)
        assert result == {'site_id': '123', 'vpc_id': '456'}

    def test_nested_dict(self):
        data = {'statusHistory': [{'statusMessage': 'ok'}]}
        result = convert_keys(data, camel_to_snake)
        assert result == {'status_history': [{'status_message': 'ok'}]}

    def test_list(self):
        data = [{'siteId': '1'}, {'siteId': '2'}]
        result = convert_keys(data, camel_to_snake)
        assert result == [{'site_id': '1'}, {'site_id': '2'}]

    def test_scalar(self):
        assert convert_keys('hello', camel_to_snake) == 'hello'
        assert convert_keys(42, camel_to_snake) == 42
        assert convert_keys(None, camel_to_snake) is None

    def test_roundtrip(self):
        data = {'site_id': '123', 'vpc_id': '456'}
        camel = convert_keys(data, snake_to_camel)
        assert camel == {'siteId': '123', 'vpcId': '456'}
        back = convert_keys(camel, camel_to_snake)
        assert back == data

    def test_labels_preserved_camel_to_snake(self):
        """Labels keys are user-defined and must not be converted."""
        data = {
            'name': 'my-vpc',
            'labels': {'RackIdentifier': 'GVX11F01C02', 'ServerName': 'srv-01'},
        }
        result = convert_keys(data, camel_to_snake)
        assert result == {
            'name': 'my-vpc',
            'labels': {'RackIdentifier': 'GVX11F01C02', 'ServerName': 'srv-01'},
        }

    def test_labels_preserved_snake_to_camel(self):
        """Labels keys must not be converted when building API payloads."""
        data = {
            'name': 'my-vpc',
            'labels': {'env': 'dev', 'region': 'us-west-1'},
        }
        result = convert_keys(data, snake_to_camel)
        assert result == {
            'name': 'my-vpc',
            'labels': {'env': 'dev', 'region': 'us-west-1'},
        }

    def test_nested_interfaces_converted(self):
        """Non-opaque nested dicts should have keys converted."""
        data = {
            'interfaces': [
                {'subnet_id': 'sub-1', 'is_physical': True},
            ],
        }
        result = convert_keys(data, snake_to_camel)
        assert result == {
            'interfaces': [
                {'subnetId': 'sub-1', 'isPhysical': True},
            ],
        }
