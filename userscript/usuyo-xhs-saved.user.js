// ==UserScript==
// @name         usuyo：交出小红书的收藏和赞过
// @namespace    usuyo-fetch/xiaohongshu
// @version      0.1.0
// @description  在你自己的「收藏」「赞过」页上，只看页面自己请求回来的列表，记下笔记 id、xsec_token 和标题；你点按钮时交给本机的 usuyo。不发额外请求、不自动滚动、不读 cookie。
// @match        https://www.xiaohongshu.com/*
// @run-at       document-start
// @noframes
// @grant        unsafeWindow
// @grant        GM_xmlhttpRequest
// @grant        GM_getValue
// @grant        GM_setValue
// @grant        GM_registerMenuCommand
// @connect      127.0.0.1
// @license      MIT
// ==/UserScript==

/*
 * 改写自 wkwunju/xhs-vault 的 userscript/xhs-export.user.js（MIT）：
 * 留下了「截获页面自己发出的列表请求」这一做法；去掉了自动滚动、导出 cookies.json、
 * 写页面 localStorage，导出文件改成交给本机 usuyo 的 /v1/batches（usuyo 插件协议 §5.1、§9.4）。
 *
 * xhs-vault 的许可声明：
 *
 * MIT License
 *
 * Copyright (c) 2026 the xhs-vault contributors
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

(function () {
  'use strict';

  const page = unsafeWindow;
  const DEFAULT_PORT = 47821;
  // xsec_token 只在几小时内有效，记下超过这个时间的条目不再交
  const KEEP_MS = 12 * 60 * 60 * 1000;
  // 收藏页、赞过页自己请求的列表（OpenCLI 的 collection-helpers.js 记的接口名）
  const ENDPOINT = /\/api\/sns\/web\/v\d+\/note\/(collect|like)\/page(?:[?#]|$)/;
  const LABEL = { favorite: '收藏', like: '赞过' };

  // ---------- 记下的条目，存在脚本管理器里，页面读不到 ----------

  /** @type {{favorite: Object<string, {token: string, title: string, seenAt: number}>, like: Object}} */
  const store = loadStore();
  const status = { favorite: { hasMore: null }, like: { hasMore: null }, warning: '', notice: '' };

  function loadStore() {
    const empty = { favorite: {}, like: {} };
    let saved;
    try {
      saved = GM_getValue('items', empty);
    } catch (e) {
      return empty;
    }
    const now = Date.now();
    const out = { favorite: {}, like: {} };
    for (const action of ['favorite', 'like']) {
      const items = (saved && saved[action]) || {};
      for (const id of Object.keys(items)) {
        const item = items[id];
        if (item && typeof item.token === 'string' && now - item.seenAt < KEEP_MS) {
          out[action][id] = item;
        }
      }
    }
    return out;
  }

  function saveStore() {
    GM_setValue('items', store);
  }

  function count(action) {
    return Object.keys(store[action]).length;
  }

  // ---------- 只看页面自己请求回来的列表 ----------

  function actionOf(url) {
    const m = ENDPOINT.exec(url);
    if (!m) return null;
    return m[1] === 'collect' ? 'favorite' : 'like';
  }

  /** 登录的是谁：读页面已有的状态对象，不发请求。读不出来就是空字符串。 */
  function ownUserId() {
    try {
      const info = page.__INITIAL_STATE__ && page.__INITIAL_STATE__.user && page.__INITIAL_STATE__.user.userInfo;
      const value = info && (info._value || info._rawValue || info.value || info);
      return String((value && (value.user_id || value.userId)) || '');
    } catch (e) {
      return '';
    }
  }

  function absorb(rawUrl, json) {
    const action = actionOf(rawUrl);
    if (!action || !json || typeof json !== 'object') return;
    if (json.code !== 0) {
      // 掉线或风控时页面自己拿到的就是这些码；照实说出来，让你停手
      status.warning = '小红书回了 code ' + String(json.code).slice(0, 12) + '，先别往下翻了';
      render();
      return;
    }
    // 只收你自己的列表：请求里的 user_id 要等于页面里登录的那个人
    let listOwner = '';
    try {
      listOwner = new URL(rawUrl, page.location.href).searchParams.get('user_id') || '';
    } catch (e) {
      return;
    }
    const me = ownUserId();
    if (!me || listOwner !== me) {
      status.notice = me ? '这不是你自己的列表，没有记' : '没认出登录的账号，没有记';
      render();
      return;
    }
    const data = json.data || {};
    if ('has_more' in data) status[action].hasMore = data.has_more;
    const notes = Array.isArray(data.notes) ? data.notes : Array.isArray(data.note_list) ? data.note_list : [];
    const now = Date.now();
    for (const n of notes) {
      const card = (n && (n.note_card || n.noteCard)) || n || {};
      const id = String(n.note_id || n.noteId || n.id || card.note_id || card.noteId || '');
      const token = String(n.xsec_token || n.xsecToken || card.xsec_token || card.xsecToken || '');
      if (!/^[0-9a-f]{24}$/i.test(id) || !token) continue;
      const title = String(card.display_title || card.displayTitle || card.title || '').slice(0, 200);
      store[action][id.toLowerCase()] = { token, title, seenAt: now };
    }
    status.notice = '';
    status.warning = '';
    saveStore();
    render();
  }

  // fetch：拿响应的副本读，原响应原样交回页面
  const originalFetch = page.fetch;
  page.fetch = function (input) {
    const result = originalFetch.apply(this, arguments);
    try {
      const url = typeof input === 'string' ? input : (input && input.url) || '';
      if (actionOf(url)) {
        result
          .then((r) => r.clone().json())
          .then((j) => absorb(url, j))
          .catch(() => {});
      }
    } catch (e) {
      // 读不了就不记，页面照常
    }
    return result;
  };

  // XHR：请求地址记在脚本自己的 WeakMap 里，不往页面的对象上挂属性
  const xhrUrls = new WeakMap();
  const xhr = page.XMLHttpRequest.prototype;
  const originalOpen = xhr.open;
  const originalSend = xhr.send;
  xhr.open = function (_method, url) {
    try {
      xhrUrls.set(this, String(url));
    } catch (e) {
      // 同上
    }
    return originalOpen.apply(this, arguments);
  };
  xhr.send = function () {
    const url = xhrUrls.get(this) || '';
    if (actionOf(url)) {
      this.addEventListener('load', () => {
        try {
          const body = this.responseType === 'json' ? this.response : JSON.parse(this.responseText);
          absorb(url, body);
        } catch (e) {
          // 同上
        }
      });
    }
    return originalSend.apply(this, arguments);
  };

  // ---------- 交给 usuyo ----------

  // 弹窗用脚本自己的 prompt，不用页面上可能被换掉的那个
  function token() {
    let t = GM_getValue('usuyoToken', '');
    if (!t) {
      t = (prompt('粘贴 usuyo 设置 › 隐私 › 本地接口里给这个脚本生成的访问令牌') || '').trim();
      if (t) GM_setValue('usuyoToken', t);
    }
    return t;
  }

  function noteUrl(id, item) {
    return 'https://www.xiaohongshu.com/explore/' + id + '?xsec_token=' + encodeURIComponent(item.token) + '&xsec_source=pc_user';
  }

  function stamp() {
    const d = new Date();
    const pad = (n) => String(n).padStart(2, '0');
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) + ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
  }

  function send(action, bearer) {
    const ids = Object.keys(store[action]);
    if (!ids.length) return Promise.resolve(0);
    const body = {
      name: '小红书' + LABEL[action] + ' ' + stamp(),
      action,
      links: ids.map((id) => ({ url: noteUrl(id, store[action][id]), title: store[action][id].title || undefined })),
    };
    const port = Number(GM_getValue('usuyoPort', DEFAULT_PORT)) || DEFAULT_PORT;
    return new Promise((resolve, reject) => {
      GM_xmlhttpRequest({
        method: 'POST',
        url: 'http://127.0.0.1:' + port + '/v1/batches',
        headers: { Authorization: 'Bearer ' + bearer, 'Content-Type': 'application/json' },
        data: JSON.stringify(body),
        timeout: 10000,
        onload: (r) => {
          if (r.status === 202) {
            for (const id of ids) delete store[action][id];
            saveStore();
            resolve(ids.length);
          } else if (r.status === 401) {
            GM_setValue('usuyoToken', '');
            reject(new Error('令牌不对，下次点的时候重新粘贴'));
          } else {
            reject(new Error('usuyo 回了 HTTP ' + r.status));
          }
        },
        onerror: () => reject(new Error('连不上本机的 usuyo，确认它开着')),
        ontimeout: () => reject(new Error('usuyo 没有回应')),
      });
    });
  }

  async function deliver() {
    const bearer = token();
    if (!bearer) return;
    status.notice = '正在交给 usuyo…';
    render();
    try {
      let sent = 0;
      for (const action of ['favorite', 'like']) sent += await send(action, bearer);
      status.notice = sent ? '交给了 usuyo：' + sent + ' 条，在 usuyo 的导入预览里确认' : '还没有记下的条目';
    } catch (e) {
      status.notice = e.message;
    }
    render();
  }

  function clearAll() {
    store.favorite = {};
    store.like = {};
    status.notice = '';
    status.warning = '';
    saveStore();
    render();
  }

  GM_registerMenuCommand('usuyo：重新填访问令牌', () => {
    GM_setValue('usuyoToken', '');
    token();
  });
  GM_registerMenuCommand('usuyo：改本地端口', () => {
    const p = (prompt('usuyo 本地接口的端口', String(GM_getValue('usuyoPort', DEFAULT_PORT))) || '').trim();
    if (/^\d{2,5}$/.test(p)) GM_setValue('usuyoPort', Number(p));
  });

  // ---------- 角落里的一小块计数 ----------

  let ui = null;

  function onProfile() {
    return page.location.pathname.startsWith('/user/profile/');
  }

  function mount() {
    if (ui || !document.body) return;
    const host = document.createElement('div');
    const root = host.attachShadow({ mode: 'closed' });
    const style = document.createElement('style');
    style.textContent =
      ':host{all:initial}' +
      '.box{position:fixed;right:12px;bottom:12px;z-index:2147483647;max-width:260px;padding:8px 10px;' +
      'border-radius:8px;background:rgba(32,33,36,.86);color:#eee;font:12px/1.5 -apple-system,system-ui,sans-serif}' +
      '.row{display:flex;gap:6px;margin-top:6px}' +
      'button{flex:1;padding:3px 6px;border:1px solid rgba(255,255,255,.25);border-radius:5px;' +
      'background:transparent;color:inherit;font:inherit;cursor:pointer}' +
      'button:hover{background:rgba(255,255,255,.12)}' +
      '.note{margin-top:4px;opacity:.75}.warn{margin-top:4px;color:#ffb4a8}';
    const box = document.createElement('div');
    box.className = 'box';
    const line = document.createElement('div');
    const warn = document.createElement('div');
    warn.className = 'warn';
    const note = document.createElement('div');
    note.className = 'note';
    const row = document.createElement('div');
    row.className = 'row';
    const go = document.createElement('button');
    go.textContent = '交给 usuyo';
    go.addEventListener('click', deliver);
    const clear = document.createElement('button');
    clear.textContent = '清空';
    clear.addEventListener('click', clearAll);
    row.append(go, clear);
    box.append(line, warn, note, row);
    root.append(style, box);
    document.body.appendChild(host);
    ui = { host, line, warn, note };
    render();
  }

  function render() {
    if (!ui) return;
    ui.host.style.display = onProfile() ? '' : 'none';
    const part = (action) => {
      const end = status[action].hasMore === false ? '（已到底）' : '';
      return LABEL[action] + ' ' + count(action) + end;
    };
    ui.line.textContent = 'usuyo · ' + part('favorite') + ' · ' + part('like');
    ui.warn.textContent = status.warning;
    ui.note.textContent = status.notice || '自己往下翻，翻到的会记下来';
  }

  // 页面是单页应用：换页时只切换显示，不做别的
  let lastPath = '';
  setInterval(() => {
    mount();
    if (page.location.pathname !== lastPath) {
      lastPath = page.location.pathname;
      render();
    }
  }, 1000);
})();
