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
  loading: false,
  currentIdx: -1,
  selected: new Set(),
  lastClickIdx: -1,
  sentenceLoop: false,
  followMode: false,
  followPausedIdx: -1,
    showZh: false,
    zhReveal: new Set(), // 单句翻开译文的字幕 number（全局关闭时仍可逐句查看）
  settings: null,
  hotkeyMap: {},       // combo -> action（由 settings.hotkeys 反转）
  copyTemplate: '请逐句讲解以下美剧台词中的生词、短语和口语用法：\n\n{lines}',
  delayStep: 0.1,
  subDelay: 0,
  subSpeed: 1,
};

// 禁用 WebView2 默认右键菜单（播放器 UI 不应露浏览器菜单）；输入框保留编辑菜单
//（后续若在字幕单词上加自定义右键功能，在这里放行或接管）
document.addEventListener('contextmenu', (e) => {
  if (e.target.closest('input, textarea, [contenteditable]')) return;
  e.preventDefault();
});

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
  // 单窗口形态下通知条收在抽屉里（关上时不可见），视频上同步一份 OSD 纯文本
  if (singleMode) osd(bar.textContent.trim());
}

// 顶栏加载提示：顶栏不与原生 mpv 视频 HWND 重叠，所以抽屉关闭、首次尚未
// 启动 mpv、或切换视频时都能看到。错误保留数秒，避免失败信息也藏在抽屉里。
let videoLoadStatusTimer = null;
function showVideoLoadStatus(text, error = false) {
  if (videoLoadStatusTimer) {
    clearTimeout(videoLoadStatusTimer);
    videoLoadStatusTimer = null;
  }
  const indicator = $('#video-load-indicator');
  const label = $('#video-load-text');
  label.textContent = String(text);
  label.title = String(text);
  indicator.classList.toggle('error', error);
  indicator.classList.remove('hidden');
  document.body.classList.toggle('video-loading', !error);
  if (error) {
    videoLoadStatusTimer = setTimeout(hideVideoLoadStatus, 6000);
  }
}

function hideVideoLoadStatus() {
  if (videoLoadStatusTimer) {
    clearTimeout(videoLoadStatusTimer);
    videoLoadStatusTimer = null;
  }
  $('#video-load-indicator').classList.add('hidden');
  $('#video-load-indicator').classList.remove('error');
  document.body.classList.remove('video-loading');
}

$('#video-load-indicator').addEventListener('click', () => {
  if ($('#video-load-indicator').classList.contains('error')) hideVideoLoadStatus();
});

// ---------- 视频 / 字幕加载 ----------
async function loadVideo(path) {
  if (state.loading) return; // 加载中，防止重复触发
  state.loading = true;
  let failed = false;
  const fileName = path.split(/[\\/]/).pop() || path;
  showVideoLoadStatus(`正在打开 ${fileName}…`);
  showNotice('<span class="dim">正在打开视频…</span>');
  try {
    const res = await invoke('load_video', { path });
    state.videoPath = path;
    state.videoHash = res.video_hash;
    state.lines = res.lines;
    state.translations = {};
    state.currentIdx = -1;
    state.selected.clear();
    markConnected();
    // libmpv 属性跨 loadfile 保留；先清掉上一视频的对齐值，再恢复当前视频记录。
    state.subDelay = 0;
    state.subSpeed = 1;
    await mpv('set_property', 'sub-delay', 0);
    await mpv('set_property', 'sub-speed', 1);
    await applySavedSyncOffset();
    renderList();
    if (singleMode) invoke('webview_ready').catch(() => {}); // 视频加载完成后抬顶+落位视频层
    $('#trans-bar').classList.toggle('hidden', state.lines.length === 0);
    if (res.notice) {
      showNotice(`${esc(res.notice)} <button id="notice-search">去搜索</button>`);
      $('#notice-search').addEventListener('click', openSearchPanel);
    } else {
      showNotice(null);
      osd(`字幕来源：${res.source === 'cache' ? '缓存' : '内嵌提取'}`);
      loadCachedTranslation();
    }
    if (res.resume_s >= 5) osd(`已从 ${fmtTime(res.resume_s * 1000)} 续播`);
  } catch (e) {
    failed = true;
    const message = '加载视频失败: ' + String(e);
    showNotice(esc(message));
    showVideoLoadStatus(message, true);
  } finally {
    if (!failed) hideVideoLoadStatus();
    state.loading = false;
    refreshHistory();
  }
}

