// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { createContext, runInContext } from 'node:vm';

const template = readFileSync(new URL('ui_templates/switches.html', import.meta.url), 'utf8');
const script = template.match(/<script>([\s\S]*?)<\/script>/)[1];

function registrationPage(response = { responses: [{ uuid: 'switch-1', isNew: true }] }) {
    const inputs = Object.fromEntries(['bmc-ip', 'bmc-mac', 'nvos-ip', 'nvos-mac', 'rack-id']
        .map(name => [`.sw-${name}`, { value: '', dataset: {} }]));
    const entry = { querySelector: selector => inputs[selector] };
    const requests = [];
    const messages = [];
    const navigation = [];
    const context = createContext({
        document: {
            getElementById: () => ({ addEventListener() {} }),
            querySelectorAll: () => [entry],
        },
        window: {},
        location: { reload: () => navigation.push('reload') },
        closeModal: id => navigation.push(`close ${id}`),
        showToast: (message, error) => messages.push({ message, error }),
        fetch: async (url, options) => {
            requests.push({ url, ...options, body: JSON.parse(options.body) });
            return { ok: true, json: async () => response };
        },
    });
    runInContext(script, context);
    const entryHTML = context.createSwitchEntryHTML(0);
    for (const [selector, input] of Object.entries(inputs)) {
        const tag = entryHTML.match(new RegExp(`<input\\b[^>]*class="${selector.slice(1)}"[^>]*>`))[0];
        const handler = tag.match(/\boninput="([^"]*)"/)?.[1];
        input.closest = () => entry;
        if (handler) input.oninput = runInContext(`(function() { ${handler} })`, context);
    }
    return {
        inputs, requests, messages, navigation,
        input(type, field, value) {
            const input = inputs[`.sw-${type}-${field}`];
            input.value = value;
            input.oninput?.();
        },
        submit: () => context.submitBatchRegister(),
    };
}

test('submitBatchRegister', async t => {
    const cases = [
        {
            name: 'IPv4 autofill',
            bmc: { ip: '192.0.2.10', mac: '02:00:c0:00:02:0a' },
            nvos: { ip: '192.0.2.11', mac: '02:01:c0:00:02:0b' },
        },
        {
            name: 'IPv6 with entered MAC addresses',
            bmc: { ip: '2001:db8::10', mac: '02:aa:00:00:00:10' },
            nvos: { ip: '2001:db8::11', mac: '02:bb:00:00:00:11' },
            explicitMACs: true,
        },
        {
            name: 'entered IPv4 MAC addresses override autofill',
            bmc: { ip: '192.0.2.10', mac: '02:aa:00:00:00:10' },
            nvos: { ip: '192.0.2.11', mac: '02:bb:00:00:00:11' },
            explicitMACs: true,
        },
    ];
    for (const scenario of cases) {
        await t.test(scenario.name, async () => {
            const page = registrationPage();
            for (const type of ['bmc', 'nvos']) {
                page.input(type, 'ip', scenario[type].ip);
                if (scenario.explicitMACs) page.input(type, 'mac', scenario[type].mac);
            }
            page.inputs['.sw-rack-id'].value = 'rack-a';
            await page.submit();
            assert.equal(page.requests.length, 1);
            const request = page.requests[0];
            assert.equal(request.url, '/api/register-switch');
            assert.equal(request.method, 'POST');
            assert.equal(request.headers['Content-Type'], 'application/json');
            assert.deepEqual(request.body, {
                switches: [{ bmc: scenario.bmc, nvos: scenario.nvos, rack_id: 'rack-a' }],
            });
        });
    }

    await t.test('IPv6 missing a MAC does not submit', async () => {
        const page = registrationPage();
        page.input('bmc', 'ip', '2001:db8::10');
        page.input('nvos', 'ip', '2001:db8::11');
        page.input('nvos', 'mac', '02:aa:00:00:00:11');
        await page.submit();
        assert.equal(page.requests.length, 0);
        assert.deepEqual(page.messages, [{
            message: 'Provide BMC and NVOS MAC addresses, or valid IPv4 addresses for autofill.',
            error: true,
        }]);
    });

    await t.test('registration errors preserve the form', async () => {
        const page = registrationPage({ responses: [{ status: 1, error: 'Invalid BMC: invalid MAC address' }] });
        page.input('bmc', 'ip', '2001:db8::10');
        page.input('bmc', 'mac', 'not-a-mac');
        page.input('nvos', 'ip', '2001:db8::11');
        page.input('nvos', 'mac', '02:aa:00:00:00:11');
        await page.submit();
        assert.equal(page.requests.length, 1);
        assert.deepEqual(page.messages, [{ message: 'Switch 1: Invalid BMC: invalid MAC address', error: true }]);
        assert.deepEqual(page.navigation, []);
        assert.equal(page.inputs['.sw-bmc-mac'].value, 'not-a-mac');
    });
});

test('updateGeneratedMac', async t => {
    const cases = [
        { name: 'IPv4 edits refresh autofill', nextIPs: ['192.0.2.12'], expectedMAC: '02:00:c0:00:02:0c' },
        { name: 'IPv6 clears the generated IPv4 MAC', nextIPs: ['2001:db8::10'], expectedMAC: '' },
        {
            name: 'retyping the generated MAC preserves it when the IP changes',
            nextIPs: ['2001:db8::10'],
            enteredMAC: '02:00:c0:00:02:0a', expectedMAC: '02:00:c0:00:02:0a',
        },
        {
            name: 'successive IP edits preserve an entered MAC that matches autofill',
            nextIPs: ['192.0.2.12', '2001:db8::10'],
            enteredMAC: '02:00:c0:00:02:0c', expectedMAC: '02:00:c0:00:02:0c',
        },
    ];
    for (const scenario of cases) {
        await t.test(scenario.name, () => {
            const page = registrationPage();
            page.input('bmc', 'ip', '192.0.2.10');
            assert.equal(page.inputs['.sw-bmc-mac'].value, '02:00:c0:00:02:0a');
            if (scenario.enteredMAC) page.input('bmc', 'mac', scenario.enteredMAC);
            for (const ip of scenario.nextIPs) page.input('bmc', 'ip', ip);
            assert.equal(page.inputs['.sw-bmc-mac'].value, scenario.expectedMAC);
        });
    }
});
