// node --test userscript/test/
// 交给 usuyo 的测试要一个一次性的 usuyo 守护进程：USUYO_BIN 指向已经编好的 usuyo，
// 端口 USUYO_TEST_PORT（默认 47702，不用 47615），家目录是新建的临时目录，测完 SIGTERM 并送进废纸篓。
import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, existsSync } from 'node:fs';
import { createHmac, randomBytes } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { load, startSite, startListener, note, until, SCRIPT_FRAME } from './harness.mjs';

const BIN = process.env.USUYO_BIN;
const PORT = Number(process.env.USUYO_TEST_PORT || 47702);
const ME = '5b56be0014de415b2db830a6';
const OTHER = '600000000000000000000001';

let site;
let daemon;
let home;
let owner = '';
let deliverToken = '';

async function api(method, path, body, token = owner) {
  const res = await fetch('http://127.0.0.1:' + PORT + path, {
    method,
    headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
    body: body && JSON.stringify(body),
    signal: AbortSignal.timeout(5000),
  });
  return { status: res.status, json: res.status === 204 ? null : await res.json() };
}

before(async () => {
  site = await startSite({
    [ME]: { collect: [note(1), note(2), note(3)], like: [note(4), note(5)] },
    [OTHER]: { collect: [note(9)], like: [note(8)] },
  });
  if (!BIN) return;
  home = mkdtempSync(join(process.env.TMPDIR || tmpdir(), 'usuyo-userscript-'));
  daemon = spawn(BIN, ['daemon', '--port', String(PORT), '--first-library', join(home, 'lib')], {
    env: { ...process.env, USUYO_HOME: home },
    stdio: 'ignore',
  });
  await until(() => existsSync(join(home, 'api-token')), 15000);
  owner = readFileSync(join(home, 'api-token'), 'utf8').trim();
  await until(() => owner.length > 0);
  let created;
  for (let i = 0; i < 50 && !created; i++) {
    try {
      created = await api('POST', '/deliver-tokens', { name: '小红书油猴脚本（测试）', kind: 'extension' });
    } catch {
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  assert.equal(created.status, 200);
  deliverToken = created.json.token;
});

after(async () => {
  await site.close();
  if (daemon) {
    daemon.kill('SIGTERM');
    await new Promise((r) => daemon.once('exit', r));
  }
  if (home) spawnSync('trash', [home], { timeout: 10000 });
});

/** 让「页面」自己请求两张列表，跟真页面翻收藏、赞过一样。 */
async function browse(s, user = ME) {
  await s.page.fetch(site.origin + '/api/sns/web/v2/note/collect/page?num=30&cursor=&user_id=' + user);
  await s.page.fetch(site.origin + '/api/sns/web/v1/note/like/page?num=30&cursor=&user_id=' + user);
  await new Promise((r) => setTimeout(r, 50));
}

/** 点交给，在浏览器的对话框里贴令牌，等到不再是「先确认…」。 */
async function handOver(s, token) {
  if (token) s.answerPrompt(token);
  s.click('交给 usuyo');
  await until(() => !/先确认/.test(s.note()));
}

/** 每个测试最后都查：令牌没进页面的存储、没出现在面板上，脚本只连了 127.0.0.1。 */
function noLeak(s, token, port) {
  const secret = token.split('.')[1];
  assert.ok(!s.panelText().includes(secret), 'secret shown in the panel');
  assert.ok(!s.hasInput(), 'an input element in the page');
  for (const v of s.pageStorage.values()) assert.ok(!v.includes(secret), 'token in page storage');
  assert.deepEqual(s.violations, []);
  for (const r of s.gmRequests) assert.equal(new URL(r.url).host, '127.0.0.1:' + port);
}

test('records only your own lists', async () => {
  const s = load({ port: 1, me: ME });
  await browse(s, OTHER);
  assert.deepEqual(s.gm.get('items'), undefined);
  assert.match(s.note(), /不是你自己的列表/);
  await browse(s, ME);
  const items = s.gm.get('items');
  assert.deepEqual(Object.keys(items.favorite).sort(), [note(1), note(2), note(3)].map((n) => n.note_id));
  assert.deepEqual(Object.keys(items.like).sort(), [note(4), note(5)].map((n) => n.note_id));
  assert.equal(items.favorite[note(1).note_id].token, note(1).xsec_token);
  assert.match(s.panelText(), /收藏 3（已到底）/);
  assert.equal(s.gmRequests.length, 0, 'recording sends nothing');
  // 页面自己的那四次请求以外，脚本没向页面的站点发过东西
  assert.equal(site.seen.length, 4);
});

test('delivers both lists to a throwaway usuyo daemon', { skip: !BIN && 'USUYO_BIN not set' }, async () => {
  const s = load({ port: PORT, me: ME });
  await browse(s);
  const before = (await api('GET', '/imports')).json.length;
  const pageRequests = site.seen.length;
  await handOver(s, deliverToken);
  assert.equal(site.seen.length, pageRequests, 'delivery sends nothing to the page site');
  assert.match(s.note(), /交给 usuyo 了：收藏 3 条、赞过 2 条，到 usuyo 的导入页确认了才会去取/);

  // 握手只发随机数和令牌的 id，令牌只在核对过以后进 /v1/batches 的请求头
  const [hs, ...rest] = s.gmRequests;
  assert.equal(new URL(hs.url).pathname, '/handshake');
  assert.deepEqual(Object.keys(JSON.parse(hs.data)).sort(), ['client', 'nonce']);
  assert.equal(JSON.parse(hs.data).client, deliverToken.split('.')[0]);
  assert.ok(!JSON.stringify(hs).includes(deliverToken.split('.')[1]));
  assert.deepEqual(rest.map((r) => new URL(r.url).pathname), ['/v1/batches', '/v1/batches']);

  const imports = (await api('GET', '/imports')).json;
  assert.equal(imports.length, before + 2);
  const byTitle = Object.fromEntries(imports.map((i) => [i.title, i]));
  const day = new Date().toLocaleDateString('sv');
  for (const [label, action, n] of [['收藏', 'favorite', 3], ['赞过', 'like', 2]]) {
    const imp = byTitle['小红书' + label + ' ' + day];
    assert.ok(imp, 'batch for ' + label);
    assert.equal(imp.kind, 'delivered');
    const entries = (await api('GET', '/imports/' + imp.id + '/entries')).json;
    assert.equal(entries.length, n);
    for (const e of entries) {
      assert.equal(e.action, action);
      assert.match(e.url, /^https:\/\/www\.xiaohongshu\.com\/explore\/[0-9a-f]{24}\?xsec_token=/);
      assert.match(e.title, /^笔记 \d$/);
    }
  }
  // 交出去的从脚本里去掉
  assert.deepEqual(s.gm.get('items'), { favorite: {}, like: {} });
  assert.equal(s.gm.get('deliverToken'), deliverToken);
  assert.match(s.panelText(), /令牌 [0-9a-f]{6}… 存在脚本管理器里/);
  noLeak(s, deliverToken, PORT);
});

test('a listener that cannot prove itself gets nothing', async (t) => {
  const fake = await startListener(() => [200, { proof: randomBytes(32).toString('hex') }]);
  t.after(() => fake.close());
  const token = randomBytes(8).toString('hex') + '.' + randomBytes(32).toString('hex');
  const s = load({ port: fake.port, me: ME });
  await browse(s);
  await handOver(s, token);
  assert.match(s.note(), /没能证明自己是 usuyo.*令牌没有发出去，什么都没交/);
  assert.deepEqual(fake.seen.map((r) => r.url), ['/handshake']);
  const secret = token.split('.')[1];
  for (const r of fake.seen) {
    assert.ok(!JSON.stringify(r.headers).includes(secret) && !r.body.includes(secret));
    assert.equal(r.headers.authorization, undefined);
  }
  assert.equal(Object.keys(s.gm.get('items').favorite).length, 3, 'entries stay');
  noLeak(s, token, fake.port);
});

test('closed port says usuyo is not running', async () => {
  const gone = await startListener(() => [500, {}]);
  const port = gone.port;
  await gone.close();
  const token = randomBytes(8).toString('hex') + '.' + randomBytes(32).toString('hex');
  const s = load({ port, me: ME });
  await browse(s);
  await handOver(s, token);
  assert.match(s.note(), /usuyo 没有开着（本机 \d+ 端口没有回应），什么都没交/);
  assert.equal(s.gmRequests.length, 1);
  noLeak(s, token, port);
});

test('a refused token stops the delivery', async (t) => {
  const token = randomBytes(8).toString('hex') + '.' + randomBytes(32).toString('hex');
  // 能答对握手、却不认这个令牌的对面（例如令牌在握手之后被撤掉）
  const fake = await startListener((req, body) => {
    if (req.url === '/handshake') {
      const { nonce } = JSON.parse(body);
      const proof = createHmac('sha256', token).update(`usuyo-daemon-proof/v1\n127.0.0.1:${fake.port}\n${nonce}`).digest('hex');
      return [200, { proof }];
    }
    return [401, { code: 'unauthorized' }];
  });
  t.after(() => fake.close());
  const s = load({ port: fake.port, me: ME });
  await browse(s);
  await handOver(s, token);
  assert.match(s.note(), /usuyo 不认这个令牌（可能撤掉了），剩下的没有收下/);
  assert.deepEqual(fake.seen.map((r) => r.url), ['/handshake', '/v1/batches'], 'stops after the first refusal');
  assert.equal(Object.keys(s.gm.get('items').favorite).length, 3, 'entries stay');
  noLeak(s, token, fake.port);
  s.click('忘掉令牌');
  assert.equal(s.gm.get('deliverToken'), '');
});

test('a token revoked in usuyo is never sent to it', { skip: !BIN && 'USUYO_BIN not set' }, async () => {
  const created = await api('POST', '/deliver-tokens', { name: '撤掉的', kind: 'extension' });
  const token = created.json.token;
  assert.equal((await api('DELETE', '/deliver-tokens/' + created.json.id)).status, 204);
  const s = load({ port: PORT, me: ME });
  await browse(s);
  await handOver(s, token);
  assert.match(s.note(), /也可能这个令牌在 usuyo 里撤掉了/);
  assert.deepEqual(s.gmRequests.map((r) => new URL(r.url).pathname), ['/handshake']);
  noLeak(s, token, PORT);
});

test('a malformed token is not kept', async () => {
  const s = load({ port: 1, me: ME });
  await browse(s);
  s.answerPrompt('not-a-token');
  s.click('交给 usuyo');
  assert.match(s.note(), /这不像 usuyo 的投递令牌/);
  assert.equal(s.gm.get('deliverToken'), undefined);
  assert.equal(s.gmRequests.length, 0);
});

// 脚本管理器替脚本带上的 Origin（见 README 的「脚本管理器带什么 Origin」）：扩展的来源能进，页面的来源不能
for (const origin of ['chrome-extension://abcdefghijklmnop', 'moz-extension://0b1e5a8c-1111-4222-8333-944445555666', 'safari-web-extension://0B1E5A8C-1111-4222-8333-944445555666']) {
  test('delivers with Origin ' + origin.split(':')[0], { skip: !BIN && 'USUYO_BIN not set' }, async () => {
    const s = load({ port: PORT, me: ME, origin });
    await browse(s);
    await handOver(s, deliverToken);
    assert.match(s.note(), /交给 usuyo 了：收藏 3 条、赞过 2 条/);
    noLeak(s, deliverToken, PORT);
  });
}

test('a page Origin is turned away before any token is sent', { skip: !BIN && 'USUYO_BIN not set' }, async () => {
  const s = load({ port: PORT, me: ME, origin: 'https://www.xiaohongshu.com' });
  await browse(s);
  await handOver(s, deliverToken);
  assert.match(s.note(), /usuyo 没让这个脚本管理器的请求进来，什么都没交/);
  assert.deepEqual(s.gmRequests.map((r) => new URL(r.url).pathname), ['/handshake']);
  noLeak(s, deliverToken, PORT);
});

test('the token can be entered and forgotten from the script manager menu', async () => {
  const token = randomBytes(8).toString('hex') + '.' + randomBytes(32).toString('hex');
  const s = load({ port: 1, me: ME });
  assert.deepEqual(s.menuLabels(), ['填 usuyo 令牌', '忘掉 usuyo 令牌']);
  await browse(s);
  // 对话框没弹出来或被关掉（prompt 回 null）
  s.click('交给 usuyo');
  assert.match(s.note(), /没拿到令牌.*脚本管理器的菜单里点「填 usuyo 令牌」/);
  assert.equal(s.gmRequests.length, 0);
  s.answerPrompt('  ' + token + '\n');
  s.menu('填 usuyo 令牌');
  assert.equal(s.gm.get('deliverToken'), token, 'kept, trimmed');
  // 面板上只有 id 的前 6 位
  assert.ok(s.panelText().includes('令牌 ' + token.slice(0, 6) + '…'));
  assert.ok(!s.panelText().includes(token.slice(6, 16)) && !s.panelText().includes(token.slice(17, 25)));
  s.menu('忘掉 usuyo 令牌');
  assert.equal(s.gm.get('deliverToken'), '');
  assert.ok(!s.hasInput());
});

// 页面和脚本在同一个环境里时，页面能改掉的内置方法：换成会记账的间谍
function plant(page, secret) {
  const hits = [];
  const has = String.prototype.indexOf;
  const values = Object.values;
  let inside = false;
  // 只算脚本自己直接调的：脚本管理器那一侧（测试里是 harness 的 GM_xmlhttpRequest 和 node:http）不算
  const fromScript = () => {
    const st = new Error().stack;
    // 栈：Error、fromScript、间谍自己，再下一帧才是调它的
    const own = Reflect.apply(has, st, ['\n    at ']);
    const spyFrame = Reflect.apply(has, st, ['\n    at ', own + 1]);
    const caller = Reflect.apply(has, st, ['\n    at ', spyFrame + 1]);
    const next = Reflect.apply(has, st, ['\n', caller + 1]);
    const at = Reflect.apply(has, st, [SCRIPT_FRAME, caller]);
    return caller >= 0 && at >= 0 && (next < 0 || at < next);
  };
  const carries = (v, depth = 0) => {
    if (typeof v === 'string') return Reflect.apply(has, v, [secret]) >= 0;
    if (v && typeof v === 'object' && depth < 2) {
      try {
        return values(v).some((x) => carries(x, depth + 1));
      } catch {
        return false;
      }
    }
    return false;
  };
  const restores = [];
  const spy = (obj, name, label) => {
    const orig = obj[name];
    obj[name] = function (...args) {
      if (!inside) {
        inside = true;
        try {
          if (fromScript() && (carries(this) || args.some((a) => carries(a)))) hits.push(label);
        } finally {
          inside = false;
        }
      }
      return new.target ? Reflect.construct(orig, args, new.target) : Reflect.apply(orig, this, args);
    };
    restores.push(() => (obj[name] = orig));
  };
  for (const n of ['exec', 'test']) spy(RegExp.prototype, n, 'RegExp.' + n);
  for (const n of ['trim', 'indexOf', 'slice', 'charCodeAt', 'split', 'substring']) spy(String.prototype, n, 'String.' + n);
  spy(TextEncoder.prototype, 'encode', 'TextEncoder.encode');
  spy(JSON, 'stringify', 'JSON.stringify');
  for (const n of ['call', 'apply', 'bind']) spy(Function.prototype, n, 'Function.' + n);
  const promptCalls = [];
  const pagePrompt = page.prompt;
  page.prompt = (...a) => {
    promptCalls.push(a);
    return null;
  };
  restores.push(() => (page.prompt = pagePrompt));
  return { hits, promptCalls, restore: () => restores.reverse().forEach((r) => r()) };
}

test('page-patched builtins never see the token', { skip: !BIN && 'USUYO_BIN not set' }, async () => {
  const s = load({ port: PORT, me: ME });
  await browse(s);
  const secret = deliverToken.split('.')[1];
  const spies = plant(s.page, secret);
  try {
    s.answerPrompt(' ' + deliverToken + ' ');
    s.click('交给 usuyo');
    await until(() => !/先确认|令牌存着了/.test(s.note()));
  } finally {
    spies.restore();
  }
  assert.match(s.note(), /交给 usuyo 了：收藏 3 条、赞过 2 条/);
  assert.deepEqual(spies.hits, [], 'a patched builtin saw the token');
  assert.deepEqual(spies.promptCalls, [], 'the page prompt was used');
  assert.equal(s.prompted.length, 1, 'the prompt captured at start was used');
  assert.ok(!s.hasInput());
  noLeak(s, deliverToken, PORT);
});

// ---------- 诊断（复制诊断） ----------

const DIAG_START = '  // ---------- 诊断：只记回答的形状，不记任何值 ----------';
const DIAG_END = '  // ---------- 诊断结束 ----------';
const SCRIPT_TEXT = readFileSync(new URL('../usuyo-xhs-saved.user.js', import.meta.url), 'utf8');

function diagBlock(text) {
  const start = text.indexOf(DIAG_START);
  const end = text.indexOf(DIAG_END);
  assert.ok(start >= 0 && end > start, 'diagnosis block markers');
  return text.slice(start, end + DIAG_END.length);
}

const wait = (ms = 30) => new Promise((r) => setTimeout(r, ms));

/** 页面的 fetch 换成固定回答，并数它被调了几次。 */
function answering(body, status = 200) {
  const calls = [];
  const fetchImpl = async (input) => {
    calls.push(String(input));
    return new Response(typeof body === 'string' ? body : JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });
  };
  return { fetchImpl, calls };
}

async function copyOut(s) {
  s.click('复制诊断');
  await wait(5);
  return s.copied[s.copied.length - 1];
}

// mulberry32：小的种子也给出分散的头几个值
function rng(seed) {
  let t0 = seed >>> 0;
  return () => {
    t0 = (t0 + 0x6d2b79f5) >>> 0;
    let t = t0;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 2 ** 32;
  };
}

// 代表内容的字符串：id、url、名字、令牌、正文、里面带 JSON 的字符串
function markerString(r, i) {
  const kinds = [
    () => 'MARK' + i + 'x' + Math.floor(r() * 1e9),
    () => 'https://mark' + i + '.example.invalid/v?token=MARK' + i,
    () => String(14545102784038246591n + BigInt(i)),
    () => 'v2_0600002310' + i + 'MARK@finder',
    () => '标记内容' + i + ' MARK' + i,
    () => '{"inner":"MARKJSON' + i + '","n":' + i + '}',
  ];
  return kinds[Math.floor(r() * kinds.length)]();
}

function randomValue(r, depth, markers) {
  const pick = r();
  if (depth > 4 || pick < 0.35) {
    const m = markerString(r, markers.length);
    markers.push(m);
    return m;
  }
  if (pick < 0.45) {
    const n = 900000000 + Math.floor(r() * 99999999);
    markers.push(String(n));
    return n;
  }
  if (pick < 0.7) return Array.from({ length: Math.floor(r() * 4) }, () => randomValue(r, depth + 1, markers));
  const obj = {};
  const n = 1 + Math.floor(r() * 4);
  for (let i = 0; i < n; i++) {
    // 字段名一样的键，和长得像内容的键（id、中文、url），后一种不能出现在诊断里
    if (r() < 0.5) {
      obj['field' + String.fromCharCode(97 + i)] = randomValue(r, depth + 1, markers);
    } else {
      const k = markerString(r, markers.length);
      markers.push(k);
      obj[k] = randomValue(r, depth + 1, markers);
    }
  }
  return obj;
}


const LIST = 'https://www.xiaohongshu.com/api/sns/web/v2/note/collect/page';

test('the diagnosis never holds a planted value, key or path id', async () => {
  let nested = 0;
  for (let seed = 1; seed <= 300; seed++) {
    const r = rng(seed);
    const markers = [];
    const body = { root: randomValue(r, 1, markers), [markerString(r, -1)]: randomValue(r, 1, markers) };
    markers.push(Object.keys(body)[1]);
    if (JSON.stringify(body).includes('field')) nested++;
    const { fetchImpl } = answering(body);
    const s = load({ port: 1, me: ME, fetchImpl });
    await s.page.fetch('https://www.xiaohongshu.com/api/sns/web/v2/note/MARKID9abc/collect/page?user_id=MARKQ&cursor=MARKCURSOR');
    await s.page.fetch('https://www.xiaohongshu.com/api/sns/web/v1/note/like/MARKword' + 'x'.repeat(30));
    await wait(5);
    const out = await copyOut(s);
    for (const m of [...markers, 'MARKID9abc', 'MARKQ', 'MARKCURSOR']) {
      assert.ok(!out.includes(m), `seed ${seed}: ${JSON.stringify(m)} leaked into ${out}`);
    }
    assert.ok(!/MARK|标记|\bmark\d/.test(out), `seed ${seed}: marker fragment leaked into ${out}`);
  }
  assert.ok(nested > 100, 'generator produced nested objects');
});

test('the diagnosis keeps key paths, types and lengths of an answer', async () => {
  const { fetchImpl } = answering({ code: 0, data: { other: { items: [{ name: 'hello' }, { name: 'hi', url: 'x' }], count: 4 } } });
  const s = load({ port: 1, me: ME, fetchImpl });
  await s.page.fetch(LIST + '?user_id=' + ME);
  await wait();
  const d = JSON.parse(await copyOut(s));
  assert.deepEqual(d.answers[0].shape, { code: 'number', data: { other: { items: { $array: 2, $items: { name: 'string:2..5', url: 'string:1' } }, count: 'number' } } });
});

test('repeats merge and the number of kept answers is bounded', async () => {
  const { fetchImpl } = answering({ code: 0, data: {} });
  const s = load({ port: 1, me: ME, fetchImpl });
  const name = (i) => 'collect' + String.fromCharCode(97 + (i % 26)) + String.fromCharCode(97 + Math.floor(i / 26));
  const url = (i) => 'https://www.xiaohongshu.com/api/sns/web/v1/note/' + name(i);
  for (let i = 0; i < 50; i++) await s.page.fetch(url(i));
  await wait(60);
  let d = JSON.parse(await copyOut(s));
  assert.equal(d.answers.length, 20);
  assert.equal(d.looked, 50);
  assert.equal(d.dropped, 30);
  assert.deepEqual(d.answers.map((a) => a.path), Array.from({ length: 20 }, (_, i) => '/api/sns/web/v1/note/' + name(30 + i)));
  // 同一个请求再来 5 次：并成一条，不挤掉别的
  for (let i = 0; i < 5; i++) await s.page.fetch(url(49) + '?cursor=' + i);
  await wait(60);
  d = JSON.parse(await copyOut(s));
  assert.equal(d.answers.length, 20);
  const last = d.answers[d.answers.length - 1];
  assert.equal(last.path, '/api/sns/web/v1/note/' + name(49));
  assert.equal(last.answers, 6);
  assert.equal(d.dropped, 30);
});

test('a recognised list answer records its counts', async () => {
  const s = load({ port: 1, me: ME });
  await browse(s);
  const d = JSON.parse(await copyOut(s));
  assert.equal(d.tool, 'usuyo-xhs-saved');
  assert.equal(d.page, 'profile');
  assert.equal(d.recognised, 2);
  const a = d.answers.find((e) => e.path.endsWith('/collect/page'));
  assert.equal(a.path, '/api/sns/web/v2/note/collect/page');
  assert.deepEqual({ status: a.status, json: a.json, recognised: a.recognised, took: a.took, answers: a.answers }, { status: 200, json: true, recognised: true, took: 3, answers: 1 });
  assert.deepEqual(a.skipped, { noId: 0, noToken: 0 });
  assert.deepEqual(a.stopped, []);
  assert.deepEqual(a.shape.data.notes.$items, { note_id: 'string:24', xsec_token: 'string:6', display_title: 'string:4' });
  assert.equal(d.answers.find((e) => e.path.endsWith('/like/page')).took, 2);
});

test('entries skipped are counted by kind', async () => {
  const good = note(1);
  const { fetchImpl } = answering({ code: 0, data: { has_more: false, notes: [good, { ...note(2), note_id: 'short' }, { ...note(3), xsec_token: '' }, { ...note(4), note_id: '' }] } });
  const s = load({ port: 1, me: ME, fetchImpl });
  await s.page.fetch(LIST + '?user_id=' + ME);
  await wait();
  const a = JSON.parse(await copyOut(s)).answers[0];
  assert.deepEqual({ recognised: a.recognised, took: a.took, skipped: a.skipped }, { recognised: true, took: 1, skipped: { noId: 2, noToken: 1 } });
});

test('an unrecognised answer on a list path is recorded as not recognised', async () => {
  for (const [body, status, stopped] of [
    [{ code: 0, data: { renamed: [{ a: 1 }] } }, 200, []],
    [{ code: -100, msg: 'x' }, 200, ['errorCode']],
    ['<html>blocked</html>', 403, []],
  ]) {
    const { fetchImpl } = answering(body, status);
    const s = load({ port: 1, me: ME, fetchImpl });
    await s.page.fetch(LIST + '?user_id=' + ME);
    await wait();
    const d = JSON.parse(await copyOut(s));
    assert.equal(d.recognised, 0);
    const a = d.answers[0];
    assert.deepEqual({ status: a.status, json: a.json, recognised: a.recognised, took: a.took, stopped: a.stopped }, { status, json: typeof body !== 'string', recognised: false, took: 0, stopped });
  }
  // 名字像、但脚本不认的接口也记下来；别的接口不看
  const { fetchImpl } = answering({ code: 0, data: { boards: [{ id: '1' }] } });
  const s = load({ port: 1, me: ME, fetchImpl });
  await s.page.fetch('https://www.xiaohongshu.com/api/sns/web/v1/board/collect/list');
  await s.page.fetch('https://www.xiaohongshu.com/api/sns/web/v1/homefeed');
  await wait();
  const d = JSON.parse(await copyOut(s));
  assert.deepEqual(d.answers.map((a) => [a.path, a.recognised]), [['/api/sns/web/v1/board/collect/list', false]]);
});

test('an xhr answer is recorded the same way', async () => {
  const s = load({ port: 1, me: ME });
  const x = new s.page.XMLHttpRequest();
  const listeners = {};
  x.addEventListener = (t, fn) => (listeners[t] = fn);
  x.open('GET', LIST + '?user_id=' + ME);
  x.send();
  x.responseType = '';
  x.responseText = JSON.stringify({ code: 0, data: { has_more: false, notes: [note(1)] } });
  x.status = 200;
  listeners.load();
  const d = JSON.parse(await copyOut(s));
  assert.deepEqual(d.answers.map((a) => [a.path, a.status, a.recognised, a.took]), [['/api/sns/web/v2/note/collect/page', 200, true, 1]]);
});

test('copying puts the text on the clipboard and sends it nowhere else', async () => {
  const { fetchImpl, calls } = answering({ code: 0, data: { notes: [] } });
  const s = load({ port: 1, me: ME, fetchImpl });
  await s.page.fetch(LIST + '?user_id=' + ME);
  await wait();
  const items = structuredClone(s.gm.get('items'));
  const text = await copyOut(s);
  assert.equal(s.copied.length, 1);
  assert.equal(typeof JSON.parse(text).version, 'string');
  assert.match(s.note(), /诊断已复制，只含结构/);
  assert.match(s.panelText(), /诊断里只有回答的结构，没有任何内容/);
  assert.equal(s.paste().style.display, 'none');
  assert.deepEqual(s.gmRequests, []);
  assert.deepEqual(s.violations, []);
  assert.equal(s.pageStorage.size, 0);
  assert.deepEqual(s.gm.get('items'), items);
  assert.equal(calls.length, 1, 'no request beyond the page own');
  assert.ok(!s.hasInput());
});

test('a refused or missing clipboard falls back to a selected text box', async () => {
  for (const clipboard of ['refuse', 'none']) {
    const s = load({ port: 1, me: ME, clipboard });
    await browse(s);
    s.click('复制诊断');
    await wait(5);
    const box = s.paste();
    assert.equal(box.style.display, 'block');
    assert.equal(box.selected, true);
    assert.equal(box.readOnly, true);
    assert.equal(JSON.parse(box.value).tool, 'usuyo-xhs-saved');
    assert.match(s.note(), /没能写进剪贴板/);
    assert.deepEqual(s.copied, []);
    assert.deepEqual(s.gmRequests, []);
  }
});

test('the diagnosis holds none of the fixture titles, names or ids', async () => {
  const s = load({ port: 1, me: ME });
  await browse(s);
  const text = await copyOut(s);
  for (const n of [note(1), note(2), note(3), note(4), note(5)]) {
    assert.ok(!text.includes(n.note_id) && !text.includes(n.note_id.slice(-8)), 'note id ' + n.note_id);
    assert.ok(!text.includes(n.xsec_token), 'xsec_token');
    assert.ok(!text.includes(n.display_title), 'title');
  }
  for (const word of ['笔记', 'XT', ME, ME.slice(0, 8)]) assert.ok(!text.includes(word), word);
  assert.ok(!/https?:/.test(text), 'no url');
  assert.ok(!text.includes('127.0.0.1'));
});

test('a quiet page says so once, calmly, and only until a list answer is recognised', async (t) => {
  const realNow = Date.now;
  t.after(() => (Date.now = realNow));
  const s = load({ port: 1, me: ME });
  assert.match(s.note(), /自己往下翻/);
  const base = realNow();
  Date.now = () => base + 31000;
  s.tick();
  assert.match(s.note(), /还没认出列表的回答.*复制诊断/);
  assert.ok(!/[0-9]/.test(s.note()), 'no counts in the sentence');
  assert.equal(s.warnText(), '', 'nothing in the warning line');
  await browse(s);
  s.tick();
  assert.doesNotMatch(s.note(), /还没认出/);
  // 不在个人页上不提
  const other = load({ port: 1, me: ME, path: '/explore' });
  Date.now = () => base + 90000;
  other.tick();
  assert.doesNotMatch(other.note(), /还没认出/);
});

test('the version is bumped and the diagnosis code is the same in all three scripts', () => {
  assert.match(SCRIPT_TEXT, /^\/\/ @version      0\.3\.0$/m);
  assert.ok(SCRIPT_TEXT.includes("const VERSION = '0.3.0';"));
  for (const sibling of ['../../../x/userscript/usuyo-x-saved.user.js', '../../../bilibili/userscript/usuyo-bilibili-saved.user.js']) {
    const url = new URL(sibling, import.meta.url);
    assert.ok(existsSync(url), 'sibling script is expected at ' + url.pathname);
    assert.equal(diagBlock(SCRIPT_TEXT), diagBlock(readFileSync(url, 'utf8')));
  }
  assert.equal(SCRIPT_TEXT.match(/^\/\/ @connect\s+.*$/gm).length, 1);
});

test('an empty diagnosis still says whether requests reached the script', async () => {
  const { fetchImpl } = answering({});
  const s = load({ port: 1, me: ME, fetchImpl });
  let d = JSON.parse(await copyOut(s));
  assert.deepEqual({ requests: d.requests, looked: d.looked, answers: d.answers }, { requests: 0, looked: 0, answers: [] });
  assert.equal(typeof d.startedAs, 'string');
  await s.page.fetch('https://www.xiaohongshu.com/api/sns/web/v1/homefeed');
  await wait();
  d = JSON.parse(await copyOut(s));
  assert.deepEqual({ requests: d.requests, looked: d.looked, answers: d.answers }, { requests: 1, looked: 0, answers: [] });
});

test('a protocol-relative address loses its host and merges with the full one', async () => {
  const { fetchImpl } = answering({});
  const s = load({ port: 1, me: ME, fetchImpl });
  await s.page.fetch('//edith.xiaohongshu.com/api/sns/web/v2/note/collect/page?user_id=' + ME);
  await s.page.fetch('https://www.xiaohongshu.com/api/sns/web/v2/note/collect/page?user_id=' + ME);
  await wait();
  const d = JSON.parse(await copyOut(s));
  assert.deepEqual(d.answers.map((a) => [a.path, a.answers]), [['/api/sns/web/v2/note/collect/page', 2]]);
});