// 📂 原生文件对话框选视频（Rust 侧 pick_video 调起）
$('#btn-browse').addEventListener('click', async () => {
  try {
    const path = await invoke('pick_video');
    if (path) loadVideo(path);
  } catch (e) {
    showNotice('打开文件对话框失败: ' + esc(String(e)));
  }
});

// macOS Finder“打开方式”/拖到 Dock 图标；冷启动另由 take_startup_video
// 兜底。重复的同一路径不重载，加载中的事件也由 loadVideo 自身去重。
listen('open-video', (e) => {
  const path = String(e.payload ?? '');
  if (path && path !== state.videoPath) loadVideo(path);
});

// 后端分阶段推送加载进度（探测/提取字幕/启动播放器），首次打开不再“卡死”
listen('video-load-progress', (e) => {
  const message = String(e.payload);
  showNotice(`<span class="dim">${esc(message)}</span>`);
  if (state.loading) showVideoLoadStatus(message);
});

// 历史记录下拉（页面内渲染，原生 select 弹出层会被置顶面板盖住）
async function refreshHistory() {
  let entries = [];
  try {
    entries = await invoke('get_history');
  } catch {
    return;
  }
  // 当前文件名移到历史面板头部（顶栏历史入口已图标按钮化）
  $('#history-panel .panel-title').textContent = state.videoPath
    ? `最近播放（当前：${state.videoPath.split(/[\\/]/).pop()}）`
    : '最近播放';
  $('#history-list').innerHTML = entries
    .map(
      (e) =>
        `<div class="history-item" data-path="${esc(e.path)}" title="${esc(e.path)}">${esc(e.path.split(/[\\/]/).pop())}</div>`
    )
    .join('');
}

// 历史记录为全铺面板（同搜索面板）：下拉浮层在单窗口形态会被 mpv 渲染层盖住
$('#btn-history').addEventListener('click', () => {
  $('#history-panel').classList.toggle('hidden');
});
$('#btn-history-close').addEventListener('click', () => $('#history-panel').classList.add('hidden'));
$('#btn-fit').addEventListener('click', () => fitVideoWindow());

$('#history-list').addEventListener('click', (e) => {
  const item = e.target.closest('.history-item');
  if (!item) return;
  $('#history-panel').classList.add('hidden');
  const p = item.dataset.path;
  if (p && p !== state.videoPath) loadVideo(p);
});

// 拖入视频文件即加载（Tauri 默认拦截文件拖放并转发为 tauri://drag-drop 事件）
listen('tauri://drag-drop', (e) => {
  const paths = e.payload?.paths;
  if (paths && paths.length > 0) loadVideo(paths[0]);
});

// 播放位置记忆：每 5s 上报一次（异常退出最多丢 5s）
setInterval(async () => {
  if (!state.connected || !state.videoPath) return;
  const pos = await mpv('get_property', 'time-pos');
  if (typeof pos === 'number' && pos > 0) {
    invoke('save_playback_position', { positionS: pos }).catch(() => {});
  }
}, 5000);

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
        <span class="time" title="点击跳转到这句">${fmtTime(l.start_ms)}</span>
        <span class="text" title="点击显示/隐藏这句翻译">${esc(l.text)}${zh ? `<span class="zh${state.zhReveal.has(l.number) ? ' reveal' : ''}">${esc(zh)}</span>` : ''}</span>
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
  } else if (e.target.closest('.time')) {
    // 点时间戳：跳转播放这句
    state.lastClickIdx = idx;
    seekToLine(idx);
    return;
  } else {
    // 点文字：只切换该句译文，不打断播放。
    // 拖选复制后松手也会派生 click，选区非折叠时忽略，防误切换
    const sel = window.getSelection();
    if (sel && !sel.isCollapsed) return;
    toggleLineZh(row, idx);
    state.lastClickIdx = idx;
    return;
  }
  state.lastClickIdx = idx;
  syncSelectionUI();
});

