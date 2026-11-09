const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');
const source = readFileSync(join(__dirname, '../assets/sentry.js'), 'utf8');

const capabilities = ["errors", "traces", "logs", "metrics", "replay"];

async function load({ enabled = true, profilingFails = false, sdkMissing = false, selected = capabilities } = {}) {
  let element, options, replay, processor;
  const calls = [];
  const Sentry = {
    browserTracingIntegration: () => ({ name: "traces" }),
    replayIntegration: config => { replay = config; return { name: "replay" }; },
    lazyLoadIntegration: async () => { calls.push("profiling"); if (profilingFails) throw Error('blocked'); return () => ({}); },
    addEventProcessor: callback => { processor = callback; },
    withStaticSpan: callback => callback,
    init: config => { options = config; },
    logger: { info: message => calls.push(message) },
    metrics: { count: name => calls.push(name) },
  };
  vm.runInNewContext(source, {
    window: { Sentry: sdkMissing ? undefined : Sentry, Profiler: function () {}, location: { origin: 'https://example.test' } },
    document: {
      currentScript: { dataset: enabled ? { dsn: 'https://public@example.com/1', environment: 'test', release: 'test', tracesRate: '1', replayRate: '0.25', profileRate: '0.25', ...Object.fromEntries(capabilities.map(key => [key, String(selected.includes(key))])) } : {} },
      createElement: () => ({}), head: { appendChild: value => { element = value; } },
    },
  });
  if (element) await element.onload();
  return { element, options, replay, calls, processor };
}

test('disabled SDK does not load; blocked SDK/profiling cannot break the site', async () => {
  assert.equal((await load({ enabled: false })).element, undefined);
  assert.equal((await load({ sdkMissing: true })).options, undefined);
  assert.ok((await load({ profilingFails: true })).options);
});

test('configures masked replay, profiling and restricted trace propagation', async () => {
  const { element, options, replay, calls } = await load();
  assert.ok(element.integrity.startsWith('sha384-'));
  assert.equal(options.dataCollection.userInfo, false);
  assert.equal(options.dataCollection.cookies, false);
  assert.equal(options.dataCollection.httpHeaders, false);
  assert.equal(options.dataCollection.httpBodies.length, 0);
  assert.equal(options.dataCollection.urlQueryParams, false);
  assert.equal(options.tracesSampleRate, 1);
  assert.equal(options.profileLifecycle, 'trace');
  assert.equal(options.replaysOnErrorSampleRate, 1);
  assert.equal(replay.maskAllText, true);
  assert.equal(replay.maskAllInputs, true);
  assert.equal(replay.blockAllMedia, true);
  for (const url of ['https://example.test/items/new', '/sign-in']) {
    assert.ok(options.tracePropagationTargets.some(pattern => pattern.test(url)));
  }
  for (const url of ['https://example.test.evil.com/', '//external.example/', 'https://us.i.posthog.com/']) {
    assert.ok(options.tracePropagationTargets.every(pattern => !pattern.test(url)));
  }
  assert.ok(calls.includes('browser.page_load'));
});

test('removes sensitive URL components and request data from telemetry', async () => {
  const { options, processor } = await load();
  assert.equal(processor({ type: "replay_event", urls: ["/?secret=1"] }).urls[0], "/");
  const input = { request: { url: 'https://example.test/items/01900000-1234-7123-8123-123456789abc?secret=password#private', headers: { Cookie: 'secret' }, data: 'password' } };
  assert.equal(JSON.stringify(options.beforeSend(input)), '{"request":{"url":"https://example.test/items/:id"}}');
  assert.equal(options.beforeBreadcrumb({ category: 'console' }), null);
  const crumb = options.beforeBreadcrumb({ category: 'fetch', data: { url: '/?secret=1' } });
  assert.equal(crumb.data.url, '/');
  const span = options.beforeSendSpan({ description: 'GET /?secret=1', data: { 'url.full': '/?secret=1', 'url.query': 'secret=1' } });
  assert.equal(span.description, 'GET /');
  assert.equal(span.data['url.query'], undefined);
});


test('all capability subsets load only selected instrumentation and filter unselected payloads', async () => {
  for (let mask = 0; mask < 32; mask++) {
    const selected = capabilities.filter((_, index) => mask & (1 << index));
    const { element, options, calls, replay } = await load({ selected });
    if (!selected.length) {
      assert.equal(element, undefined);
      continue;
    }
    const has = key => selected.includes(key);
    assert.equal(options.defaultIntegrations, has('errors') ? undefined : false);
    assert.equal(options.beforeSend({ message: 'synthetic error' }) !== null, has('errors'));
    assert.equal(options.beforeSendTransaction({ transaction: '/test' }) !== null, has('traces'));
    assert.equal(options.beforeSendLog({ body: 'synthetic log' }) !== null, has('logs'));
    assert.equal(options.beforeSendMetric({ name: 'synthetic metric' }) !== null, has('metrics'));
    assert.equal(options.integrations.some(integration => integration.name === 'traces'), has('traces'));
    assert.equal(Boolean(replay), has('replay'));
    assert.equal(options.tracesSampleRate, has('traces') ? 1 : 0);
    assert.equal(options.tracePropagationTargets.length > 0, has('traces'));
    assert.equal(options.profileSessionSampleRate, has('traces') ? 0.25 : 0);
    assert.equal(calls.includes('profiling'), has('traces'));
    assert.equal(options.replaysSessionSampleRate, has('replay') ? 0.25 : 0);
    assert.equal(options.replaysOnErrorSampleRate, has('replay') && has('errors') ? 1 : 0);
    assert.equal(calls.includes('browser telemetry initialized'), has('logs'));
    assert.equal(calls.includes('browser.page_load'), has('metrics'));
  }
});
