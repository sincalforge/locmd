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
    consumeSseJson,
  });
})(globalThis);