// 切换单句译文显示（点文字 / Ctrl+F 触发；全局译文关闭时该句仍可见）
function toggleLineZh(row, idx) {
  const num = state.lines[idx]?.number;
  if (num == null) return;
  state.zhReveal.has(num) ? state.zhReveal.delete(num) : state.zhReveal.add(num);
  const zhEl = row.querySelector('.zh');
  if (zhEl) zhEl.classList.toggle('reveal', state.zhReveal.has(num));
}

// 开关当前句译文（快捷键）：复用单句翻开机制；该句无译文时提示不动
function toggleCurrentZh() {
  const l = state.lines[state.currentIdx];
  if (!l) return;
  if (!state.translations[l.number]) return osd('当前句暂无译文');
  const row = listEl.querySelector(`.line[data-idx="${state.currentIdx}"]`);
  if (row) toggleLineZh(row, state.currentIdx);
}

function syncSelectionUI() {
  listEl.querySelectorAll('.line').forEach((el) => {
    el.classList.toggle('selected', state.selected.has(Number(el.dataset.idx)));
  });
}

// 句子底纹表示程序的数据选区（V / Ctrl+点击 / Shift+点击），与浏览器拖蓝的
// 原生文字选区是两套状态。用户开始拖选文字时清掉旧的数据选区，避免画面上
// 同时出现“268 有底纹、270 文字被拖蓝”却在 Ctrl+C 时优先复制 268 的歧义。
listEl.addEventListener('mousedown', (e) => {
  if (!e.target.closest('.text') || e.ctrlKey || e.metaKey || e.shiftKey) return;
  if (state.selected.size === 0) return;
  state.selected.clear();
  state.lastClickIdx = -1;
  syncSelectionUI();
});

function subtitleToMediaSeconds(ms) {
  const speed = state.subSpeed > 0 ? state.subSpeed : 1;
  return ms / 1000 * speed + state.subDelay;
}

function mediaToSubtitleMs(seconds) {
  const speed = state.subSpeed > 0 ? state.subSpeed : 1;
  return (seconds - state.subDelay) / speed * 1000;
}

