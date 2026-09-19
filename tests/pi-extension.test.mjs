// Run with Node >= 22.18 (native TypeScript stripping): node --test tests/pi-extension.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

test('Pi extension maps decisions, handles noninteractive asks, and records human results', async () => {
  const dir = await mkdtemp(join(tmpdir(), 'pi-extension-test-'));
  try {
    const state = join(dir, 'state.json');
    const capture = join(dir, 'capture.json');
    const executable = join(dir, 'approver');
    await writeFile(executable, `#!${process.execPath}\nconst fs = require('node:fs');\nlet s='';process.stdin.on('data',c=>s+=c);process.stdin.on('end',()=>{const p=JSON.parse(s);fs.writeFileSync(${JSON.stringify(capture)},JSON.stringify({command:process.argv[2],payload:p}));if(process.argv[2]==='hook')process.stdout.write(fs.readFileSync(${JSON.stringify(state)}));});\n`, { mode: 0o755 });
    const source = (await readFile(new URL('../extensions/pi.ts', import.meta.url), 'utf8')).replace('const executable = "any-auto";', `const executable = ${JSON.stringify(executable)};`);
    const extension = join(dir, 'extension.ts');
    await writeFile(extension, source);
    const { default: install } = await import(pathToFileURL(extension).href);
    const handlers = new Map();
    const entries = [];
    install({ on: (name, handler) => handlers.set(name, handler), appendEntry: (type, data) => entries.push({ type:'custom',customType:type,data }), getAllTools: () => [{name:'write',sourceInfo:{source:'builtin'}}] });
    let confirmations = 0;
    const ctx = { cwd: dir, hasUI: false, sessionManager: { getSessionId: () => 'user-session', getBranch: () => entries }, ui: { confirm: async () => { confirmations++; return true; } } };
    await handlers.get('session_start')({}, ctx);
    const event = { toolName:'write', input:{path:'hello'}, toolCallId:'tool-1' };
    await writeFile(state, JSON.stringify({decision:'allow'}));
    assert.equal(await handlers.get('tool_call')(event, ctx), undefined);
    let call = JSON.parse(await readFile(capture,'utf8'));
    assert.equal(call.payload.builtin_tool, true);
    assert.equal(call.payload.conversationId, 'user-session');
    await writeFile(state, JSON.stringify({decision:'deny',reason:'blocked'}));
    assert.equal((await handlers.get('tool_call')(event, ctx)).reason, 'blocked');
    await writeFile(state, JSON.stringify({decision:'ask',reason:'confirm'}));
    assert.equal((await handlers.get('tool_call')(event, ctx)).block, true);
    assert.equal(confirmations,0);
    ctx.hasUI = true;
    assert.equal(await handlers.get('tool_call')(event, ctx), undefined);
    assert.equal(confirmations,1);
    call = JSON.parse(await readFile(capture,'utf8'));
    assert.equal(call.command,'human-result');
    assert.equal(call.payload.allowed,true);
    await handlers.get('session_tree')({},ctx);
    await handlers.get('session_start')({},ctx);
    await writeFile(state, JSON.stringify({decision:'allow'}));
    await handlers.get('tool_call')(event, ctx);
    call = JSON.parse(await readFile(capture,'utf8'));
    assert.match(call.payload.conversationId,/^user-session:.+/);
    await writeFile(state, '{bad json');
    assert.equal((await handlers.get('tool_call')(event, ctx)).block,true);
    const controller = new AbortController(); controller.abort(); ctx.signal = controller.signal;
    await writeFile(state, JSON.stringify({decision:'allow'}));
    assert.equal((await handlers.get('tool_call')(event, ctx)).block,true);
  } finally { await rm(dir,{recursive:true,force:true}); }
});
