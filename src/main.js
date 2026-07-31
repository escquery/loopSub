// loopSub 前端：面板遥控 mpv + 双语句子列表 + 英文复制 + 搜索/翻译/窗口行为
// 通过 withGlobalTauri 暴露的 window.__TAURI__.core.invoke 调用 Rust 命令
'use strict';

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ---------- 全局状态 ----------
const state = {
  lines: [],           // [{number, start_ms, end_ms, text}] 英文原文
  translations: {},    // number -> 中文译文
  videoHash: '',
  videoPath: '',
  connected: false,
  currentIdx: -1,
  selected: new Set(),
  lastClickIdx: -1,
  sentenceLoop: false,
  followMode: false,
  followPausedIdx: -1,
  showZh: true,
  settings: null,
  hotkeyMap: {},       // combo -> action（由 settings.hotkeys 反转）
  copyTemplate: '请逐句讲解以下美剧台词中的生词、短语和口语用法：\n\n{lines}',
  delayStep: 0.1,
};

const $ = (sel) => document.querySelector(sel);
const listEl = $('#sentence-list');
const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

// ---------- mpv 命令封装 ----------
async function mpv(...args) {
  if (!state.connected) return null;
  try {
    return await invoke('mpv_command', { args });
  } catch (e) {
    console.warn('mpv command failed', args, e);
    return null;
  }
}

const osd = (text) => mpv('show-text', String(text), 1200);

function markConnected() {
  state.connected = true;
  $('#conn-status').textContent = '已连接';
  $('#conn-status').classList.add('ok');
}

function showNotice(html) {
  const bar = $('#notice-bar');
  if (!html) {
    bar.classList.add('hidden');
    return;
  }
  bar.innerHTML = html;
  bar.classList.remove('hidden');
}

// ---------- 视频 / 字幕加载 ----------
$('#btn-play').addEventListener('click', async () => {
  const path = $('#video-path').value.trim();
  if (!path) return;
  const btn = $('#btn-play');
  btn.disabled = true;
  btn.textContent = '加载中…';
  try {
    const res = await invoke('load_video', { path });
    state.videoPath = path;
    state.videoHash = res.video_hash;
    state.lines = res.lines;
    state.translations = {};
    state.currentIdx = -1;
    state.selected.clear();
    markConnected();
    renderList();
    $('#trans-bar').classList.toggle('hidden', state.lines.length === 0);
    if (res.notice) {
      showNotice(`${esc(res.notice)} <button id="notice-search">去搜索</button>`);
      $('#notice-search').addEventListener('click', openSearchPanel);
    } else {
      showNotice(null);
      osd(`字幕来源：${res.source === 'cache' ? '缓存' : '内嵌提取'}`);
      loadCachedTranslation();
    }
  } catch (e) {
    showNotice('加载视频失败: ' + esc(String(e)));
  } finally {
    btn.disabled = false;
    btn.textContent = '加载视频';
  }
});

// 手动连接已运行的 mpv（高级）
$('#btn-connect').addEventListener('click', async () => {
  const socketPath = $('#socket-path').value.trim();
  if (!socketPath) return;
  try {
    await invoke('mpv_connect', { socketPath });
    markConnected();
  } catch (e) {
    $('#conn-status').textContent = '连接失败: ' + e;
  }
});

// 手动加载 SRT
$('#btn-load').addEventListener('click', async () => {
  const path = $('#srt-path').value.trim();
  if (!path) return;
  try {
    state.lines = await invoke('load_srt', { path });
    state.translations = {};
    state.currentIdx = -1;
    state.selected.clear();
    renderList();
    $('#trans-bar').classList.remove('hidden');
  } catch (e) {
    showNotice('字幕加载失败: ' + esc(String(e)));
  }
});

// ---------- OpenSubtitles 搜索 ----------
function openSearchPanel() {
  $('#search-panel').classList.remove('hidden');
}
$('#btn-open-search').addEventListener('click', openSearchPanel);
$('#btn-search-close').addEventListener('click', () => $('#search-panel').classList.add('hidden'));