function seekToLine(idx) {
  const l = state.lines[idx];
  if (!l) return;
  state.followPausedIdx = -1;
  mpv('seek', Math.max(0, subtitleToMediaSeconds(l.start_ms)), 'absolute');
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

// ---------- 画面底部进度条（单窗口模式）：点击/拖动 seek，轮询跟新 ----------
const progressFill = $('#progress-fill');
let progressDragging = false;
function seekToClientX(clientX) {
  const r = $('#progress-bar').getBoundingClientRect();
  if (r.width <= 0) return;
  const pct = Math.min(100, Math.max(0, (clientX - r.left) / r.width * 100));
  progressFill.style.width = pct + '%';
  mpv('seek', pct.toFixed(2), 'absolute-percent');
}
$('#progress-bar').addEventListener('mousedown', (e) => {
  if (!singleMode) return;
  progressDragging = true;
  seekToClientX(e.clientX);
  e.preventDefault();
});
document.addEventListener('mousemove', (e) => { if (progressDragging) seekToClientX(e.clientX); });
document.addEventListener('mouseup', () => { progressDragging = false; });

// ---------- 轮询播放状态 ----------
setInterval(async () => {
  if (!state.connected) return;
  const paused = await mpv('get_property', 'pause');
  const pos = await mpv('get_property', 'time-pos');
  const delay = await mpv('get_property', 'sub-delay');
  const subSpeed = await mpv('get_property', 'sub-speed');
  if (typeof delay === 'number') state.subDelay = delay;
  if (typeof subSpeed === 'number' && subSpeed > 0) state.subSpeed = subSpeed;
  // 进度条跟新（拖动中由拖动逻辑接管，避免覆盖打架）
  if (singleMode && !progressDragging) {
    const pct = await mpv('get_property', 'percent-pos');
    if (typeof pct === 'number') progressFill.style.width = pct + '%';
  }
  if (typeof pos === 'number') {
    if (singleMode) $('#pos-time').textContent = fmtTime(pos * 1000);
    if (state.lines.length > 0) {
      // 面板字幕也必须使用 mpv 的 sub-delay/sub-speed 时间映射，否则 macOS
      // 默认面板模式下“自动对齐”和延迟微调只改了隐藏字幕，界面毫无变化。
      const subPosMs = mediaToSubtitleMs(pos);
      setCurrent(findCurrent(subPosMs));
      if (state.followMode && state.currentIdx >= 0) {
        const line = state.lines[state.currentIdx];
        if (line && subPosMs >= line.end_ms && state.followPausedIdx !== state.currentIdx) {
          state.followPausedIdx = state.currentIdx;
          mpv('set_property', 'pause', true);
          osd('跟读暂停');
        }
      }
    }
  }
  const speed = await mpv('get_property', 'speed');
  if (typeof speed === 'number') $('#speed-label').textContent = speed.toFixed(1) + 'x';
  updateBadges(paused, state.subDelay);
}, 300);

// ---------- 状态徽章（点击即关闭对应功能） ----------
async function updateBadges(paused, delay) {
  // 未设置 AB 点时 mpv 侧读 ab-loop-a 报错，catch 兜底为 null（badge 不显示）
  const abA = await mpv('get_property', 'ab-loop-a').catch(() => null);
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
    case 'ab': clearAB(); break;
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

// 清除 AB 循环（badge 点击 / 取消 AB 快捷键 / 单句循环关闭共用）
function clearAB() {
  state.sentenceLoop = false;
  mpv('set_property', 'ab-loop-a', 'no');
  mpv('set_property', 'ab-loop-b', 'no');
}

async function setABPoint(which) {
  // time-pos 在 seek/加载落地前的极短窗口内不可用：原实现静默 return 无提示，
  // 用户以为设上了（尤其 A 端），随后 B 设了也不循环——“第一次失效、
  // 第二次必成功”的头号嫌疑。此处必须发声。
  const pos = await mpv('get_property', 'time-pos').catch(() => null);
  if (typeof pos !== 'number') return osd('播放位置未就绪，请再按一次');
  // 顺序校验：mpv 仅在 a < b 时循环，反序设置静默不生效。
  // 反序时拒绝设置并明示，另一端未设时读取报错 catch 为 null（反序流可设）
  const other = await mpv('get_property', `ab-loop-${which === 'a' ? 'b' : 'a'}`).catch(() => null);
  if (typeof other === 'number') {
    if (which === 'b' && pos <= other) return osd(`B 点须在 A 点（${other.toFixed(1)}s）之后`);
    if (which === 'a' && pos >= other) return osd(`A 点须在 B 点（${other.toFixed(1)}s）之前`);
  }
  await mpv('set_property', `ab-loop-${which}`, pos);
  // 回读两端实际值并报告循环激活状态：mpv 仅在 a、b 皆设且 a<b 时循环。
  // “设了没循环”时此提示直接暴露原因（另一端未设/被清），不再靠猜。
  const [ra, rb] = await Promise.all([
    mpv('get_property', 'ab-loop-a').catch(() => null),
    mpv('get_property', 'ab-loop-b').catch(() => null),
  ]);
  const fmt = (v) => (typeof v === 'number' ? v.toFixed(1) : '—');
  const active = typeof ra === 'number' && typeof rb === 'number' && ra < rb;

  // B 通常取自“当前播放位置”。从读取 time-pos 到属性真正写入之间画面仍在
  // 前进，写完时播放头可能已经越过 B；mpv 不保证为这种“设置时已在界外”的
  // 情况补发一次回跳。B 成功激活后明确跳到 A，首轮立即开始且行为确定。
  // 旧版 250ms 监视器会等到越过 B+0.8s 后把 B 改成更晚的当前位置，正是
  // 延迟约两秒并出现“AB 未激活已重设B”的原因，现已彻底移除。
  if (active && which === 'b') {
    await mpv('seek', ra, 'absolute+exact');
  }
  osd(`${which.toUpperCase()}: ${pos.toFixed(1)}s｜A:${fmt(ra)} B:${fmt(rb)}${active ? '' : '（未循环）'}`);
}

async function nudgeABPoint(which, delta) {
  // 未设置时读取报错（DOUBLE 格式下无“no”值），catch 后静默退出
  const cur = await mpv('get_property', `ab-loop-${which}`).catch(() => null);
  if (typeof cur !== 'number') return;
  const next = Math.max(0, cur + delta);
  await mpv('set_property', `ab-loop-${which}`, next);
  osd(`${which.toUpperCase()}: ${next.toFixed(1)}s`);
}

async function toggleSentenceLoop() {
  if (state.sentenceLoop) {
    clearAB();
    osd('单句循环 关');
  } else {
    const l = state.lines[state.currentIdx];
    if (!l) return;
    await mpv('set_property', 'ab-loop-a', subtitleToMediaSeconds(l.start_ms));
    await mpv('set_property', 'ab-loop-b', subtitleToMediaSeconds(l.end_ms));
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
  state.subDelay = next;
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
    case 'autosync': autoSync(); break;
  }
});

// ---------- 字幕自动对齐（能量包络互相关；结果只写 mpv 属性，可逆） ----------
function fmtSyncOffset(delayS, speed) {
  return `${delayS >= 0 ? '+' : ''}${delayS.toFixed(2)}s` + (speed !== 1 ? ` ×${speed.toFixed(4)}` : '');
}

async function autoSync() {
  if (!state.videoPath || state.lines.length === 0) return osd('请先加载视频和字幕');
  const btn = document.querySelector('[data-act="autosync"]');
  btn.disabled = true;
  osd('正在分析音频并对齐字幕…');
  try {
    const r = await invoke('auto_sync_subtitles', {
      videoPath: state.videoPath,
      lines: state.lines,
      searchS: 30,
    });
    await mpv('set_property', 'sub-delay', r.delay_s);
    await mpv('set_property', 'sub-speed', r.speed);
    state.subDelay = r.delay_s;
    state.subSpeed = r.speed;
    await invoke('save_sync_offset', {
      videoHash: state.videoHash,
      offset: { delay_s: r.delay_s, speed: r.speed },
    });
    osd(`字幕已对齐（${r.segments_ok}/${r.segments_total} 段）：${fmtSyncOffset(r.delay_s, r.speed)}`);
  } catch (e) {
    osd('自动对齐失败: ' + e);
  } finally {
    btn.disabled = false;
  }
}

// 加载视频后恢复上次保存的对齐结果
async function applySavedSyncOffset() {
  try {
    const off = await invoke('get_sync_offset', { videoHash: state.videoHash });
    // speed 必须为正：0/负值会冻结 mpv 字幕时钟（字幕永不显示）；
    // 后端已对历史毒化配置免疫，此处再挡一道
    if (off && off.speed > 0) {
      await mpv('set_property', 'sub-delay', off.delay_s);
      await mpv('set_property', 'sub-speed', off.speed);
      state.subDelay = off.delay_s;
      state.subSpeed = off.speed;
      osd(`已恢复对齐：${fmtSyncOffset(off.delay_s, off.speed)}`);
    }
  } catch {
    /* 无保存结果或 mpv 未就绪：静默 */
  }
}

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
  ab_clear_a: () => { state.sentenceLoop = false; mpv('set_property', 'ab-loop-a', 'no'); osd('A 点 取消'); },
  ab_clear_b: () => { state.sentenceLoop = false; mpv('set_property', 'ab-loop-b', 'no'); osd('B 点 取消'); },
  ab_clear: () => { clearAB(); osd('AB循环 取消'); },
  sentence_loop: () => toggleSentenceLoop(),
  follow_mode: () => toggleFollow(),
  toggle_translation: () => toggleZh(),
  reveal_current_translation: () => toggleCurrentZh(),
  select_current: () => {
    if (state.currentIdx < 0) return;
    const idx = state.currentIdx;
    const number = state.lines[idx].number;
    const selected = state.selected.has(idx);
    selected ? state.selected.delete(idx) : state.selected.add(idx);
    state.lastClickIdx = idx;
    syncSelectionUI();
    osd(selected ? `已取消 #${number}` : `已选中 #${number}`);
  },
  copy_current: () => {
    if (state.currentIdx < 0) return;
    state.selected.add(state.currentIdx);
    syncSelectionUI();
    copySelected();
  },
  sub_delay_minus: () => adjustSubDelay(-state.delayStep),
  sub_delay_plus: () => adjustSubDelay(state.delayStep),
  sub_delay_minus_coarse: () => adjustSubDelay(-0.5),
  sub_delay_plus_coarse: () => adjustSubDelay(0.5),
  sub_delay_reset: () => adjustSubDelay(0, true),
  // macOS 控制面板始终完整常驻；该动作只保留给 Windows 字幕抽屉。
  toggle_panel: () => { if (singleMode) toggleDrawer(); },
  recall_mpv: () => invoke('recall_mpv').then(() => osd('已召回 mpv')).catch(osd),
  fit_video_window: () => fitVideoWindow(),
  anki_export: () => exportAnki(),
};

