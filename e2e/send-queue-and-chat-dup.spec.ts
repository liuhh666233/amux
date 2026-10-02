// 2026-10-01, two message-delivery defects from Ethan's phone:
// - MSG-72091: sticky Queue mode read "Queue" on an IDLE worker and the message
//   waited. A queued message to an idle worker is now delivered at once by the
//   server, so the button says what a press does: "Send" while idle, "Queue"
//   only while the worker is busy.
// - "chat creates a duplicate message": the Chat tab showed the server's copy
//   and the outbox's pending copy of one send. They are reconciled by msg_id.
// No real worker is messaged: state is set in the page and nothing is sent.
import { test, expect } from './fixtures';

async function boot(page: any) {
  await page.addInitScript(() => localStorage.setItem('amux_walkthrough_done', '1'));
  await page.goto('/');
  await page.waitForFunction(() => typeof (window as any)._sendLabelFor === 'function'
    && typeof (window as any)._chatRender === 'function');
}

test('Queue mode labels an idle worker Send and a busy one Queue', async ({ page }) => {
  await boot(page);
  const r = await page.evaluate(() => {
    const g = globalThis as any;
    const list = g.eval('sessions');
    list.push({ name: 'sq-idle', status: 'idle', running: true }, { name: 'sq-busy', status: 'active', running: true });
    g.eval('_sendMode = "queue"');
    const out = { idle: g._sendLabelFor('sq-idle'), busy: g._sendLabelFor('sq-busy'),
      idleTitle: g._sendTitleFor('sq-idle'), busyTitle: g._sendTitleFor('sq-busy') };
    g.eval('_sendMode = "send"');
    out['sendMode'] = g._sendLabelFor('sq-busy');
    return out;
  });
  expect(r.idle).toBe('Send');
  expect(r.idleTitle).toMatch(/idle, so it is delivered now/);
  expect(r.busy).toBe('Queue');
  expect(r.busyTitle).toMatch(/finishes its current turn/);
  expect(r.sendMode).toBe('Send');
});

test('a Chat send recorded by the server shows once, matched by msg_id', async ({ page }) => {
  await boot(page);
  const count = (recordedId: string) => page.evaluate((recordedId: string) => {
    const g = globalThis as any;
    g.eval('_chatShowing = () => true');
    const chat = g.eval('_chat');
    chat.name = 'dup-probe@chat';
    chat.messages = [{ role: 'user', text: 'is gs12 on pace?', ts: Date.now() / 1000, turn_id: 't1', msg_id: recordedId }];
    chat.streaming = null;
    const q = g.eval('offlineQueue');
    q.splice(0, Infinity, { id: 'op1', url: '/api/sessions/dup-probe%40chat/send', timestamp: Date.now(),
      options: { method: 'POST', body: JSON.stringify({ text: 'is gs12 on pace?', msg_id: 'm-1' }) } });
    g._chatRender();
    const n = document.querySelectorAll('#peek-body .chat-pending').length;
    q.splice(0, Infinity);
    return n;
  }, recordedId);
  expect(await count('m-1'), 'the same msg_id is one message, not two').toBe(0);
  expect(await count('m-other'), 'a different send stays visible as pending').toBe(1);
});

// AMUX-5356: a Chat-tab send goes to `<worker>@chat`, but the composer and its
// attachments belong to the open worker. The cleanup keyed on the send target,
// so attachments sent from the Chat tab stayed in the composer to be sent again.
test('a Chat-tab send clears the attachments it sent', async ({ page }) => {
  await boot(page);
  const left = await page.evaluate(async () => {
    const g = globalThis as any;
    g.eval('peekSession = "att-w"');
    g.eval('_peekChatTarget = () => "att-w@chat"');
    g.eval('doSend = async () => "sent"');
    g.eval('_cancelUpload = () => true');
    g.eval('peekFiles = [{ path: "/tmp/att-probe.txt", name: "att-probe.txt" }]');
    (document.getElementById('peek-cmd-input') as HTMLTextAreaElement).value = 'see attached';
    await g.sendPeekCmd();
    return g.eval('peekFiles').length;
  });
  expect(left, 'the sent attachment must leave the composer').toBe(0);
});
