const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const source = readFileSync(join(__dirname, '../assets/analytics.js'), 'utf8');

function load(userId = '', previousUser, sdkAvailable = true, webAnalytics = true) {
    const calls = [];
    let script;
    let options;
    const posthog = {
        init(token, config) {
            assert.equal(token, 'phc_test');
            options = config;
            config.loaded(this);
        },
        get_property(name) { assert.equal(name, '$user_id'); return previousUser; },
        reset(resetDevice) { calls.push(['reset', resetDevice]); },
        identify(id) { calls.push(['identify', id]); },
        capture(event) { calls.push(['capture', event]); },
    };
    const context = {
        window: sdkAvailable ? { posthog } : {},
        document: {
            currentScript: { dataset: {
                projectToken: 'phc_test', apiHost: 'https://eu.i.posthog.com',
                sdkUrl: 'https://eu-assets.i.posthog.com/static/array.js', userId,
                webAnalytics: String(webAnalytics),
            } },
            createElement(tag) { assert.equal(tag, 'script'); return {}; },
            head: { appendChild(element) { script = element; } },
        },
    };
    vm.runInNewContext(source, context);
    assert.equal(script.async, true);
    assert.equal(script.src, 'https://eu-assets.i.posthog.com/static/array.js');
    assert.deepEqual(calls, [], 'loading the SDK must not block on tracking');
    script.onload();
    return { calls, options };
}

test('anonymous visits keep their identity and capture exactly one pageview', () => {
    const { calls, options } = load();
    assert.deepEqual(calls, [['capture', '$pageview']]);
    assert.equal(options.api_host, 'https://eu.i.posthog.com');
    assert.equal(options.capture_pageview, false);
    assert.equal(options.capture_pageleave, true);
    assert.equal(options.person_profiles, 'identified_only');
    assert.equal(options.autocapture, false);
    assert.equal(options.capture_exceptions, false);
    assert.equal(options.capture_performance.web_vitals, true);
    assert.equal(options.disable_session_recording, true);
    assert.equal(options.disable_surveys, true);
});

test('identifies before the first signed-in pageview, without resetting anonymous history', () => {
    assert.deepEqual(load('user-a').calls, [['identify', 'user-a'], ['capture', '$pageview']]);
    assert.deepEqual(load('user-a', 'user-a').calls, [['identify', 'user-a'], ['capture', '$pageview']]);
});

test('sign-out resets the identity and device before capturing another pageview', () => {
    assert.deepEqual(load('', 'user-a').calls, [['reset', true], ['capture', '$pageview']]);
});

test('switching accounts resets before identifying the next user', () => {
    assert.deepEqual(load('user-b', 'user-a').calls, [
        ['reset', true], ['identify', 'user-b'], ['capture', '$pageview'],
    ]);
});

test('missing SDK does not throw or affect the page', () => {
    assert.deepEqual(load('', undefined, false).calls, []);
});

test('without web analytics, never emits pageviews, pageleaves or Web Vitals', () => {
    const { calls, options } = load('user-a', undefined, true, false);
    assert.deepEqual(calls, [['identify', 'user-a']]);
    assert.equal(options.capture_pageleave, false);
    assert.equal(options.capture_performance, false);
    assert.equal(options.enable_heatmaps, false);
    assert.equal(options.advanced_disable_feature_flags, true);
});

test('never captures exceptions (PostHog error tracking is not used)', () => {
    assert.equal(load().options.capture_exceptions, false);
    assert.equal(load('', undefined, true, false).options.capture_exceptions, false);
});