// macOS/Linux 的 mpv 视频窗获得焦点时，libmpv input section 将动作名通过
// client-message → Tauri 事件送回这里，因此与 WebView keydown 共用同一动作表。
listen('mpv-hotkey', (e) => {
  const action = String(e.payload ?? '');
  if (action && actions[action]) actions[action]();
});

// ---------- Anki 导出（K：截图+音频切片+双语文本 → AnkiConnect/兜底文件） ----------
async function exportAnki() {
  const l = state.lines[state.currentIdx];
  if (!l) return osd('没有当前句');
  if (!state.videoPath) return osd('请先加载视频');
  osd('正在导出到 Anki…');
  try {
    const msg = await invoke('export_anki_note', {
      videoHash: state.videoHash,
      videoPath: state.videoPath,
      line: l,
      zh: state.translations[l.number] ?? null,
    });
    osd(msg);
  } catch (e) {
    osd('Anki 导出失败: ' + e);
  }
}

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

// ---------- 复制英文 ----------
// 原生拖蓝文字的优先级最高：应复制用户眼前精确选中的字符，不套模板；仅当
// 没有原生文字选区时，Ctrl+C 才序列化 V / Ctrl+点击选中的整句数据。
function hasNativeTextSelection() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || sel.rangeCount === 0) return false;
  const range = sel.getRangeAt(0);
  const container = range.commonAncestorContainer.nodeType === Node.ELEMENT_NODE
    ? range.commonAncestorContainer
    : range.commonAncestorContainer.parentElement;
  return !!container?.closest?.('#sentence-list');
}

