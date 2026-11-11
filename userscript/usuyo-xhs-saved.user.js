// ==UserScript==
// @name         usuyo：交出小红书的收藏和赞过
// @namespace    usuyo-fetch/xiaohongshu
// @version      0.1.1
// @description  在你自己的「收藏」「赞过」页上，只看页面自己请求回来的列表，记下笔记 id、xsec_token 和标题。usuyo 还不能接收这份清单，这一版不往任何地方发送。不发额外请求、不自动滚动、不读 cookie。
// @match        https://www.xiaohongshu.com/*
// @run-at       document-start
// @noframes
// @grant        unsafeWindow
// @grant        GM_getValue
// @grant        GM_setValue
// @license      MIT
// ==/UserScript==

/*
 * 改写自 wkwunju/xhs-vault 的 userscript/xhs-export.user.js（MIT）：
 * 留下了「截获页面自己发出的列表请求」这一做法；去掉了自动滚动、导出 cookies.json、
 * 写页面 localStorage。交给 usuyo 的那一步等 usuyo 有了接收清单的本机接口、
 * 又能先确认对面是 usuyo 时再加（usuyo 插件协议 §5.1）；在那之前脚本不往外发任何东西。
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

  // usuyo 现在还没有接收这份清单的本机接口，也没有办法先确认监听那个端口的就是 usuyo。
  // 所以这一版什么都不发：不要令牌，不连本机端口，记下的条目只留在脚本管理器里。
  // 早先版本存下的令牌和端口在这里清掉。
  GM_setValue('usuyoToken', '');
  GM_setValue('usuyoPort', '');

  function deliver() {
    status.notice = '这一版 usuyo 还不能接收清单，什么都没有发出去；记下的条目留在这里，12 小时内有效';
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