$('#btn-search').addEventListener('click', async () => {
  if (!state.videoPath) {
    $('#search-results').innerHTML = '<span class="dim">请先加载视频</span>';
    return;
  }
  const btn = $('#btn-search');
  btn.disabled = true;
  $('#search-results').innerHTML = '<span class="dim">搜索中…</span>';
  try {
    const list = await invoke('search_subtitles', { videoPath: state.videoPath });
    if (list.length === 0) {
      $('#search-results').innerHTML = '<span class="dim">没有找到匹配的字幕</span>';
      return;
    }
    $('#search-results').innerHTML = list
      .map(
        (c, i) => `<div class="candidate" data-id="${c.file_id}" data-idx="${i}">
          <span class="release" title="${esc(c.release)}">${esc(c.release || '(未命名)')}</span>
          <span class="meta">⬇${c.downloads}${c.hearing_impaired ? ' · HI' : ''}</span>
        </div>`
      )
      .join('');
  } catch (e) {
    $('#search-results').innerHTML = `<span class="dim">搜索失败: ${esc(String(e))}</span>`;
  } finally {
    btn.disabled = false;
  }
});

$('#search-results').addEventListener('click', async (e) => {
  const row = e.target.closest('.candidate');
  if (!row) return;
  row.classList.add('busy');
  try {
    const lines = await invoke('download_subtitle', {
      videoHash: state.videoHash,
      fileId: Number(row.dataset.id),
    });
    state.lines = lines;
    state.translations = {};
    state.currentIdx = -1;
    state.selected.clear();
    renderList();
    $('#search-panel').classList.add('hidden');
    $('#trans-bar').classList.remove('hidden');
    showNotice(null);
    osd(`字幕已下载（${lines.length} 句）`);
  } catch (err) {
    showNotice('下载失败: ' + esc(String(err)));
  } finally {
    row.classList.remove('busy');
  }
});

// ---------- LLM 翻译 ----------
listen('translate-progress', (e) => {
  const p = e.payload;
  $('#trans-progress').textContent =
    `${p.done_batches}/${p.total_batches} 批` + (p.failed_lines > 0 ? `（${p.failed_lines} 句失败）` : '');
});

$('#btn-translate').addEventListener('click', async () => {
  if (!state.videoHash) return;
  const btn = $('#btn-translate');
  btn.disabled = true;
  btn.textContent = '翻译中…';
  $('#trans-progress').textContent = '准备中…';
  try {
    const n = await invoke('translate_subtitles', { videoHash: state.videoHash, force: false });
    $('#trans-progress').textContent = `完成（${n} 句）`;
    await loadCachedTranslation();
    osd(`翻译完成（${n} 句）`);
  } catch (e) {
    $('#trans-progress').textContent = '失败: ' + e;
  } finally {
    btn.disabled = false;
    btn.textContent = '翻译整集';
  }
});

async function loadCachedTranslation() {
  if (!state.videoHash) return;
  try {
    const map = await invoke('get_translation', { videoHash: state.videoHash });
    if (map && Object.keys(map).length > 0) {
      state.translations = map;
      renderList();
      $('#btn-toggle-zh').classList.remove('hidden');
    }
  } catch (e) {
    console.warn('load translation failed', e);
  }
}

function toggleZh() {
  state.showZh = !state.showZh;
  document.body.classList.toggle('no-zh', !state.showZh);
  $('#btn-toggle-zh').textContent = state.showZh ? '隐藏译文' : '显示译文';
  osd(state.showZh ? '译文 显' : '译文 隐');
}
$('#btn-toggle-zh').addEventListener('click', toggleZh);

// ---------- 句子列表（英文 + 译文；译文 user-select:none 防误复制） ----------
function fmtTime(ms) {
  const s = Math.floor(ms / 1000);
  const mm = String(Math.floor(s / 60)).padStart(2, '0');
  const ss = String(s % 60).padStart(2, '0');
  return `${mm}:${ss}`;
}