function copySelected() {
  if (state.selected.size === 0) return;
  const idxs = [...state.selected].sort((a, b) => a - b);
  const lines = idxs.map((i) => state.lines[i].text).join('\n');
  const out = state.copyTemplate.replace('{lines}', lines);
  navigator.clipboard.writeText(out).then(() => osd(`已复制 ${idxs.length} 句英文`));
}

document.addEventListener('keydown', (e) => {
  if (!(e.ctrlKey || e.metaKey) || e.key.toLowerCase() !== 'c') return;
  if (hasNativeTextSelection()) return; // 交给浏览器复制拖蓝的原文
  if (state.selected.size === 0) return;
  e.preventDefault();
  copySelected();
});

// 窗口重置为视频原始大小（顶栏 1:1 按钮 / 快捷键）：dwidth/dheight 为显示
// 像素（已含宽高比/旋转），尺寸计算与 set_size 由 Rust 侧完成（含工作区上限）
async function fitVideoWindow() {
  if (!state.connected) return;
  const w = await mpv('get_property', 'dwidth');
  const h = await mpv('get_property', 'dheight');
  if (typeof w !== 'number' || typeof h !== 'number' || w <= 0 || h <= 0) {
    return osd('视频尺寸不可用');
  }
  try {
    await invoke('fit_window_to_video', { w, h });
  } catch (e) {
    osd('调整失败: ' + e);
  }
}

