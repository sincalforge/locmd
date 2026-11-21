(function installAgentCallUtils(root) {
  'use strict';

  function parseToolArguments(raw) {
    if (raw == null || raw === '') {
      return { ok: true, value: {}, serialized: '{}', error: '' };
    }
    try {
      const value = typeof raw === 'string' ? JSON.parse(raw) : raw;
      if (!value || typeof value !== 'object' || Array.isArray(value)) {
        return { ok: false, value: {}, serialized: '{}', error: '工具参数必须是 JSON 对象' };
      }
      return { ok: true, value, serialized: JSON.stringify(value), error: '' };
    } catch (error) {
      return { ok: false, value: {}, serialized: '{}', error: String(error?.message || error) };
    }
  }

  function prepareToolCalls(calls, round) {
    return calls.map((call, index) => {
      const parsed = parseToolArguments(call.args);
      return {
        ...call,
        id: call.id || `call_${round}_${index}`,
        parsedArgs: parsed.value,
        argsValid: parsed.ok,
        argsError: parsed.error,
        historyArgs: parsed.serialized,
      };
    });
  }

  function classifyToolCallError(message) {
    const text = String(message || '');
    if (
      /failed to parse (?:the )?(?:tool|function) call arguments as json/i.test(text) ||
      /(?:tool|function) call arguments?[^\n]{0,160}\b(?:invalid|not valid|json parse|parse failed)\b/i.test(text) ||
      /invalid string:.{0,100}\bcolumn\s+\d+/i.test(text)
    ) return 'invalid_arguments';

    const unsupported = [
      /\b(?:model|server)\s+(?:does not|doesn't|do not|don't)\s+support\s+(?:tool|function)\s+(?:calling|calls?)\b/i,
      /\b(?:tool|function)\s+calls?\s+(?:are\s+)?(?:not supported|unsupported|not available|disabled)\b/i,
      /\b(?:tool|function)\s+calling\s+(?:is\s+)?(?:not supported|unsupported|not available|disabled)\b/i,
      /\b(?:model|server)\s+(?:does not|doesn't|do not|don't)\s+support\s+tools?\b/i,
      /\btools?\s+(?:are\s+)?(?:not supported|unsupported|not available|disabled)\b/i,
    ];
    return unsupported.some((pattern) => pattern.test(text)) ? 'unsupported' : 'other';
  }

  function shouldRetryToolCallError(message, attempts) {
    return Number(attempts) < 1 && classifyToolCallError(message) === 'invalid_arguments';
  }

  function sanitizeToolCallHistory(messages) {
    let repaired = 0;
    for (const message of messages || []) {
      for (const call of message.tool_calls || []) {
        if (!call.function) continue;
        const parsed = parseToolArguments(call.function.arguments);
        if (!parsed.ok) repaired++;
        call.function.arguments = parsed.serialized;
      }
    }
    return repaired;
  }

  function decodeXmlText(value) {
    return String(value || '').replace(/&(#x[\da-f]+|#\d+|amp|lt|gt|quot|apos|nbsp);/gi, (all, entity) => {
      const lower = entity.toLowerCase();
      if (lower === 'amp') return '&';
      if (lower === 'lt') return '<';
      if (lower === 'gt') return '>';
      if (lower === 'quot') return '"';
      if (lower === 'apos') return "'";
      if (lower === 'nbsp') return '\u00a0';
      const code = lower.startsWith('#x') ? parseInt(lower.slice(2), 16) : parseInt(lower.slice(1), 10);
      return Number.isInteger(code) && code >= 0 && code <= 0x10ffff && !(code >= 0xd800 && code <= 0xdfff) ? String.fromCodePoint(code) : all;
    });
  }

  // Parse only Python's literal data structures; never evaluate model-generated code.
  function parsePythonLiteral(source) {
    let index = 0;
    const fail = () => { throw new Error('invalid literal'); };
    const whitespace = () => { while (/\s/.test(source[index] || '')) index++; };
    const string = () => {
      const quote = source[index++];
      let result = '';
      while (index < source.length) {
        const ch = source[index++];
        if (ch === quote) return result;
        if (ch !== '\\') { result += ch; continue; }
        if (index >= source.length) fail();
        const escaped = source[index++];
        const simple = { a: '\x07', b: '\b', f: '\f', n: '\n', r: '\r', t: '\t', v: '\x0b', '\\': '\\', "'": "'", '"': '"' };
        if (Object.hasOwn(simple, escaped)) { result += simple[escaped]; continue; }
        if (escaped === '\n') continue;
        if (escaped === 'x' || escaped === 'u' || escaped === 'U') {
          const length = escaped === 'x' ? 2 : escaped === 'u' ? 4 : 8;
          const digits = source.slice(index, index + length);
          if (digits.length !== length || !new RegExp(`^[0-9a-fA-F]{${length}}$`).test(digits)) fail();
          const code = parseInt(digits, 16);
          if (code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff)) fail();
          result += String.fromCodePoint(code);
          index += length;
          continue;
        }
        // Python preserves unknown escapes; retaining the backslash is safer than interpreting them.
        result += `\\${escaped}`;
      }
      fail();
    };
    const sequence = (close) => {
      index++;
      const result = [];
      whitespace();
      if (source[index] === close) { index++; return result; }
      for (;;) {
        result.push(value());
        whitespace();
        if (source[index] === close) { index++; return result; }
        if (source[index++] !== ',') fail();
        whitespace();
        if (source[index] === close) { index++; return result; }
      }
    };
    const object = () => {
      index++;
      const result = Object.create(null);
      whitespace();
      if (source[index] === '}') { index++; return result; }
      for (;;) {
        whitespace();
        if (source[index] !== '"' && source[index] !== "'") fail();
        const key = string();
        whitespace();
        if (source[index++] !== ':') fail();
        result[key] = value();
        whitespace();
        if (source[index] === '}') { index++; return result; }
        if (source[index++] !== ',') fail();
        whitespace();
        if (source[index] === '}') { index++; return result; }
      }
    };
    const value = () => {
      whitespace();
      const ch = source[index];
      if (ch === '"' || ch === "'") return string();
      if (ch === '[') return sequence(']');
      if (ch === '(') return sequence(')');
      if (ch === '{') return object();
      const keyword = source.slice(index).match(/^(True|False|None)\b/);
      if (keyword) {
        index += keyword[0].length;
        return { True: true, False: false, None: null }[keyword[0]];
      }
      const number = source.slice(index).match(/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?/);
      if (number) {
        index += number[0].length;
        const parsed = Number(number[0]);
        if (Number.isFinite(parsed)) return parsed;
      }
      fail();
    };
    const parsed = value();
    whitespace();
    if (index !== source.length) fail();
    return parsed;
  }

  function parseXmlScalar(raw, type, isCdata = false) {
    const value = isCdata ? String(raw || '') : decodeXmlText(raw);
    if (type === 'string') return { ok: true, value };
    let normalized = '';
    let quoted = false;
    let escaped = false;
    for (let i = 0; i < value.length; i++) {
      const ch = value[i];
      if (ch === '"' && !escaped) quoted = !quoted;
      if (!quoted) {
        const word = value.slice(i).match(/^(True|False|None)\b/);
        if (word) {
          normalized += { True: 'true', False: 'false', None: 'null' }[word[1]];
          i += word[1].length - 1;
          escaped = false;
          continue;
        }
      }
      normalized += ch;
      escaped = ch === '\\' && !escaped;
      if (ch !== '\\') escaped = false;
    }
    try {
      const parsed = JSON.parse(normalized);
      return { ok: true, value: parsed };
    } catch {
      try { return { ok: true, value: parsePythonLiteral(value) }; }
      catch { return { ok: false, value: null }; }
    }
  }

  function xmlToolDefs(definitions) {
    if (Array.isArray(definitions)) {
      return Object.fromEntries(definitions.map((d) => [d.function?.name, {
        params: d.function?.parameters?.properties || {},
        required: d.function?.parameters?.required || [],
      }]));
    }
    return definitions || {};
  }

  // MiniCPM5 native format. Fenced code is deliberately opaque so examples are not executed.
  function extractMiniCpmXmlToolCalls(source, definitions) {
    const defs = xmlToolDefs(definitions);
    const text = String(source || '');
    const fenced = /```[\s\S]*?(?:```|$)|~~~[\s\S]*?(?:~~~|$)|(`+)[^`]*?\1/g;
    let cursor = 0;
    let cleaned = '';
    const calls = [];
    const appendOrdinary = (part) => {
      const re = /<function\b[^>]*>((?:<!\[CDATA\[[\s\S]*?\]\]>|(?!<\/function\s*>)[\s\S])*)<\/function\s*>/gi;
      let at = 0;
      for (const match of part.matchAll(re)) {
        cleaned += part.slice(at, match.index);
        const block = match[0];
        const head = block.match(/^<function\b([^>]*)>/i);
        const name = head?.[1]?.match(/\bname\s*=\s*(["'])(.*?)\1/i)?.[2];
        const params = {};
        let valid = !!name && !!defs[name];
        const seen = new Set();
        const body = match[1];
        for (const p of body.matchAll(/<param\b([^>]*)>((?:<!\[CDATA\[[\s\S]*?\]\]>|(?!<\/param\s*>)[\s\S])*)<\/param\s*>/gi)) {
          const attr = p[1].match(/\bname\s*=\s*(["'])(.*?)\1/i);
          const key = attr?.[2];
          const spec = key && defs[name]?.params?.[key];
          const cdata = p[2].match(/^\s*<!\[CDATA\[([\s\S]*?)\]\]>\s*$/i);
          const raw = cdata ? cdata[1] : p[2];
          if (!key || !spec || seen.has(key)) { valid = false; continue; }
          seen.add(key);
          const parsed = parseXmlScalar(raw, spec.type || 'string', !!cdata);
          if (!parsed.ok) valid = false;
          else params[key] = parsed.value;
        }
        for (const required of defs[name]?.required || []) if (!seen.has(required)) valid = false;
        if (valid) calls.push({ id: '', name, args: JSON.stringify(params) });
        at = match.index + block.length;
      }
      cleaned += part.slice(at);
    };
    for (const match of text.matchAll(fenced)) {
      appendOrdinary(text.slice(cursor, match.index));
      cleaned += match[0];
      cursor = match.index + match[0].length;
    }
    appendOrdinary(text.slice(cursor));
    return { text: cleaned, calls };
  }

  function stableValue(value) {
    if (Array.isArray(value)) return `[${value.map(stableValue).join(',')}]`;
    if (value && typeof value === 'object') return `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${stableValue(value[key])}`).join(',')}}`;
    return JSON.stringify(value);
  }

  function mergeToolCalls(structuredCalls, xmlCalls) {
    const result = [...(structuredCalls || [])];
    const identity = (call) => {
      const parsed = parseToolArguments(call.args);
      return `${call.name}\u0000${parsed.ok ? stableValue(parsed.value) : String(call.args || '')}`;
    };
    const seen = new Set(result.map(identity));
    for (const call of xmlCalls || []) {
      const key = identity(call);
      if (!seen.has(key)) { result.push(call); seen.add(key); }
    }
    return result;
  }

  function workspaceToolIntentError(name, path, needsWorkspaceAction) {
    if (name !== 'make_dir' || !needsWorkspaceAction) return '';
    const value = String(path || '').replace(/[\\/]+$/, '');
    const base = value.split(/[\\/]/).pop() || '';
    if (!/\.(?:html?|htm)$/i.test(base)) return '';
    return `“${path}”看起来是网页文件路径，不能用 make_dir 创建文件夹。请先用 create_file 写入完整 HTML 文件，成功后再用 open_html 预览。`;
  }

  async function consumeSseJson(reader, onJson) {
    const decoder = new TextDecoder();
    let buffer = '';
    let data = [];

    const dispatch = () => {
      if (!data.length) return;
      const payload = data.join('\n');
      data = [];
      if (payload.trim() === '[DONE]') return;
      let value;
      try {
        value = JSON.parse(payload);
      } catch (error) {
        throw new Error(`模型流中的 SSE 数据不是有效 JSON：${error?.message || error}`);
      }
      onJson(value);
    };

    const processLine = (raw) => {
      const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw;
      if (!line) {
        dispatch();
        return;
      }
      if (line.startsWith(':')) return;
      const colon = line.indexOf(':');
      const field = colon < 0 ? line : line.slice(0, colon);
      if (field !== 'data') return;
      let value = colon < 0 ? '' : line.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      data.push(value);
    };

    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        buffer += decoder.decode();
        const lines = buffer.split('\n');
        buffer = lines.pop();
        for (const line of lines) processLine(line);
        if (buffer) processLine(buffer);
        dispatch();
        return;
      }
      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop();
      for (const line of lines) processLine(line);
    }
  }

  root.AgentCallUtils = Object.freeze({
    parseToolArguments,
    prepareToolCalls,
    classifyToolCallError,
    shouldRetryToolCallError,
    sanitizeToolCallHistory,
    extractMiniCpmXmlToolCalls,
    mergeToolCalls,
    workspaceToolIntentError,
    consumeSseJson,
  });
})(globalThis);
