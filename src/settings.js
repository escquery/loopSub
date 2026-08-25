// loopSub 设置页：表单渲染、热键改绑捕获、保存
// 暴露 window.comboOf（键盘事件 → 组合键字符串）与 window.SettingsUI
'use strict';

// 键盘事件 → 规范化组合键，与 Rust 默认表格式一致：
// 修饰键顺序 ctrl → alt → shift → meta；单字符小写；空格记为 Space；
// 符号键按物理键（e.code）归一为基键字符——Shift+[ 的 e.key 是 {，而
// macOS 拼音输入法下裸 [ 也可能是【；绑定表统一记美式物理键位的 [。
const SHIFT_BASE_KEYS = {
  BracketLeft: '[', BracketRight: ']', Comma: ',', Period: '.', Slash: '/',
  Backslash: '\\', Semicolon: ';', Quote: "'", Minus: '-', Equal: '=', Backquote: '`',
  Digit1: '1', Digit2: '2', Digit3: '3', Digit4: '4', Digit5: '5',
  Digit6: '6', Digit7: '7', Digit8: '8', Digit9: '9', Digit0: '0',
};
window.comboOf = function (e) {
  const parts = [];
  if (e.ctrlKey) parts.push('ctrl');
  if (e.altKey) parts.push('alt');
  if (e.shiftKey) parts.push('shift');
  if (e.metaKey) parts.push('meta');
  let key = e.key;
  if (key === ' ') key = 'Space';
  // 标点键始终按物理键位归一。macOS 拼音输入法下裸 [ / ] 的 e.key 可能是
  // 全角【/】，此前只有带 Shift 时才归一，导致设置 A/B 无响应而取消却正常。
  else if (SHIFT_BASE_KEYS[e.code]) key = SHIFT_BASE_KEYS[e.code];
  else if (key.length === 1) key = key.toLowerCase();
  parts.push(key);
  return parts.join('+');
};

const ACTION_LABELS = {
  toggle_pause: '播放 / 暂停',
  seek_back: '快退 2 秒',
  seek_forward: '快进 2 秒',
  prev_sentence: '上一句',
  next_sentence: '下一句',
  speed_down: '减速 0.1',
  speed_up: '加速 0.1',
  speed_reset: '速度还原 1.0x',
  ab_set_a: '设置 A 点',
  ab_set_b: '设置 B 点',
  ab_nudge_a_back: 'A 点 −100ms',
  ab_nudge_b_back: 'B 点 −100ms',
  ab_nudge_a_fwd: 'A 点 +100ms',
  ab_nudge_b_fwd: 'B 点 +100ms',
  ab_clear_a: '取消 A 点',
  ab_clear_b: '取消 B 点',
  ab_clear: '取消 AB 循环',
  sentence_loop: '单句循环',
  follow_mode: '跟读模式',
  toggle_translation: '译文显隐',
  reveal_current_translation: '当前句译文显隐',
  select_current: '选中 / 取消当前句',
  sub_delay_minus: '字幕延迟 −0.1s',
  sub_delay_plus: '字幕延迟 +0.1s',
  sub_delay_minus_coarse: '字幕延迟 −0.5s',
  sub_delay_plus_coarse: '字幕延迟 +0.5s',
  sub_delay_reset: '字幕延迟归零',
  toggle_panel: '展开 / 收起字幕抽屉（Windows）',
  recall_mpv: '召回 mpv 窗口',
  fit_video_window: '窗口重置为视频大小',
  anki_export: '导出当前句到 Anki',
};

const tauriInvoke = window.__TAURI__.core.invoke;
const $s = (sel) => document.querySelector(sel);