// ---------- 窗口行为：失焦沉底 / 切回召回 mpv ----------
// 必须在异步注册 focus listener 前初始化，避免监听刚装好就回调时落入 TDZ。
let singleMode = false;
let drawerOpen = false;
let blurredAt = 0;
listen('tauri://blur', () => {
  blurredAt = Date.now();
  // Windows 单窗口从不置顶，也不要在失焦后反复下发 NOTOPMOST；后者会改变
  // 普通窗口的 Z 序，造成 Alt+Tab 已转移焦点但 loopSub 偶尔仍压在上面。
  if (!singleMode && state.settings?.window?.sink_on_blur) {
    invoke('set_always_on_top', { flag: false }).catch(() => {});
  }
});
listen('tauri://focus', () => {
  // Windows 已是视频内嵌的单顶层窗口，系统激活本身就会正常置前；沿用旧版
  // “悬浮面板”策略设为 topmost，会破坏 Alt+Tab 的标准 Z 序行为。
  if (singleMode) return;
  if (!state.settings?.window?.recall_mpv_on_focus) return;
  if (Date.now() - blurredAt < 2000) return; // 短暂离开不召回
  invoke('set_always_on_top', { flag: true }).catch(() => {});
  invoke('recall_mpv').catch(() => {}); // mpv 未拉起时静默忽略
});

// ---------- 单窗口模式（Windows：mpv 画面内嵌主窗口，学习面板收进右侧抽屉） ----------
function toggleDrawer() {
  drawerOpen = !drawerOpen;
  $('#drawer').classList.toggle('hidden', !drawerOpen);
  document.body.classList.toggle('drawer-open', drawerOpen);
  invoke('set_drawer', { open: drawerOpen }).catch(() => {});
}

async function initWindowMode() {
  try {
    singleMode = (await invoke('window_mode')) === 'single';
  } catch { return; }
  // 命令行参数和 macOS Finder Opened 冷启动路径在所有窗口形态都要消费；
  // 旧逻辑位于 singleMode 分支内，导致 macOS 收到路径后永远不打开。
  let startup = null;
  try {
    startup = await invoke('take_startup_video');
  } catch {}
  if (!singleMode) {
    if (startup) loadVideo(startup);
    return;
  }
  // 单窗口模式不需要悬浮面板的自动置顶；启动时也主动清掉可能由早到的
  // focus 事件或旧逻辑留下的 topmost 状态，保证鼠标与 Alt+Tab 行为一致。
  invoke('set_always_on_top', { flag: false }).catch(() => {});
  document.body.classList.add('single');
  // 面板元素搬入右侧抽屉（事件绑在元素上，搬移后保留）；通知条进抽屉顶部
  const drawer = $('#drawer');
  drawer.appendChild($('#notice-bar'));
  for (const sel of ['#status-badges', '#trans-bar', '#advanced', '#sentence-list', '#mini-bar', '#search-panel', '#history-panel', '#settings-drawer']) {
    drawer.appendChild($(sel));
  }
  // 播放控制提上顶栏（从 mini-bar 中提出，插到时间显示前）
  $('#top-bar').insertBefore($('#transport'), $('#pos-time'));
  $('#pos-time').classList.remove('hidden');
  const bp = $('#btn-panel');
  bp.classList.remove('hidden');
  bp.addEventListener('click', toggleDrawer);
  // 搜索/设置/历史的面板都在抽屉里：抽屉关着时点这些入口先自动开抽屉
  //（捕获阶段先执行，原处理逻辑照常走）
  for (const sel of ['#btn-history', '#btn-open-search', '#btn-settings']) {
    $(sel).addEventListener('click', () => { if (!drawerOpen) toggleDrawer(); }, true);
  }
  // 通知 Rust 侧 webview 已就绪：抬升并重排 mpv 子窗口
  invoke('webview_ready').catch(() => {});
  // 右键菜单“用 loopSub 播放”带入的启动视频
  if (startup) loadVideo(startup);
}

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
  await initWindowMode();
  try {
    applySettings(await invoke('get_settings'));
  } catch (e) {
    console.warn('settings load failed', e);
  }
  refreshHistory();
})();

