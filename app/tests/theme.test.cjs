const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/views/mod.rs'), 'utf8');
const script = name => source.match(new RegExp(`const ${name}: &str = r#"([\\s\\S]*?)"#;`))[1];

function browser({ saved, dark = false, storageBlocked = false } = {}) {
  const storage = new Map(saved ? [['theme', saved]] : []);
  const buttons = ['light', 'dark', 'system'].map(value => ({
    dataset: { themeValue: value },
    attributes: {},
    addEventListener(event, handler) { this[event] = handler; },
    setAttribute(name, value) { this.attributes[name] = value; },
  }));
  const media = {
    matches: dark,
    addEventListener(event, handler) { this[event] = handler; },
  };
  const document = {
    documentElement: { dataset: {} },
    querySelectorAll(selector) { return selector === '[data-theme-value]' ? buttons : []; },
    addEventListener() {},
  };
  const context = vm.createContext({
    document,
    matchMedia: () => media,
    localStorage: {
      getItem(key) { if (storageBlocked) throw new Error('blocked'); return storage.get(key) ?? null; },
      setItem(key, value) { if (storageBlocked) throw new Error('blocked'); storage.set(key, value); },
      removeItem(key) { if (storageBlocked) throw new Error('blocked'); storage.delete(key); },
    },
  });
  vm.runInContext(script('THEME_BOOT'), context);
  const firstPaint = document.documentElement.dataset.theme || 'light';
  vm.runInContext(script('THEME_TOGGLE'), context);
  return {
    storage,
    firstPaint,
    get theme() { return document.documentElement.dataset.theme; },
    get selected() { return buttons.filter(b => b.attributes['aria-pressed'] === 'true').map(b => b.dataset.themeValue); },
    choose(value) { buttons.find(b => b.dataset.themeValue === value).click(); },
    systemDark(value) { media.matches = value; media.change(); },
  };
}

test('saved themes agree with first paint and persist across page loads', () => {
  const page = browser({ saved: 'dark' });
  assert.equal(page.firstPaint, 'dark');
  assert.equal(page.theme, 'dark');
  assert.deepEqual(page.selected, ['dark']);
  page.choose('light');
  assert.equal(page.theme, 'light');
  assert.equal(page.storage.get('theme'), 'light');
  page.systemDark(true);
  assert.equal(page.theme, 'light', 'explicit choices override system appearance');
  const next = browser({ saved: page.storage.get('theme'), dark: true });
  assert.equal(next.firstPaint, 'light');
  assert.equal(next.theme, 'light');
});

test('system mode clears the override and follows live system appearance changes', () => {
  const page = browser({ saved: 'light', dark: true });
  page.choose('system');
  assert.equal(page.storage.has('theme'), false);
  assert.equal(page.theme, 'dark');
  assert.deepEqual(page.selected, ['system']);
  page.systemDark(false);
  assert.equal(page.theme, 'light');
  const next = browser({ dark: true });
  assert.equal(next.firstPaint, 'dark');
  assert.equal(next.theme, 'dark');
  assert.deepEqual(next.selected, ['system']);
});

test('blocked browser storage does not prevent changing themes', () => {
  const page = browser({ storageBlocked: true, dark: true });
  assert.equal(page.firstPaint, 'dark');
  page.choose('light');
  assert.equal(page.theme, 'light');
  assert.deepEqual(page.selected, ['light']);
  page.choose('dark');
  assert.equal(page.theme, 'dark');
  page.choose('system');
  page.systemDark(false);
  assert.equal(page.theme, 'light');
  assert.deepEqual(page.selected, ['system']);
});

test('invalid saved preferences fall back to the system before first paint', () => {
  const page = browser({ saved: 'invalid', dark: true });
  assert.equal(page.firstPaint, 'dark');
  assert.equal(page.theme, 'dark');
  assert.deepEqual(page.selected, ['system']);
});

test('new browsers default to System for either device appearance without saving an override', () => {
  for (const dark of [false, true]) {
    const page = browser({ dark });
    assert.deepEqual(page.selected, ['system']);
    assert.equal(page.firstPaint, dark ? 'dark' : 'light');
    assert.equal(page.theme, dark ? 'dark' : 'light');
    assert.equal(page.storage.size, 0);
  }
});