function renderList() {
  const cur = state.currentIdx;
  listEl.innerHTML = state.lines
    .map((l, i) => {
      const zh = state.translations[l.number];
      return `<div class="line" data-idx="${i}">
        <span class="no">${l.number}</span>
        <span class="time">${fmtTime(l.start_ms)}</span>
        <span class="text">${esc(l.text)}${zh ? `<span class="zh">${esc(zh)}</span>` : ''}</span>
      </div>`;
    })
    .join('');
  if (cur >= 0) setCurrent(cur);
  syncSelectionUI();
}

listEl.addEventListener('click', (e) => {
  const row = e.target.closest('.line');
  if (!row) return;
  const idx = Number(row.dataset.idx);

  if (e.shiftKey && state.lastClickIdx >= 0) {
    const [a, b] = [Math.min(state.lastClickIdx, idx), Math.max(state.lastClickIdx, idx)];
    state.selected.clear();
    for (let i = a; i <= b; i++) state.selected.add(i);
  } else if (e.ctrlKey || e.metaKey) {
    state.selected.has(idx) ? state.selected.delete(idx) : state.selected.add(idx);
  } else {
    state.lastClickIdx = idx;
    seekToLine(idx);
    return;
  }
  state.lastClickIdx = idx;
  syncSelectionUI();
});

function syncSelectionUI() {
  listEl.querySelectorAll('.line').forEach((el) => {
    el.classList.toggle('selected', state.selected.has(Number(el.dataset.idx)));
  });
}

function seekToLine(idx) {
  const l = state.lines[idx];
  if (!l) return;
  state.followPausedIdx = -1;
  mpv('seek', l.start_ms / 1000, 'absolute');
  mpv('set_property', 'pause', false);
}

// ---------- 当前句反查（time-pos → 句子表） ----------
function findCurrent(posMs) {
  let lo = 0, hi = state.lines.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (state.lines[mid].start_ms <= posMs) lo = mid + 1;
    else hi = mid;
  }
  return lo === 0 ? -1 : lo - 1;
}

function setCurrent(idx) {
  if (idx === state.currentIdx) return;
  state.currentIdx = idx;
  listEl.querySelectorAll('.line.current').forEach((el) => el.classList.remove('current'));
  const el = listEl.querySelector(`.line[data-idx="${idx}"]`);
  if (el) {
    el.classList.add('current');
    el.scrollIntoView({ block: 'nearest' });
    $('#current-sentence').textContent = state.lines[idx]?.text ?? '—';
  }
}

// ---------- 轮询播放状态 ----------
setInterval(async () => {
  if (!state.connected) return;
  const paused = await mpv('get_property', 'pause');
  if (state.lines.length > 0) {
    const pos = await mpv('get_property', 'time-pos');
    if (typeof pos === 'number') {
      setCurrent(findCurrent(pos * 1000));
      if (state.followMode && state.currentIdx >= 0) {
        const line = state.lines[state.currentIdx];
        if (line && pos * 1000 >= line.end_ms && state.followPausedIdx !== state.currentIdx) {
          state.followPausedIdx = state.currentIdx;
          mpv('set_property', 'pause', true);
          osd('跟读暂停');
        }
      }
    }
  }
  const speed = await mpv('get_property', 'speed');
  if (typeof speed === 'number') $('#speed-label').textContent = speed.toFixed(1) + 'x';
  // 迷你条：设置开启时跟随播放/暂停自动收放
  if (state.settings?.window?.mini_bar && typeof paused === 'boolean') {
    document.body.classList.toggle('mini', !paused);
  }
  updateBadges(paused);
}, 300);

// ---------- 状态徽章（点击即关闭对应功能） ----------
async function updateBadges(paused) {
  const abA = await mpv('get_property', 'ab-loop-a');
  const delay = await mpv('get_property', 'sub-delay');
  const badges = [];
  if (paused) badges.push({ id: 'paused', label: '暂停' });
  if (typeof abA === 'number') badges.push({ id: 'ab', label: 'AB循环' });
  if (state.sentenceLoop) badges.push({ id: 'loop', label: '单句循环' });
  if (state.followMode) badges.push({ id: 'follow', label: '跟读' });
  if (typeof delay === 'number' && Math.abs(delay) > 0.001) {
    badges.push({ id: 'delay', label: `字幕${delay > 0 ? '+' : ''}${delay.toFixed(1)}s` });
  }
  $('#status-badges').innerHTML = badges
    .map((b) => `<button class="badge" data-badge="${b.id}" title="点击关闭">${b.label}</button>`)
    .join('');
}