const SettingsUI = {
  settings: null,
  capturing: null, // 正在改绑的 action 名

  async open() {
    this.settings = await tauriInvoke('get_settings');
    this.render();
    $s('#settings-drawer').classList.remove('hidden');
    // 右键菜单开关（仅 Windows 显示；状态以注册表为准，不落 settings.json）
    tauriInvoke('window_mode').then(async (mode) => {
      if (mode !== 'single') return;
      const on = await tauriInvoke('get_explorer_menu').catch(() => false);
      $s('#explorer-menu-section').hidden = false;
      $s('#explorer-menu').checked = on;
    });
  },

  close() {
    this.capturing = null;
    $s('#settings-drawer').classList.add('hidden');
  },

  render() {
    const s = this.settings;
    const esc = (v) => String(v ?? '').replace(/"/g, '&quot;');
    $s('#settings-body').innerHTML = `
      <section>
        <h3>OpenSubtitles（仅搜索字幕时需要）</h3>
        <label>API Key <input type="password" data-k="opensubtitles.api_key" value="${esc(s.opensubtitles.api_key)}" placeholder="留空则搜索不可用" /></label>
      </section>
      <section>
        <h3>大模型（仅翻译时需要）</h3>
        <label>Base URL <input data-k="llm.base_url" value="${esc(s.llm.base_url)}" placeholder="如 https://api.deepseek.com/v1" /></label>
        <label>模型名 <input data-k="llm.model" value="${esc(s.llm.model)}" placeholder="如 deepseek-chat" /></label>
        <label>API Key <input type="password" data-k="llm.api_key" value="${esc(s.llm.api_key)}" /></label>
        <label>场景间隙阈值（秒）<input type="number" step="1" data-k="llm.scene_threshold_s" value="${s.llm.scene_threshold_s}" /></label>
        <label>每批最小句数 <input type="number" data-k="llm.min_batch_lines" value="${s.llm.min_batch_lines}" /></label>
        <label>每批最大句数 <input type="number" data-k="llm.max_batch_lines" value="${s.llm.max_batch_lines}" /></label>
      </section>
      <section>
        <h3>音频</h3>
        <label class="row"><input type="checkbox" data-k="audio.dialogue_boost" ${s.audio.dialogue_boost ? 'checked' : ''} /> 对白增强（语音频段优化）</label>
        <label>音量上限 %（0 = 不限制）<input type="number" data-k="audio.volume_max" value="${s.audio.volume_max ?? 0}" /></label>
      </section>
      <section>
        <h3>字幕</h3>
        <label>大写还原
          <select data-k="subtitle.truecase">
            <option value="rule" ${s.subtitle.truecase === 'rule' ? 'selected' : ''}>规则法（默认）</option>
            <option value="llm" ${s.subtitle.truecase === 'llm' ? 'selected' : ''}>大模型转换</option>
          </select>
        </label>
        <label>渲染方式
          <select data-k="subtitle.render">
            <option value="panel" ${s.subtitle.render === 'panel' ? 'selected' : ''}>面板渲染（默认）</option>
            <option value="mpv" ${s.subtitle.render === 'mpv' ? 'selected' : ''}>mpv 渲染（ASS 特效）</option>
          </select>
        </label>
        <label>延迟微调步长（毫秒）<input type="number" data-k="subtitle.delay_step_ms" value="${s.subtitle.delay_step_ms}" /></label>
      </section>
      <section>
        <h3>复制</h3>
        <label>剪贴板模板（{lines} 为台词占位符）</label>
        <textarea data-k="copy.template" rows="3">${esc(s.copy.template)}</textarea>
      </section>
      <section>
        <h3>缓存</h3>
        <label>缓存目录（留空用系统默认）<input data-k="cache.dir" value="${esc(s.cache.dir)}" /></label>
      </section>
      <section>
        <h3>外部程序</h3>
        <label>mpv / ffmpeg 所在目录（安装版留空使用内置组件；源码运行会自动搜索 PATH 与 Homebrew）<input data-k="bins.dir" value="${esc(s.bins.dir)}" /></label>
      </section>
      <section>
        <h3>窗口</h3>
        <label class="row"><input type="checkbox" data-k="window.sink_on_blur" ${s.window.sink_on_blur ? 'checked' : ''} /> 切走时取消置顶（沉底）</label>
        <label class="row"><input type="checkbox" data-k="window.recall_mpv_on_focus" ${s.window.recall_mpv_on_focus ? 'checked' : ''} /> 切回时召回 mpv 窗口</label>
      </section>
      <section id="explorer-menu-section" hidden>
        <h3>系统集成</h3>
        <label class="row"><input type="checkbox" id="explorer-menu" /> 资源管理器右键菜单：视频文件“用 loopSub 播放”</label>
      </section>
      <section>
        <h3>Anki（需装 AnkiConnect 插件并启动 Anki）</h3>
        <label>牌组名 <input data-k="anki.deck" value="${esc(s.anki.deck)}" /></label>
        <label>标签（空格分隔） <input data-k="anki.tags" value="${esc(s.anki.tags)}" /></label>
        <label>AnkiConnect 地址 <input data-k="anki.connect_url" value="${esc(s.anki.connect_url)}" /></label>
      </section>
      <section>
        <h3>快捷键（点击组合键改绑）</h3>
        <table id="hotkey-table">
          ${Object.entries(ACTION_LABELS)
            .map(
              ([action, label]) => `<tr>
                <td>${label}</td>
                <td><button class="hk" data-action="${action}">${esc(s.hotkeys[action] ?? '未绑定')}</button></td>
              </tr>`
            )
            .join('')}
        </table>
      </section>`;
  },

  collect() {
    const s = this.settings;
    $s('#settings-body').querySelectorAll('[data-k]').forEach((el) => {
      const path = el.dataset.k.split('.');
      let obj = s;
      for (let i = 0; i < path.length - 1; i++) obj = obj[path[i]];
      const key = path[path.length - 1];
      if (el.type === 'checkbox') {
        obj[key] = el.checked;
      } else if (el.type === 'number') {
        const n = Number(el.value);
        if (key === 'volume_max') obj[key] = n > 0 ? n : null;
        else obj[key] = n;
      } else {
        obj[key] = el.value || null;
        if (key === 'template') obj[key] = el.value;
      }
    });
    return s;
  },
};

// ---------- 事件绑定 ----------
$s('#btn-settings').addEventListener('click', () => SettingsUI.open());
$s('#btn-settings-close').addEventListener('click', () => SettingsUI.close());

$s('#settings-body').addEventListener('click', (e) => {
  const btn = e.target.closest('.hk');
  if (!btn) return;
  // 进入捕获模式：下一次按键成为新绑定
  SettingsUI.capturing = btn.dataset.action;
  document.querySelectorAll('.hk.capturing').forEach((b) => b.classList.remove('capturing'));
  btn.classList.add('capturing');
  btn.textContent = '按下新组合键…';
});

// 捕获改绑（捕获阶段，抢在 main.js 热键分发之前）
document.addEventListener(
  'keydown',
  (e) => {
    if (!SettingsUI.capturing) return;
    e.preventDefault();
    e.stopPropagation();
    if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return; // 纯修饰键继续等
    const combo = window.comboOf(e);
    const action = SettingsUI.capturing;
    // 冲突检测：占用同一组合键的其他动作让位
    for (const [a, c] of Object.entries(SettingsUI.settings.hotkeys)) {
      if (c === combo && a !== action) delete SettingsUI.settings.hotkeys[a];
    }
    SettingsUI.settings.hotkeys[action] = combo;
    SettingsUI.capturing = null;
    SettingsUI.render();
  },
  true
);

$s('#btn-settings-save').addEventListener('click', async () => {
  try {
    const s = SettingsUI.collect();
    await tauriInvoke('save_settings', { settings: s });
    $s('#settings-msg').textContent = '已保存';
    // 通知主界面热加载
    window.dispatchEvent(new CustomEvent('settings-saved', { detail: s }));
    setTimeout(() => ($s('#settings-msg').textContent = ''), 2000);
  } catch (e) {
    $s('#settings-msg').textContent = '保存失败: ' + e;
  }
});

// 右键菜单开关：立即生效（写/删 HKCU 注册表），失败回滚勾选
$s('#settings-body').addEventListener('change', async (e) => {
  if (e.target.id !== 'explorer-menu') return;
  try {
    await tauriInvoke('set_explorer_menu', { enable: e.target.checked });
    $s('#settings-msg').textContent = e.target.checked ? '右键菜单已添加' : '右键菜单已移除';
    setTimeout(() => ($s('#settings-msg').textContent = ''), 2000);
  } catch (err) {
    e.target.checked = !e.target.checked;
    $s('#settings-msg').textContent = '右键菜单设置失败: ' + err;
  }
});

window.SettingsUI = SettingsUI;
