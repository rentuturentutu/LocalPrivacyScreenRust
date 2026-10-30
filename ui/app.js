(() => {
  'use strict';

  const invoke = window.__TAURI__.core.invoke;
  const $ = (id) => document.getElementById(id);

  const MOD_ALT = 1;
  const MOD_CONTROL = 2;
  const MOD_SHIFT = 4;
  const MOD_WIN = 8;
  const MODIFIER_KEYS = new Set([16, 17, 18, 91, 92, 93, 229]);
  const SPECIAL_KEYS = {
    8: 'Backspace', 9: 'Tab', 13: 'Enter', 19: 'Pause', 20: 'CapsLock', 27: 'Esc',
    32: 'Space', 33: 'PageUp', 34: 'PageDown', 35: 'End', 36: 'Home',
    37: 'Left', 38: 'Up', 39: 'Right', 40: 'Down', 44: 'PrintScreen',
    45: 'Insert', 46: 'Delete', 106: 'Num*', 107: 'Num+', 109: 'Num-',
    110: 'Num.', 111: 'Num/', 144: 'NumLock', 145: 'ScrollLock',
    186: ';', 187: '=', 188: ',', 189: '-', 190: '.', 191: '/', 192: '`',
    219: '[', 220: '\\', 221: ']', 222: "'"
  };

  let settings = null;
  let recording = false;
  let busy = false;
  let wasRunning = false;
  let toastTimer = 0;

  function keyName(vk) {
    if ((vk >= 0x30 && vk <= 0x39) || (vk >= 0x41 && vk <= 0x5a)) return String.fromCharCode(vk);
    if (vk >= 0x60 && vk <= 0x69) return 'Num' + (vk - 0x60);
    if (vk >= 0x70 && vk <= 0x87) return 'F' + (vk - 0x6f);
    return SPECIAL_KEYS[vk] || ('VK' + vk.toString(16).toUpperCase().padStart(2, '0'));
  }

  function hotkeyLabel(modifiers, vk) {
    const parts = [];
    if (modifiers & MOD_CONTROL) parts.push('Ctrl');
    if (modifiers & MOD_ALT) parts.push('Alt');
    if (modifiers & MOD_SHIFT) parts.push('Shift');
    if (modifiers & MOD_WIN) parts.push('Win');
    parts.push(keyName(vk));
    return parts.join('+');
  }

  function setStatus(message, kind) {
    const node = $('statusText');
    node.textContent = message;
    node.className = 'status' + (kind ? ' ' + kind : '');
  }

  function showToast(message, isError) {
    const node = $('toast');
    node.textContent = message;
    node.className = 'toast' + (isError ? ' error' : '');
    node.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { node.hidden = true; }, isError ? 5000 : 2500);
  }

  function errorText(error) {
    return typeof error === 'string' ? error : (error && error.message) || String(error);
  }

  async function save() {
    try {
      await invoke('persist_settings', { settings });
      setStatus('设置已保存', 'ok');
    } catch (error) {
      setStatus(errorText(error), 'error');
    }
  }

  function renderHotkey() {
    const label = settings.hotkey.label;
    $('hotkeyLabel').textContent = recording ? '请按下组合键…' : label;
    $('heroHotkey').textContent = label;
    $('hotkeyButton').classList.toggle('recording', recording);
  }

  function renderFit() {
    for (const input of document.querySelectorAll('input[name="fit"]')) {
      input.checked = input.value === settings.fit;
    }
    $('preview').classList.remove('cover', 'contain', 'stretch');
    $('preview').classList.add(settings.fit);
    for (const screen of [$('miniScreen'), $('miniScreen2')]) {
      screen.classList.remove('contain', 'stretch');
      if (settings.fit !== 'cover') screen.classList.add(settings.fit);
    }
  }

  async function renderBackground() {
    const path = settings.backgroundPath;
    const image = $('previewImage');
    const screens = [$('miniScreen'), $('miniScreen2')];
    $('backgroundPath').textContent = path || '未选择图片，使用纯黑背景';
    $('backgroundPath').title = path;
    $('blackButton').disabled = !path;
    if (!path) {
      image.hidden = true;
      image.removeAttribute('src');
      $('previewEmpty').hidden = false;
      screens.forEach((screen) => { screen.style.backgroundImage = ''; });
      return;
    }
    try {
      const url = await invoke('preview_background', { path });
      if (settings.backgroundPath !== path) return;
      image.src = url;
      image.hidden = false;
      $('previewEmpty').hidden = true;
      screens.forEach((screen) => { screen.style.backgroundImage = 'url("' + url + '")'; });
    } catch (error) {
      image.hidden = true;
      $('previewEmpty').hidden = false;
      $('previewEmpty').textContent = '无法预览图片';
      showToast(errorText(error), true);
    }
  }

  function render() {
    renderHotkey();
    renderFit();
    return renderBackground();
  }

  function startRecording() {
    if (busy) return;
    recording = true;
    renderHotkey();
    setStatus('正在录制快捷键，按 Esc 取消');
  }

  function stopRecording() {
    recording = false;
    renderHotkey();
  }

  function onKeyDown(event) {
    if (!recording) return;
    event.preventDefault();
    event.stopPropagation();
    const vk = event.keyCode;
    if (vk === 27 && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey) {
      stopRecording();
      setStatus('已取消录制');
      return;
    }
    if (MODIFIER_KEYS.has(vk) || vk <= 0 || vk > 0xff) return;
    let modifiers = 0;
    if (event.ctrlKey) modifiers |= MOD_CONTROL;
    if (event.altKey) modifiers |= MOD_ALT;
    if (event.shiftKey) modifiers |= MOD_SHIFT;
    if (event.metaKey) modifiers |= MOD_WIN;
    if (!modifiers) {
      setStatus('请至少同时按住 Ctrl、Alt、Shift 或 Win 中的一个', 'error');
      return;
    }
    settings.hotkey = { modifiers, virtualKey: vk, label: hotkeyLabel(modifiers, vk) };
    stopRecording();
    save();
  }

  async function pickBackground() {
    try {
      const path = await invoke('pick_background');
      if (!path) return;
      settings.backgroundPath = path;
      await renderBackground();
      await save();
    } catch (error) {
      showToast(errorText(error), true);
    }
  }

  async function useBlack() {
    settings.backgroundPath = '';
    $('previewEmpty').textContent = '纯黑背景';
    await renderBackground();
    await save();
  }

  function onFitChange(event) {
    if (event.target.name !== 'fit') return;
    settings.fit = event.target.value;
    renderFit();
    save();
  }

  function setBusy(value) {
    busy = value;
    for (const id of ['startButton', 'pickButton', 'hotkeyButton', 'resetButton']) {
      $(id).disabled = value;
    }
  }

  async function startPrivacy() {
    if (recording) stopRecording();
    setBusy(true);
    try {
      await invoke('start_privacy', { settings });
      wasRunning = true;
      setStatus('隐私屏运行中');
    } catch (error) {
      setBusy(false);
      showToast(errorText(error), true);
      setStatus(errorText(error), 'error');
    }
  }

  async function pollStatus() {
    try {
      const status = await invoke('privacy_status');
      if (status.running) {
        wasRunning = true;
        if (!busy) setBusy(true);
      } else if (wasRunning) {
        wasRunning = false;
        setBusy(false);
        if (status.error) {
          showToast(status.error, true);
          setStatus(status.error, 'error');
        } else {
          setStatus('隐私屏已退出', 'ok');
        }
      }
    } catch (_) {
      // The window may be hidden while the privacy screen runs.
    }
  }

  async function resetSettings() {
    try {
      settings = await invoke('reset_settings');
      $('previewEmpty').textContent = '纯黑背景';
      await render();
      setStatus('已恢复默认设置', 'ok');
    } catch (error) {
      showToast(errorText(error), true);
    }
  }

  async function openLog() {
    try {
      await invoke('open_log');
      setStatus('已打开运行日志', 'ok');
    } catch (error) {
      showToast(errorText(error), true);
    }
  }

  async function init() {
    $('minimizeButton').addEventListener('click', () => invoke('minimize_window'));
    $('closeButton').addEventListener('click', () => invoke('close_app'));
    $('hotkeyButton').addEventListener('click', () => (recording ? stopRecording() : startRecording()));
    $('hotkeyButton').addEventListener('blur', () => { if (recording) stopRecording(); });
    $('pickButton').addEventListener('click', pickBackground);
    $('blackButton').addEventListener('click', useBlack);
    $('fitGroup').addEventListener('change', onFitChange);
    $('startButton').addEventListener('click', startPrivacy);
    $('logButton').addEventListener('click', openLog);
    $('resetButton').addEventListener('click', resetSettings);
    window.addEventListener('keydown', onKeyDown, true);
    document.addEventListener('contextmenu', (event) => event.preventDefault());

    try {
      settings = await invoke('get_settings');
    } catch (error) {
      settings = {
        hotkey: { modifiers: MOD_CONTROL | MOD_SHIFT, virtualKey: 0x50, label: 'Ctrl+Shift+P' },
        backgroundPath: '',
        fit: 'cover'
      };
      showToast(errorText(error), true);
    }
    await render();
    setInterval(pollStatus, 500);
  }

  init();
})();
