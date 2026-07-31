// loopSub 前端：面板遥控 mpv + 句子列表 + 英文复制
// 通过 withGlobalTauri 暴露的 window.__TAURI__.core.invoke 调用 Rust 命令
'use strict';

const { invoke } = window.__TAURI__.core;

// ---------- 全局状态 ----------
const state = {
  lines: [],          // [{number, start_ms, end_ms, text}]
  connected: false,
  currentIdx: -1,
  selected: new Set(),
  lastClickIdx: -1,
  sentenceLoop: false,
  copyTemplate: '请逐句讲解以下美剧台词中的生词、短语和口语用法：\n\n{lines}',
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

const osd = (text) => mpv('show-text', text, 1200);

// ---------- 连接与字幕加载 ----------
$('#btn-connect').addEventListener('click', async () => {
  const socketPath = $('#socket-path').value.trim();
  if (!socketPath) return;
  try {
    await invoke('mpv_connect', { socketPath });
    state.connected = true;
    $('#conn-status').textContent = '已连接';
    $('#conn-status').classList.add('ok');
  } catch (e) {
    $('#conn-status').textContent = '连接失败: ' + e;
  }
});

$('#btn-load').addEventListener('click', async () => {
  const path = $('#srt-path').value.trim();
  if (!path) return;
  try {
    state.lines = await invoke('load_srt', { path });
    state.currentIdx = -1;
    state.selected.clear();
    renderList();
  } catch (e) {
    alert('字幕加载失败: ' + e);
  }
});

// ---------- 句子列表 ----------
function fmtTime(ms) {
  const s = Math.floor(ms / 1000);
  const mm = String(Math.floor(s / 60)).padStart(2, '0');
  const ss = String(s % 60).padStart(2, '0');
  return `${mm}:${ss}`;
}

function renderList() {
  listEl.innerHTML = state.lines
    .map(
      (l, i) => `<div class="line" data-idx="${i}">
        <span class="no">${l.number}</span>
        <span class="time">${fmtTime(l.start_ms)}</span>
        <span class="text">${esc(l.text)}</span>
      </div>`
    )
    .join('');
}

listEl.addEventListener('click', (e) => {
  const row = e.target.closest('.line');
  if (!row) return;
  const idx = Number(row.dataset.idx);

  if (e.shiftKey && state.lastClickIdx >= 0) {
    // Shift+点击：选区间（复制用）
    const [a, b] = [Math.min(state.lastClickIdx, idx), Math.max(state.lastClickIdx, idx)];
    state.selected.clear();
    for (let i = a; i <= b; i++) state.selected.add(i);
  } else if (e.ctrlKey || e.metaKey) {
    // Ctrl+点击：切换单句选择
    state.selected.has(idx) ? state.selected.delete(idx) : state.selected.add(idx);
  } else {
    // 普通点击：跳转播放
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
  if (!state.connected || state.lines.length === 0) return;
  const pos = await mpv('get_property', 'time-pos');
  if (typeof pos === 'number') setCurrent(findCurrent(pos * 1000));
  const speed = await mpv('get_property', 'speed');
  if (typeof speed === 'number') $('#speed-label').textContent = speed.toFixed(1) + 'x';
  updateBadges();
}, 300);

async function updateBadges() {
  const paused = await mpv('get_property', 'pause');
  const abA = await mpv('get_property', 'ab-loop-a');
  const badges = [];
  if (paused) badges.push('暂停');
  if (typeof abA === 'number') badges.push('AB循环');
  if (state.sentenceLoop) badges.push('单句循环');
  $('#status-badges').textContent = badges.join(' · ');
}

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

$('#transport').addEventListener('click', (e) => {
  const act = e.target.dataset?.act;
  if (!act) return;
  switch (act) {
    case 'prev': mpv('sub-seek', -1); break;
    case 'toggle': mpv('cycle', 'pause'); break;
    case 'next': mpv('sub-seek', 1); break;
    case 'slower': changeSpeed(-0.1); break;
    case 'faster': changeSpeed(0.1); break;
  }
});

// ---------- 快捷键（面板内 web 事件，无需全局热键） ----------
document.addEventListener('keydown', (e) => {
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;
  if (e.key === 'c' && (e.ctrlKey || e.metaKey)) return; // 让位给复制

  switch (true) {
    case e.key === ' ':
      e.preventDefault(); mpv('cycle', 'pause'); break;
    case e.key === 'ArrowLeft' && !e.altKey:
      mpv('seek', -2, 'relative', 'exact'); break;
    case e.key === 'ArrowRight' && !e.altKey:
      mpv('seek', 2, 'relative', 'exact'); break;
    case e.key === 'ArrowUp':
      e.preventDefault(); mpv('sub-seek', -1); break;
    case e.key === 'ArrowDown':
      e.preventDefault(); mpv('sub-seek', 1); break;
    case e.key === 'x': changeSpeed(-0.1); break;
    case e.key === 'c': changeSpeed(0.1); break;
    case e.key === 'z':
      mpv('set_property', 'speed', 1); osd('1.0x'); break;
    case e.key === '[' && !e.ctrlKey && !e.altKey: setABPoint('a'); break;
    case e.key === ']' && !e.ctrlKey && !e.altKey: setABPoint('b'); break;
    case e.key === '[' && e.ctrlKey: e.preventDefault(); nudgeABPoint('a', -0.1); break;
    case e.key === ']' && e.ctrlKey: e.preventDefault(); nudgeABPoint('b', -0.1); break;
    case e.key === '[' && e.altKey: e.preventDefault(); nudgeABPoint('a', 0.1); break;
    case e.key === ']' && e.altKey: e.preventDefault(); nudgeABPoint('b', 0.1); break;
    case e.key === 'Enter': toggleSentenceLoop(); break;
  }
});

// ---------- 复制英文（数据层序列化，绝不包含中文） ----------
document.addEventListener('keydown', (e) => {
  if (!(e.ctrlKey || e.metaKey) || e.key !== 'c') return;
  if (state.selected.size === 0) return;
  e.preventDefault();
  const idxs = [...state.selected].sort((a, b) => a - b);
  const lines = idxs.map((i) => state.lines[i].text).join('\n');
  const out = state.copyTemplate.replace('{lines}', lines);
  navigator.clipboard.writeText(out).then(() => {
    osd(`已复制 ${idxs.length} 句英文`);
  });
});

// ---------- 启动 ----------
(async () => {
  try {
    const settings = await invoke('get_settings');
    if (settings?.copy?.template) state.copyTemplate = settings.copy.template;
  } catch (e) {
    console.warn('settings load failed', e);
  }
})();
