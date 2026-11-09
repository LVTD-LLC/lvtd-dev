const test = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
test('resend countdown uses elapsed time and enables at the deadline', () => {
  const source = fs.readFileSync('app/assets/verification.js', 'utf8');
  const button = { dataset: { resendSeconds: '60' }, disabled: true };
  const status = { textContent: '' };
  let now = 1000, callback;
  vm.runInNewContext(source, { document: { querySelector: s => s === '[data-resend-seconds]' ? button : status }, Date: { now: () => now }, setTimeout: fn => { callback = fn; } });
  assert.equal(button.disabled, true);
  now += 59000; callback();
  assert.equal(button.disabled, true);
  assert.match(status.textContent, /1 seconds/);
  now += 1000; callback();
  assert.equal(button.disabled, false);
  assert.match(status.textContent, /now/);
});