$('#status-badges').addEventListener('click', (e) => {
  const b = e.target.closest('.badge');
  if (!b) return;
  switch (b.dataset.badge) {
    case 'paused': mpv('cycle', 'pause'); break;
    case 'ab':
      mpv('set_property', 'ab-loop-a', 'no');
      mpv('set_property', 'ab-loop-b', 'no');
      state.sentenceLoop = false;
      break;
    case 'loop': toggleSentenceLoop(); break;
    case 'follow': toggleFollow(); break;
    case 'delay': adjustSubDelay(0, true); break;
  }
});

// ---------- 播放控制 ----------
async function changeSpeed(delta) {
  const cur = await mpv('get_property', 'speed');
  if (typeof cur !== 'number') return;
  const next = Math.min(3, Math.max(0.25, Math.round((cur + delta) * 10) / 10));
  await mpv('set_property', 'speed', next);
  osd(next.toFixed(1) + 'x');
}

async function setABPoint(which) {
  const pos = await mpv('get_property', 'time-pos');
  if (typeof pos !== 'number') return;
  await mpv('set_property', `ab-loop-${which}`, pos);
  osd(`${which.toUpperCase()}: ${pos.toFixed(1)}s`);
}

async function nudgeABPoint(which, delta) {
  const cur = await mpv('get_property', `ab-loop-${which}`);
  if (typeof cur !== 'number') return;
  const next = Math.max(0, cur + delta);
  await mpv('set_property', `ab-loop-${which}`, next);
  osd(`${which.toUpperCase()}: ${next.toFixed(1)}s`);
}

async function toggleSentenceLoop() {
  if (state.sentenceLoop) {
    await mpv('set_property', 'ab-loop-a', 'no');
    await mpv('set_property', 'ab-loop-b', 'no');
    state.sentenceLoop = false;
    osd('单句循环 关');
  } else {
    const l = state.lines[state.currentIdx];
    if (!l) return;
    await mpv('set_property', 'ab-loop-a', l.start_ms / 1000);
    await mpv('set_property', 'ab-loop-b', l.end_ms / 1000);
    state.sentenceLoop = true;
    osd('单句循环 开');
  }
}

function toggleFollow() {
  state.followMode = !state.followMode;
  state.followPausedIdx = -1;
  osd(state.followMode ? '跟读模式 开' : '跟读模式 关');
}

async function adjustSubDelay(delta, reset = false) {
  const cur = reset ? 0 : await mpv('get_property', 'sub-delay');
  if (typeof cur !== 'number') return;
  const next = reset ? 0 : Math.round((cur + delta) * 100) / 100;
  await mpv('set_property', 'sub-delay', next);
  osd(`字幕延迟 ${next >= 0 ? '+' : ''}${next.toFixed(2)}s`);
}

$('#transport').addEventListener('click', (e) => {
  const act = e.target.dataset?.act;
  if (!act) return;
  switch (act) {
    case 'prev': mpv('sub-seek', -1); break;
    case 'toggle': state.followPausedIdx = -1; mpv('cycle', 'pause'); break;
    case 'next': mpv('sub-seek', 1); break;
    case 'slower': changeSpeed(-0.1); break;
    case 'faster': changeSpeed(0.1); break;
  }
});

