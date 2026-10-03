// An unclosed block in app.css swallows every rule after it. On 2026-10-03 an
// `@media (max-width: 600px) {` left open by the Messages-tab menu change
// (6b929dae, 2026-10-01) made the whole Map / Location history stylesheet
// phone-only: on a desktop the overview rows lost their height and the
// Settings accordion its styling, while every phone-width e2e stayed green.
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
import { test } from 'node:test';

for (const file of ['app.css', 'state/feedback.css']) {
  test(`${file}: every block is closed`, () => {
    const raw = readFileSync(new URL('../crates/amux-dashboard/static/' + file, import.meta.url), 'utf8');
    // Comments and strings can hold braces; drop them, keeping line numbers.
    const src = raw.replace(/\/\*[\s\S]*?\*\//g, m => m.replace(/[^\n]/g, ' '))
      .replace(/"(?:\\.|[^"\\\n])*"|'(?:\\.|[^'\\\n])*'/g, m => ' '.repeat(m.length));
    const open = [];
    src.split('\n').forEach((line, i) => {
      for (const ch of line) {
        if (ch === '{') open.push(i + 1);
        else if (ch === '}') assert.ok(open.pop() !== undefined, `${file}:${i + 1} closes a block that was never opened`);
      }
    });
    assert.deepEqual(open, [], `${file}: block(s) opened at line(s) ${open.join(', ')} never close, so every later rule is nested inside`);
  });
}
