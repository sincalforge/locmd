'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../frontend/agent-call-utils.js'), 'utf8');
const context = vm.createContext({ TextDecoder, TextEncoder, ReadableStream, Uint8Array });
vm.runInContext(source, context, { filename: 'agent-call-utils.js' });
const utils = context.AgentCallUtils;

function streamFromBytes(bytes, chunkSize = 5) {
  return new ReadableStream({
    start(controller) {
      for (let i = 0; i < bytes.length; i += chunkSize) controller.enqueue(bytes.slice(i, i + chunkSize));
      controller.close();
    },
  });
}

test('valid tool arguments preserve file content containing escaped quotes and newlines', () => {
  const raw = JSON.stringify({ path: 'index.html', content: '<button title="Focus">\nstartTimer();\n' });
  const parsed = utils.parseToolArguments(raw);
  assert.equal(parsed.ok, true);
  assert.equal(JSON.stringify(parsed.value), raw);
  assert.equal(JSON.parse(parsed.serialized).content, '<button title="Focus">\nstartTimer();\n');
});

test('malformed tool arguments are represented as safe JSON in assistant history', () => {
  const calls = utils.prepareToolCalls([
    { id: 'call_bad', name: 'create_file', args: '{"path":"index.html","content":"const label = "Focus";' },
  ], 3);
  assert.equal(calls[0].argsValid, false);
  assert.equal(calls[0].historyArgs, '{}');
  assert.deepEqual(JSON.parse(calls[0].historyArgs), {});
});

test('legacy malformed tool-call history is sanitized before retry', () => {
  const messages = [{
    role: 'assistant',
    tool_calls: [{ id: 'call_old', type: 'function', function: { name: 'write_file', arguments: '{"path":"x","content":"unterminated' } }],
  }];
  assert.equal(utils.sanitizeToolCallHistory(messages), 1);
  assert.equal(messages[0].tool_calls[0].function.arguments, '{}');
  assert.deepEqual(JSON.parse(messages[0].tool_calls[0].function.arguments), {});
});

test('tool argument parser failure is not classified as unsupported tools and retries at most once', () => {
  const error = 'Failed to parse tool call arguments as JSON ... invalid string: mi at column 14871';
  assert.equal(utils.classifyToolCallError(error), 'invalid_arguments');
  assert.equal(utils.shouldRetryToolCallError(error, 0), true);
  assert.equal(utils.shouldRetryToolCallError(error, 1), false);
});

test('genuine tool-call capability errors remain distinguishable', () => {
  const error = 'This model does not support tool calls';
  assert.equal(utils.classifyToolCallError(error), 'unsupported');
  assert.equal(utils.shouldRetryToolCallError(error, 0), false);
});

test('SSE reader handles split UTF-8/JSON chunks and dispatches a final event without a newline', async () => {
  const rawArgs = JSON.stringify({ content: '番茄钟\nline', ok: true });
  const split = rawArgs.indexOf('钟') + '钟'.length;
  const first = `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: rawArgs.slice(0, split) } }] } }] })}\n\n`;
  const last = `data: ${JSON.stringify({ choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: rawArgs.slice(split) } }] } }] })}`;
  const bytes = new TextEncoder().encode(first + last);
  const events = [];
  await utils.consumeSseJson(streamFromBytes(bytes, 3).getReader(), (event) => events.push(event));
  assert.equal(events.length, 2);
  const args = events.map((event) => event.choices[0].delta.tool_calls[0].function.arguments).join('');
  assert.equal(JSON.parse(args).content, '番茄钟\nline');
  assert.equal(JSON.parse(args).ok, true);
});

test('SSE reader reports malformed data instead of silently dropping the event', async () => {
  const bytes = new TextEncoder().encode('data: {"choices": nope}\n\n');
  await assert.rejects(
    () => utils.consumeSseJson(streamFromBytes(bytes).getReader(), () => {}),
    /SSE 数据不是有效 JSON/
  );
});