// ---------- 快捷键（数据驱动：combo → action，设置页可全部改绑） ----------
const actions = {
  toggle_pause: () => { state.followPausedIdx = -1; mpv('cycle', 'pause'); },
  seek_back: () => mpv('seek', -2, 'relative', 'exact'),
  seek_forward: () => mpv('seek', 2, 'relative', 'exact'),
  prev_sentence: () => mpv('sub-seek', -1),
  next_sentence: () => mpv('sub-seek', 1),
  speed_down: () => changeSpeed(-0.1),
  speed_up: () => changeSpeed(0.1),
  speed_reset: () => { mpv('set_property', 'speed', 1); osd('1.0x'); },
  ab_set_a: () => setABPoint('a'),
  ab_set_b: () => setABPoint('b'),
  ab_nudge_a_back: () => nudgeABPoint('a', -0.1),
  ab_nudge_b_back: () => nudgeABPoint('b', -0.1),
  ab_nudge_a_fwd: () => nudgeABPoint('a', 0.1),
  ab_nudge_b_fwd: () => nudgeABPoint('b', 0.1),
  sentence_loop: () => toggleSentenceLoop(),
  follow_mode: () => toggleFollow(),
  toggle_translation: () => toggleZh(),
  select_current: () => {
    if (state.currentIdx >= 0) {
      state.selected.add(state.currentIdx);
      syncSelectionUI();
      osd(`已选中 #${state.lines[state.currentIdx].number}`);
    }
  },
  sub_delay_minus: () => adjustSubDelay(-state.delayStep),
  sub_delay_plus: () => adjustSubDelay(state.delayStep),
  sub_delay_minus_coarse: () => adjustSubDelay(-0.5),
  sub_delay_plus_coarse: () => adjustSubDelay(0.5),
  sub_delay_reset: () => adjustSubDelay(0, true),
  toggle_panel: () => {
    const cur = state.settings?.window?.mini_bar ?? true;
    if (state.settings) state.settings.window.mini_bar = !cur;
    document.body.classList.toggle('mini', cur); // 关闭自动收放时立即展开
    osd(cur ? '自动收放 关' : '自动收放 开');
  },
  recall_mpv: () => invoke('recall_mpv').then(() => osd('已召回 mpv')).catch(osd),
};

document.addEventListener('keydown', (e) => {
  if (['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target.tagName)) return;
  if ((e.ctrlKey || e.metaKey) && e.key === 'c') return; // 让位给复制
  const combo = window.comboOf(e);
  const action = state.hotkeyMap[combo];
  if (action && actions[action]) {
    e.preventDefault();
    actions[action]();
  }
});

// ---------- 复制英文（从数据层序列化，绝不包含中文） ----------
document.addEventListener('keydown', (e) => {
  if (!(e.ctrlKey || e.metaKey) || e.key !== 'c') return;
  if (state.selected.size === 0) return;
  e.preventDefault();
  const idxs = [...state.selected].sort((a, b) => a - b);
  const lines = idxs.map((i) => state.lines[i].text).join('\n');
  const out = state.copyTemplate.replace('{lines}', lines);
  navigator.clipboard.writeText(out).then(() => osd(`已复制 ${idxs.length} 句英文`));
});

// ---------- 窗口行为：失焦沉底 / 切回召回 mpv ----------
let blurredAt = 0;
listen('tauri://blur', () => {
  blurredAt = Date.now();
  if (state.settings?.window?.sink_on_blur) {
    invoke('set_always_on_top', { flag: false }).catch(() => {});
  }
});
listen('tauri://focus', () => {
  if (!state.settings?.window?.recall_mpv_on_focus) return;
  if (Date.now() - blurredAt < 2000) return; // 短暂离开不召回
  invoke('set_always_on_top', { flag: true }).catch(() => {});
  invoke('recall_mpv').catch(() => {}); // mpv 未拉起时静默忽略
});

// ---------- 启动 / 设置热加载 ----------
function applySettings(s) {
  state.settings = s;
  state.hotkeyMap = {};
  for (const [action, combo] of Object.entries(s.hotkeys ?? {})) {
    state.hotkeyMap[combo] = action;
  }
  if (s.copy?.template) state.copyTemplate = s.copy.template;
  if (s.subtitle?.delay_step_ms) state.delayStep = s.subtitle.delay_step_ms / 1000;
}

window.addEventListener('settings-saved', (e) => applySettings(e.detail));

(async () => {
  try {
    applySettings(await invoke('get_settings'));
  } catch (e) {
    console.warn('settings load failed', e);
  }
})();
