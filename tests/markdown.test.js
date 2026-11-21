'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const context = vm.createContext({ window: {} });
vm.runInContext(fs.readFileSync(path.join(__dirname, '../frontend/markdown.js'), 'utf8'), context, {
  filename: 'markdown.js',
});
const MD = context.window.MD;

test('malformed languagist font wrapper is removed from ordinary text', () => {
  const result = MD.render('已完成：<font class="languagist">番茄钟页面</font></font>');
  assert.equal(result.html, '<p>已完成：番茄钟页面</p>');
});

test('single, double, repeated, and case-variant known wrappers are removed', () => {
  const inputs = [
    '<font class="languagist">单层</font>',
    "<font class='languagist'>双层<font CLASS=\"LANGUAGIST\">内容</font></font>",
    '<FONT CLASS="languagist">重复结束</FONT></FONT></FONT>',
  ];
  const result = MD.render(inputs.join('\n'));
  assert.equal(result.html, '<p>单层<br>双层内容<br>重复结束</p>');
  assert.equal(result.html.includes('<font'), false);
  assert.equal(result.html.includes('</font>'), false);
});

test('wrapper content with ordinary angle brackets remains readable and escaped', () => {
  const result = MD.render('<font class="languagist">比较 a < b > c，及 <tag> 普通文本</font></font>');
  assert.equal(result.html, '<p>比较 a &lt; b &gt; c，及 &lt;tag&gt; 普通文本</p>');
  assert.equal(result.html.includes('<font'), false);
});

test('unrelated HTML-like text remains escaped instead of being stripped', () => {
  const result = MD.render('<font color="red">literal</font>');
  assert.equal(result.html, '<p>&lt;font color=&quot;red&quot;&gt;literal&lt;/font&gt;</p>');
});

test('font-like markup inside inline and fenced code remains literal and escaped', () => {
  const markup = '<font class="languagist">example</font></font>';
  const result = MD.render(`示例：\`${markup}\`\n\n\`\`\`html\n${markup}\n\`\`\``, ({ text }) =>
    `<pre><code>${MD.esc(text)}</code></pre>`
  );
  assert.equal(result.html.includes('<code class="inl">&lt;font class=&quot;languagist&quot;&gt;example&lt;/font&gt;&lt;/font&gt;</code>'), true);
  assert.equal(result.codes.length, 1);
  assert.equal(result.codes[0].text, markup);
  assert.equal(result.html.includes('&lt;font class=&quot;languagist&quot;&gt;example&lt;/font&gt;&lt;/font&gt;</code></pre>'), true);
});
