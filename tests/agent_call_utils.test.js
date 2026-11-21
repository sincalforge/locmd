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

test('make_dir cannot consume an HTML file path during a workspace task', () => {
  const error = utils.workspaceToolIntentError('make_dir', 'D:\\workspace\\index.html', true);
  assert.match(error, /不能用 make_dir 创建文件夹/);
  assert.match(error, /create_file/);
  assert.equal(utils.workspaceToolIntentError('make_dir', 'assets', true), '');
  assert.equal(utils.workspaceToolIntentError('make_dir', 'index.htm', false), '');
  assert.equal(utils.workspaceToolIntentError('read_file', 'index.html', true), '');
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

const XML_DEFS = {
  get_weather: { params: { city: { type: 'string' }, date: { type: 'string' } }, required: ['city', 'date'] },
  write_note: { params: { path: { type: 'string' }, content: { type: 'string' }, count: { type: 'integer' }, flags: { type: 'object' } }, required: ['path', 'content'] },
};

test('MiniCPM5 XML fixture parses entities, CDATA, escaped quotes and newlines', () => {
  const source = '前言 <function name="get_weather"><param name="city">北京 &amp; 朝阳</param><param name="date"><![CDATA[2024-06-27\n晚上]]></param></function> 后文';
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(parsed.text, '前言  后文');
  assert.equal(parsed.calls.length, 1);
  assert.equal(parsed.calls[0].id, '');
  assert.equal(parsed.calls[0].name, 'get_weather');
  assert.deepEqual(JSON.parse(parsed.calls[0].args), { city: '北京 & 朝阳', date: '2024-06-27\n晚上' });
});

test('MiniCPM5 XML supports multiple calls and safe non-string JSON/Python scalars', () => {
  const source = '<function name="write_note"><param name="path">notes.txt</param><param name="content"><![CDATA[title="Focus"\nline]]></param><param name="count">2</param><param name="flags">{"ok": True, "items": [False, None]}</param></function>!<function name="write_note"><param name="path">b.txt</param><param name="content">B</param></function>';
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(parsed.calls.length, 2);
  assert.deepEqual(JSON.parse(parsed.calls[0].args), { path: 'notes.txt', content: 'title="Focus"\nline', count: 2, flags: { ok: true, items: [false, null] } });
  assert.deepEqual(JSON.parse(parsed.calls[1].args), { path: 'b.txt', content: 'B' });
  assert.equal(parsed.text, '!');
});

test('MiniCPM5 non-string parameters accept safe Python strings, lists, tuples, and dictionaries', () => {
  const source = `<function name="write_note"><param name="path">notes.txt</param><param name="content">x</param><param name="flags">{'ok': True, 'items': ["it's", 'a'], 'pair': (1, 2), 'empty': None}</param></function>`;
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(parsed.calls.length, 1);
  assert.deepEqual(JSON.parse(parsed.calls[0].args).flags, { ok: true, items: ["it's", 'a'], pair: [1, 2], empty: null });
});

test('CDATA is not entity-decoded and malformed numeric entities cannot throw', () => {
  const source = '<function name="write_note"><param name="path">a</param><param name="content"><![CDATA[&amp;]]></param></function><function name="write_note"><param name="path">b</param><param name="content">x&#x110000;</param></function>';
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(JSON.parse(parsed.calls[0].args).content, '&amp;');
  assert.equal(JSON.parse(parsed.calls[1].args).content, 'x&#x110000;');
  assert.equal(parsed.text, '');
});

test('CDATA parameter text containing a closing-tag string is retained intact', () => {
  const content = '<div title="</param>">ready</div>';
  const source = `<function name="write_note"><param name="path">page.html</param><param name="content"><![CDATA[${content}]]></param></function>`;
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(parsed.calls.length, 1);
  assert.equal(JSON.parse(parsed.calls[0].args).content, content);
});

test('invalid, duplicate, unknown and missing XML parameters are rejected without leaking function blocks', () => {
  const source = [
    '<function name="write_note"><param name="path">a</param><param name="path">b</param><param name="content">x</param></function>',
    '<function name="write_note"><param name="path">a</param><param name="unknown">x</param><param name="content">x</param></function>',
    '<function name="get_weather"><param name="city">北京</param></function>',
    '<function name="no_such"><param name="x">x</param></function>',
  ].join('ordinary');
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(parsed.calls.length, 0);
  assert.equal(parsed.text, 'ordinaryordinaryordinary');
});

test('XML-looking text inside fenced code is preserved while ordinary XML is extracted', () => {
  const source = '```xml\n<function name="get_weather"><param name="city">示例</param><param name="date">今天</param></function>\n```\n<function name="get_weather"><param name="city">北京</param><param name="date">今天</param></function>';
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.match(parsed.text, /```xml[\s\S]*<function name="get_weather">/);
  assert.equal(parsed.calls.length, 1);
  assert.deepEqual(JSON.parse(parsed.calls[0].args), { city: '北京', date: '今天' });
});

test('XML-looking text inside an unterminated fenced code block remains inert', () => {
  const source = '```xml\n<function name="get_weather"><param name="city">北京</param><param name="date">今天</param></function>';
  const parsed = utils.extractMiniCpmXmlToolCalls(source, XML_DEFS);
  assert.equal(parsed.text, source);
  assert.equal(parsed.calls.length, 0);
});

test('XML-looking inline code remains inert while ordinary XML is extracted', () => {
  const inline = '`<function name="get_weather"><param name="city">example</param><param name="date">today</param></function>`';
  const ordinary = '<function name="get_weather"><param name="city">北京</param><param name="date">今天</param></function>';
  const parsed = utils.extractMiniCpmXmlToolCalls(`${inline} ${ordinary}`, XML_DEFS);
  assert.equal(parsed.text, `${inline} `);
  assert.equal(parsed.calls.length, 1);
});

test('structured and XML tool-call formats merge without duplicating the same call', () => {
  const structured = [{ id: 'json-1', name: 'write_note', args: JSON.stringify({ path: 'a.txt', content: 'A', count: 1 }) }];
  const xml = utils.extractMiniCpmXmlToolCalls('<function name="write_note"><param name="path">a.txt</param><param name="content">A</param><param name="count">1</param></function>', XML_DEFS).calls;
  const merged = utils.mergeToolCalls(structured, xml);
  assert.equal(merged.length, 1);
  assert.equal(merged[0].id, 'json-1');
});

test('Agent SSE loop feeds content XML into the native parser and merges calls', () => {
  const agentSource = fs.readFileSync(path.join(__dirname, '../frontend/agent.js'), 'utf8');
  assert.match(agentSource, /extractMiniCpmXmlToolCalls\(text, tools\)/);
  assert.match(agentSource, /mergeToolCalls\(structuredCalls, xml\.calls\)/);
  assert.match(agentSource, /locmd\.captureAgentStream/);
});

test('Agent page enforces HTML write-before-open and rechecks deliverables before completion', () => {
  const pageSource = fs.readFileSync(path.join(__dirname, '../frontend/agentpage.js'), 'utf8');
  assert.match(pageSource, /name === 'open_html'.*agentTaskNeedsHtmlArtifact\(t\).*htmlFiles\?\.has\(resolved\.path\)/s);
  assert.match(pageSource, /verifyWorkspaceArtifact\(\)/);
  assert.match(pageSource, /artifactVerified: true, artifactPath:/);
  assert.match(pageSource, /artifactState\.files, artifactState\.htmlFiles/);
});
