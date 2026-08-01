// loopSub 悬浮字幕条：透明置顶窗内的全部交互
// 数据全部经事件与主面板窗口互通（本窗口不 invoke 任何命令）：
//   收：floatbar:line（当前句双行文本）、floatbar:state（按钮高亮/播放图标/译文显隐）
//   发：floatbar:action（按钮动作，主面板 actions 表执行）、floatbar:moved（位置记忆）
'use strict';

const { listen, emit } = window.__TAURI__.event;
const $ = (sel) => document.querySelector(sel);

// ---------- 当前句 ----------
listen('floatbar:line', (e) => {
  const { en, zh } = e.payload || {};
  $('#sub-en').textContent = en || '';
  $('#sub-zh').textContent = zh || '';
  syncZhVisibility();
});

// 译文显隐 = 有译文且未被 toggle_translation 关闭（默认显示）
function syncZhVisibility() {
  const off = document.body.classList.contains('zh-off');
  $('#sub-zh').classList.toggle('hidden', off || !$('#sub-zh').textContent);
}

// ---------- 状态（按钮高亮 / 播放图标 / 译文显隐） ----------
listen('floatbar:state', (e) => {
  const s = e.payload || {};
  $('#fb-play').textContent = s.paused ? '▶' : '⏸';
  document.body.classList.toggle('zh-off', s.zh === false);
  for (const btn of document.querySelectorAll('[data-flag]')) {
    const flag = btn.dataset.flag;
    btn.classList.toggle('active', !!s[flag]);
  }
  syncZhVisibility();
});

// ---------- hover 展开 / 延迟收起 ----------
const bar = $('#bar');
let collapseTimer = null;
bar.addEventListener('mouseenter', () => {
  clearTimeout(collapseTimer);
  bar.classList.add('expanded');
});
bar.addEventListener('mouseleave', () => {
  clearTimeout(collapseTimer);
  collapseTimer = setTimeout(() => bar.classList.remove('expanded'), 700);
});

// ---------- 按钮 → 主面板 actions ----------
$('#controls').addEventListener('click', (e) => {
  const act = e.target.closest('button')?.dataset?.act;
  if (act) emit('floatbar:action', act);
});

// ---------- 位置记忆（拖动后防抖上报，主面板写设置） ----------
let moveTimer = null;
listen('tauri://move', (e) => {
  clearTimeout(moveTimer);
  moveTimer = setTimeout(() => {
    emit('floatbar:moved', { x: e.payload.x, y: e.payload.y });
  }, 800);
});

// 握手：listen 注册完毕后通知主面板推送当前句/状态快照（否则要等下一句变化）
emit('floatbar:ready', {});
