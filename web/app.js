'use strict';

/* ------------------------------------------------------------------ */
/* Utilities                                                           */
/* ------------------------------------------------------------------ */
const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

function escapeHtml(value) {
  return String(value ?? '').replace(/[&<>"']/g, (ch) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[ch]));
}

function icon(name) {
  return `<svg class="icon" aria-hidden="true"><use href="#i-${name}"></use></svg>`;
}

function debounce(fn, wait) {
  let timer;
  return (...args) => {
    clearTimeout(timer);
    timer = setTimeout(() => fn(...args), wait);
  };
}

function fmtBytes(bytes) {
  if (!bytes) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let index = 0;
  while (value >= 1024 && index < units.length - 1) { value /= 1024; index += 1; }
  return `${value >= 10 || index === 0 ? Math.round(value) : value.toFixed(1)} ${units[index]}`;
}

function fmtDuration(ms) {
  const total = Math.max(0, Math.round((ms || 0) / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours > 0) return `${hours}:${String(minutes).padStart(2, '0')}:${String(seconds).padStart(2, '0')}`;
  return `${minutes}:${String(seconds).padStart(2, '0')}`;
}

function fmtNumber(value) {
  return new Intl.NumberFormat('zh-CN').format(value || 0);
}

function pct(value) {
  return `${Math.round((Number(value) || 0) * 100)}%`;
}

function plural(count, one, many) {
  return `${fmtNumber(count)} ${count === 1 ? one : many}`;
}

function splitMulti(value, separators) {
  let values = [String(value ?? '')];
  for (const separator of separators || []) {
    if (!separator) continue;
    values = values.flatMap((item) => item.split(separator));
  }
  const out = [];
  for (const item of values) {
    const trimmed = item.trim();
    if (trimmed && !out.includes(trimmed)) out.push(trimmed);
  }
  return out;
}

function coverUrl(id) {
  return `/api/tracks/${id}/cover`;
}

function statusLabel(text, kind = '') {
  return `<span class="mini-flag ${kind}">${escapeHtml(text)}</span>`;
}

/* ------------------------------------------------------------------ */
/* Global state                                                        */
/* ------------------------------------------------------------------ */
const state = {
  view: 'library',
  tracks: [],
  trackMap: new Map(),
  total: 0,
  offset: 0,
  limit: 100,
  q: '',
  filter: '',
  sort: 'artist',
  selected: new Set(),
  stats: null,
  settings: null,
  settingsDirty: false,
  scan: null,
  scanWasRunning: false,
  renameItems: [],
  renameOk: false,
  renameTimer: null,
  lyricsCandidates: [],
  activeLyric: 0,
  currentTrack: null,
  scrapeCandidates: [],
  scrapeTrackId: null,
  scrapeSources: new Set(['musicbrainz', 'itunes', 'deezer']),
  naming: null,
  loadedTrackId: null,
  loadingCount: 0,
  pollHandle: null,
  modalLocked: false,
};

/* ------------------------------------------------------------------ */
/* API / feedback                                                      */
/* ------------------------------------------------------------------ */
async function api(path, options = {}) {
  const method = options.method || 'GET';
  const init = { method, credentials: 'same-origin', headers: {} };
  if (options.body !== undefined) {
    init.headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(options.body);
  }
  const response = await fetch(path, init);
  const contentType = response.headers.get('content-type') || '';
  let data = null;
  if (contentType.includes('application/json')) {
    data = await response.json().catch(() => null);
  } else {
    data = await response.text().catch(() => '');
  }
  if (response.status === 401 && !path.startsWith('/api/auth/')) {
    showLogin();
    throw new Error('登录已过期，请重新登录');
  }
  if (!response.ok) {
    throw new Error((data && data.error) || `请求失败（HTTP ${response.status}）`);
  }
  return data;
}

function toast(message, kind = 'success', timeout = 3600) {
  const container = $('#toast-container');
  const node = document.createElement('div');
  node.className = `toast ${kind}`;
  const iconName = kind === 'error' ? 'alert' : kind === 'warn' ? 'alert' : 'check';
  node.innerHTML = `${icon(iconName)}<span>${escapeHtml(message)}</span>`;
  container.appendChild(node);
  // Every toast owns its timer. A global timer caused earlier toasts (for
  // example "扫描已开始") to lose their dismissal when a newer toast appeared.
  let dismissed = false;
  const dismiss = () => {
    if (dismissed) return;
    dismissed = true;
    clearTimeout(timer);
    node.classList.add('toast-out');
    setTimeout(() => node.remove(), 220);
  };
  const timer = setTimeout(dismiss, timeout);
  node.addEventListener('click', dismiss);
  while (container.children.length > 5) {
    container.firstElementChild?.remove();
  }
}

function showLoading(text = '处理中…') {
  state.loadingCount += 1;
  $('#loading-text').textContent = text;
  $('#loading-overlay').classList.remove('hidden');
}

function hideLoading() {
  state.loadingCount = Math.max(0, state.loadingCount - 1);
  if (state.loadingCount === 0) $('#loading-overlay').classList.add('hidden');
}

async function withLoading(text, fn) {
  showLoading(text);
  try { return await fn(); } finally { hideLoading(); }
}

/* ------------------------------------------------------------------ */
/* Auth / boot                                                         */
/* ------------------------------------------------------------------ */
async function checkAuth() {
  let status;
  try {
    status = await api('/api/auth/status');
  } catch (error) {
    showLogin();
    const node = $('#login-error');
    node.textContent = error.message;
    node.classList.remove('hidden');
    return;
  }
  state.authEnabled = status.enabled;
  if (status.enabled && !status.authenticated) {
    showLogin();
  } else {
    showApp();
    bootData().catch((error) => toast(error.message, 'error', 6000));
  }
}

function showLogin() {
  closeModal();
  state.loadingCount = 0;
  state.selected.clear();
  $('#loading-overlay').classList.add('hidden');
  if (state.pollHandle) {
    clearInterval(state.pollHandle);
    state.pollHandle = null;
  }
  $('#login-screen').classList.remove('hidden');
  $('#app-shell').classList.add('hidden');
  setTimeout(() => $('#login-password')?.focus(), 80);
}

function showApp() {
  $('#login-screen').classList.add('hidden');
  $('#app-shell').classList.remove('hidden');
  $('#btn-logout').classList.toggle('hidden', !state.authEnabled);
}

async function bootData() {
  await Promise.all([loadSettings(), loadNaming()]);
  await Promise.all([loadStats(), loadTracks()]);
  await refreshScanStatus();
  if (!state.pollHandle) {
    state.pollHandle = setInterval(refreshScanStatus, 1200);
  }
}

async function refreshScanStatus() {
  try {
    const scan = await api('/api/scan/status');
    const running = !!scan.running;
    state.scan = scan;
    updateScanUI(scan);
    if (state.scanWasRunning && !running) {
      if (scan.phase === 'done') {
        toast(`扫描完成：新增 ${scan.added || 0}，更新 ${scan.updated || 0}，移除 ${scan.removed || 0}`);
      } else if (scan.phase === 'cancelled') {
        toast('扫描已取消', 'warn');
      } else if (scan.phase === 'error') {
        toast(`扫描失败：${scan.error || '未知错误'}`, 'error', 6000);
      }
      state.selected.clear();
      await Promise.all([loadStats(), loadTracks()]);
    }
    state.scanWasRunning = running;
  } catch (_) {
    /* polling failures should stay quiet */
  }
}

function updateScanUI(scan) {
  const running = !!scan.running;
  state.scan = scan;
  $('#scan-mini').classList.toggle('hidden', !running);
  $('#top-progress').classList.toggle('hidden', !running);
  $('#scan-dot').classList.toggle('live', running);
  $$('#btn-scan, #side-scan, #empty-action').forEach((button) => {
    if (button) button.disabled = running;
  });
  if (running) {
    const percent = scan.total > 0 ? Math.min(100, Math.round((scan.done / scan.total) * 100)) : 0;
    $('#scan-percent').textContent = `${percent}%`;
    $('#scan-phase').textContent = scan.phase === 'counting' ? '统计文件' : scan.phase === 'cleaning' ? '清理索引' : '扫描中';
    $('#scan-bar').style.width = `${percent}%`;
    $('#top-progress-bar').style.width = `${percent}%`;
    $('#scan-file').textContent = scan.current_file || `${fmtNumber(scan.done)} / ${fmtNumber(scan.total)}`;
    if (scan.phase === 'counting') {
      $('#scan-bar').style.width = '8%';
      $('#top-progress-bar').style.width = '8%';
    }
  }
}

async function startScan() {
  try {
    await api('/api/scan', { method: 'POST', body: {} });
    state.scanWasRunning = true;
    toast('扫描已开始');
    await refreshScanStatus();
  } catch (error) {
    toast(error.message, 'error');
  }
}

/* ------------------------------------------------------------------ */
/* Settings                                                            */
/* ------------------------------------------------------------------ */
async function loadSettings() {
  const data = await api('/api/settings');
  state.settings = data;
  populateSettings();
}

function populateSettings() {
  if (!state.settings) return;
  const config = state.settings.config;
  $('#info-version').textContent = state.settings.version || '—';
  $('#info-data').textContent = state.settings.data_dir || '—';
  $('#info-auth').textContent = state.settings.auth_enabled ? '已启用' : '未启用';
  $('#info-template').textContent = config.naming_template || '—';
  $('#info-template').title = config.naming_template || '';

  $('#tag-separator').value = config.tag_separator || '; ';
  $('#scrape-separator').value = config.scrape_separator || '/';
  $('#split-separators').value = (config.split_separators || []).join(' ');
  const separators = config.separators || {};
  $('#sep-artists').value = separators.artists || '';
  $('#sep-albumartist').value = separators.albumartist || '';
  $('#sep-genres').value = separators.genres || '';
  $('#sep-composers').value = separators.composers || '';
  $('#sep-lyricists').value = separators.lyricists || '';
  $('#naming-artist-separator').value = config.naming_artist_separator || '';
  if ($('#rename-artist-separator') && !$('#rename-artist-separator').value) {
    $('#rename-artist-separator').value = config.naming_artist_separator || '';
  }
  $('#lyrics-bilingual').checked = !!config.lyrics.bilingual;
  $('#lyrics-embed').checked = !!config.lyrics.embed;
  $('#lyrics-synced').checked = !!config.lyrics.prefer_synced;
  $('#lyrics-overwrite').checked = !!config.lyrics.overwrite;
  $('#p-musicbrainz').checked = !!config.providers.musicbrainz;
  $('#p-itunes').checked = !!config.providers.itunes;
  $('#p-deezer').checked = !!config.providers.deezer;
  $('#p-lrclib').checked = !!config.providers.lrclib;
  $('#p-netease').checked = !!config.providers.netease;
  const endpoints = config.endpoints || {};
  ENDPOINT_FIELDS.forEach(([id, key]) => {
    const input = $(`#${id}`);
    if (input) input.value = endpoints[key] || input.dataset.default;
  });
  $('#match-threshold').value = Math.round((config.auto_match_threshold || 0.62) * 100);
  $('#threshold-value').textContent = `${$('#match-threshold').value}%`;
  $('#itunes-country').value = config.itunes_country || 'cn';
  if ($('#rename-template') && !$('#rename-template').value) {
    $('#rename-template').value = config.naming_template || '{artist}/{album}/{track:02d} - {title}';
  }
  renderRoots(config.library_roots || []);
  state.settingsDirty = false;
  $('#settings-dirty').classList.add('hidden');
}

function renderRoots(roots) {
  const container = $('#roots-list');
  if (!roots.length) {
    container.innerHTML = `<div class="empty-inline" style="padding:24px 12px">还没有音乐目录，添加容器内路径，例如 /music。</div>`;
    return;
  }
  container.innerHTML = roots.map((root, index) => `
    <div class="root-row">
      ${icon('folder')}
      <span title="${escapeHtml(root)}">${escapeHtml(root)}</span>
      <button class="icon-btn" data-remove-root="${index}" title="移除">${icon('close')}</button>
    </div>`).join('');
  $$('[data-remove-root]', container).forEach((button) => {
    button.addEventListener('click', () => {
      const current = state.settings.config.library_roots;
      current.splice(Number(button.dataset.removeRoot), 1);
      renderRoots(current);
      markSettingsDirty();
    });
  });
}

function markSettingsDirty() {
  state.settingsDirty = true;
  $('#settings-dirty').classList.remove('hidden');
}

function settingsFromForm() {
  const config = JSON.parse(JSON.stringify(state.settings.config));
  config.library_roots = [...(state.settings.config.library_roots || [])];
  config.tag_separator = $('#tag-separator').value;
  config.scrape_separator = $('#scrape-separator').value || '/';
  config.separators = {
    ...(config.separators || {}),
    artists: $('#sep-artists').value,
    albumartist: $('#sep-albumartist').value,
    genres: $('#sep-genres').value,
    composers: $('#sep-composers').value,
    lyricists: $('#sep-lyricists').value,
  };
  config.naming_artist_separator = $('#naming-artist-separator').value;
  config.split_separators = $('#split-separators').value
    .split(/\s+/).map((v) => v.trim()).filter(Boolean);
  config.lyrics.bilingual = $('#lyrics-bilingual').checked;
  config.lyrics.embed = $('#lyrics-embed').checked;
  config.lyrics.prefer_synced = $('#lyrics-synced').checked;
  config.lyrics.overwrite = $('#lyrics-overwrite').checked;
  config.providers.musicbrainz = $('#p-musicbrainz').checked;
  config.providers.itunes = $('#p-itunes').checked;
  config.providers.deezer = $('#p-deezer').checked;
  config.providers.lrclib = $('#p-lrclib').checked;
  config.providers.netease = $('#p-netease').checked;
  config.endpoints = { ...(config.endpoints || {}) };
  ENDPOINT_FIELDS.forEach(([id, key]) => {
    config.endpoints[key] = $(`#${id}`).value.trim();
  });
  config.auto_match_threshold = Number($('#match-threshold').value) / 100;
  config.itunes_country = $('#itunes-country').value;
  return config;
}

async function saveSettings() {
  const config = settingsFromForm();
  if (!config.library_roots.length) {
    toast('请至少添加一个音乐目录', 'warn');
    return;
  }
  const badEndpoint = ENDPOINT_FIELDS.find(([id]) => {
    const value = $(`#${id}`).value.trim();
    return value && !/^https?:\/\//i.test(value);
  });
  if (badEndpoint) {
    toast('API 地址必须以 http:// 或 https:// 开头', 'warn');
    return;
  }
  try {
    const result = await withLoading('保存设置…', () => api('/api/settings', { method: 'PUT', body: config }));
    state.settings.config = result.config;
    state.settingsDirty = false;
    $('#settings-dirty').classList.add('hidden');
    populateSettings();
    toast('设置已保存');
    await loadStats();
    await loadTracks();
  } catch (error) {
    toast(error.message, 'error', 6000);
  }
}

/* ------------------------------------------------------------------ */
/* Dashboard / library                                                 */
/* ------------------------------------------------------------------ */
async function loadStats() {
  try {
    const stats = await api('/api/stats');
    state.stats = stats;
    renderStats(stats);
    $('#side-tracks').textContent = fmtNumber(stats.total);
    $('#side-albums').textContent = fmtNumber(stats.albums);
  } catch (error) {
    console.warn(error);
  }
}

function renderStats(stats) {
  const coverage = stats.total ? Math.round((stats.with_lyrics / stats.total) * 100) : 0;
  const cards = [
    { label: '曲目总数', value: fmtNumber(stats.total), desc: `${fmtBytes(stats.total_size)} · ${fmtDuration(stats.total_duration_ms)}`, icon: 'disc', color: '#72e6b1' },
    { label: '专辑 / 艺术家', value: fmtNumber(stats.albums), desc: `${fmtNumber(stats.artists)} 位艺术家`, icon: 'library', color: '#7dd3fc' },
    { label: '缺失核心标签', value: fmtNumber(stats.missing_core), desc: stats.missing_core ? '标题、歌手或专辑为空' : '标签完整度很好', icon: 'alert', color: stats.missing_core ? '#fbbf24' : '#72e6b1' },
    { label: '歌词覆盖', value: `${coverage}%`, desc: `${fmtNumber(stats.with_lyrics)} 首有歌词 · ${fmtNumber(stats.with_cover)} 首有封面`, icon: 'lyrics', color: '#a78bfa' },
  ];
  $('#stats-grid').innerHTML = cards.map((card) => `
    <div class="stat-card" style="--stat-color:${card.color}">
      <div class="stat-top"><span class="stat-label">${card.label}</span><span class="stat-icon">${icon(card.icon)}</span></div>
      <div class="stat-value">${card.value}</div>
      <div class="stat-desc">${escapeHtml(card.desc)}</div>
    </div>`).join('');
}

function trackQuery() {
  const params = new URLSearchParams();
  params.set('limit', state.limit);
  params.set('offset', state.offset);
  params.set('sort', state.sort);
  if (state.q) params.set('q', state.q);
  if (state.filter) params.set('filter', state.filter);
  return params.toString();
}

async function loadTracks() {
  try {
    const page = await api(`/api/tracks?${trackQuery()}`);
    state.tracks = page.items;
    state.total = page.total;
    state.trackMap = new Map(page.items.map((track) => [track.id, track]));
    renderTracks();
    updateSelectionUI();
    updatePagination();
  } catch (error) {
    toast(error.message, 'error');
  }
}

function renderTracks() {
  const body = $('#tracks-body');
  const empty = $('#empty-state');
  if (!state.tracks.length) {
    body.innerHTML = '';
    empty.classList.remove('hidden');
    const noLibrary = !state.total && !state.q && !state.filter;
    $('#empty-title').textContent = noLibrary ? '音乐库还是空的' : '没有匹配的歌曲';
    $('#empty-text').textContent = noLibrary
      ? '在设置中填写 NAS 音乐目录，然后点一下扫描。'
      : '换个关键词或筛选条件试试。';
    $('#empty-action').textContent = noLibrary ? '开始扫描' : '清除筛选';
    $('#empty-action').dataset.action = noLibrary ? 'scan' : 'clear';
    $('#library-count').textContent = noLibrary ? '等待扫描' : '0 首';
    return;
  }
  empty.classList.add('hidden');
  $('#library-count').textContent = `${fmtNumber(state.total)} 首`;
  body.innerHTML = state.tracks.map((track) => {
    const selected = state.selected.has(track.id);
    const cover = track.has_cover
      ? `<img src="${coverUrl(track.id)}" alt="" loading="lazy" onerror="this.remove()">`
      : icon('disc');
    const statuses = [
      track.has_lyrics ? `<span class="status-pill ok" title="有歌词">${icon('lyrics')}</span>` : `<span class="status-pill" title="无歌词">${icon('lyrics')}</span>`,
      track.has_cover ? `<span class="status-pill ok" title="有封面">${icon('folder')}</span>` : `<span class="status-pill" title="无封面">${icon('folder')}</span>`,
      track.missing_core ? `<span class="status-pill warn" title="标签缺失">${icon('alert')}</span>` : '',
    ].join('');
    const trackNumber = track.track
      ? `${String(track.track).padStart(2, '0')}${track.track_total ? `<span style="color:var(--muted-2)"> / ${track.track_total}</span>` : ''}`
      : '—';
    return `
      <tr data-id="${track.id}" class="${selected ? 'selected' : ''}">
        <td class="check-col"><input type="checkbox" data-select="${track.id}" ${selected ? 'checked' : ''} aria-label="选择 ${escapeHtml(track.title || track.filename)}"></td>
        <td class="cover-col"><div class="cover-thumb">${cover}</div></td>
        <td class="title-col">
          <div class="track-title-cell">
            <span class="track-title" data-open="${track.id}" title="${escapeHtml(track.title || track.filename)}">${escapeHtml(track.title || track.filename || '未命名')}</span>
            <span class="track-artist">${escapeHtml(track.artist || '未知歌手')}${track.year ? ` · ${track.year}` : ''}<span class="mobile-duration"> · ${escapeHtml(track.duration || '0:00')}</span></span>
          </div>
        </td>
        <td class="album-col"><div class="album-cell" title="${escapeHtml(track.album || '')}">${escapeHtml(track.album || '—')}</div></td>
        <td class="track-col"><span class="track-num">${trackNumber}</span></td>
        <td class="time-col"><span class="track-num">${track.duration || '0:00'}</span></td>
        <td class="format-col"><span class="format-badge">${escapeHtml(track.format || track.ext || '—')}</span></td>
        <td class="status-col"><div class="status-row">${statuses}</div></td>
        <td class="action-col"><button class="icon-btn row-actions" data-open="${track.id}" title="编辑">${icon('edit')}</button></td>
      </tr>`;
  }).join('');
}

function updatePagination() {
  const from = state.total ? state.offset + 1 : 0;
  const to = Math.min(state.offset + state.limit, state.total);
  $('#page-range').textContent = `${from}–${to} / ${fmtNumber(state.total)}`;
  const pages = Math.max(1, Math.ceil(state.total / state.limit));
  const current = Math.floor(state.offset / state.limit) + 1;
  $('#page-number').textContent = `${current} / ${pages}`;
  $('#prev-page').disabled = current <= 1;
  $('#next-page').disabled = current >= pages;
}

function updateSelectionUI() {
  const count = state.selected.size;
  $('#selected-count').textContent = fmtNumber(count);
  $('#batchbar').classList.toggle('has-selection', count > 0);
  const buttons = ['#btn-edit-selected', '#btn-auto-scrape', '#btn-lyrics-selected', '#btn-rename-selected', '#btn-delimiter'];
  buttons.forEach((selector) => { $(selector).disabled = count === 0; });
  $('#btn-edit-selected').disabled = count !== 1;
  const pageIds = state.tracks.map((track) => track.id);
  const allSelected = pageIds.length > 0 && pageIds.every((id) => state.selected.has(id));
  $('#select-page').checked = allSelected;
  $('#select-page-head').checked = allSelected;
  $('#rename-selected-count').textContent = `${count} 首`;
}

function toggleSelection(id, selected) {
  if (selected) state.selected.add(id); else state.selected.delete(id);
  const row = $(`tr[data-id="${id}"]`);
  if (row) row.classList.toggle('selected', selected);
  updateSelectionUI();
}

function selectPage(selected) {
  state.tracks.forEach((track) => {
    if (selected) state.selected.add(track.id); else state.selected.delete(track.id);
  });
  renderTracks();
  updateSelectionUI();
}

function setFilter(filter) {
  state.filter = filter;
  state.offset = 0;
  state.selected.clear();
  $$('#filter-chips .chip').forEach((chip) => chip.classList.toggle('active', chip.dataset.filter === filter));
  loadTracks();
}

function setQuery(query) {
  state.q = query.trim();
  state.offset = 0;
  state.selected.clear();
  loadTracks();
}

const debouncedQuery = debounce(() => setQuery($('#global-search').value), 320);

/* ------------------------------------------------------------------ */
/* Navigation                                                          */
/* ------------------------------------------------------------------ */
function setView(view) {
  state.view = view;
  $$('.nav-item').forEach((item) => item.classList.toggle('active', item.dataset.view === view));
  $$('.mobile-nav-item').forEach((item) => {
    item.classList.toggle('active', item.dataset.mobileView === view);
  });
  $$('.view').forEach((section) => section.classList.add('hidden'));
  $(`#view-${view}`).classList.remove('hidden');
  $('#search-wrap').classList.toggle('hidden', view !== 'library');
  if (view === 'library') {
    $('#page-title').textContent = '音乐库';
    $('#page-subtitle').textContent = '读取、刮削、翻译、重命名，一处完成。';
    $('#btn-scan').classList.remove('hidden');
  } else if (view === 'rename') {
    $('#page-title').textContent = '批量重命名';
    $('#page-subtitle').textContent = '用元数据整理目录和文件名，先预览再执行。';
    $('#btn-scan').classList.remove('hidden');
    renderRenamePage();
  } else if (view === 'settings') {
    $('#page-title').textContent = '设置';
    $('#page-subtitle').textContent = '目录、分隔符、歌词与刮削源。';
    $('#btn-scan').classList.add('hidden');
    loadSettings().catch((error) => toast(error.message, 'error'));
  }
  document.querySelector('.content')?.scrollTo({ top: 0 });
}

/* ------------------------------------------------------------------ */
/* Track editor                                                        */
/* ------------------------------------------------------------------ */
async function openEditor(id) {
  let track = state.trackMap.get(id);
  if (!track) {
    try { track = await api(`/api/tracks/${id}`); } catch (error) { toast(error.message, 'error'); return; }
  }
  state.currentTrack = track;
  const config = state.settings?.config;
  const separator = config?.tag_separator || '; ';
  const cover = track.has_cover
    ? `<img src="${coverUrl(id)}" alt="" onerror="this.remove()">`
    : icon('disc');
  const body = `
    <form id="track-form" autocomplete="off">
      <div class="editor-hero">
        <div class="editor-cover">${cover}</div>
        <div class="editor-meta">
          <h2>${escapeHtml(track.title || track.filename || '未命名')}</h2>
          <p>${escapeHtml(track.artist || '未知歌手')} · ${escapeHtml(track.album || '未知专辑')}</p>
          <div class="editor-chips">
            <span class="mini-badge">${escapeHtml(track.format || '—')}</span>
            <span class="mini-badge">${track.duration || '0:00'}</span>
            <span class="mini-badge">${track.bitrate ? `${track.bitrate} kbps` : '—'}</span>
            ${track.year ? `<span class="mini-badge">${track.year}</span>` : ''}
          </div>
          <audio class="editor-audio" controls preload="none" src="/api/tracks/${id}/stream"></audio>
        </div>
      </div>
      <div class="form-grid editor">
        <label class="field span-2"><span>标题</span><input data-field="title" value="${escapeHtml(track.title)}" placeholder="歌曲标题"></label>
        <label class="field"><span>艺术家（显示）</span><input data-field="artist" value="${escapeHtml(track.artist || '')}" placeholder="ARTIST / TPE1"><small>单个显示署名，例如 “周杰伦” 或 “A &amp; B”。</small></label>
        <label class="field"><span>专辑艺术家</span><input data-field="albumartist" value="${escapeHtml(track.album_artist)}" placeholder="Album Artist"></label>
        <label class="field span-2"><span>艺术家列表（多值）</span><input data-field="artists" value="${escapeHtml((track.artists || []).join(separator))}" placeholder="多个艺术家用分隔符隔开"><small>ARTISTS 标签；识别分隔符：${escapeHtml((config?.split_separators || []).join(' '))}</small></label>
        <label class="field span-2"><span>专辑</span><input data-field="album" value="${escapeHtml(track.album)}" placeholder="专辑名"></label>
        <label class="field"><span>音轨号</span><input data-field="track" type="number" min="0" value="${track.track ?? ''}" placeholder="1"></label>
        <label class="field"><span>音轨总数</span><input data-field="track_total" type="number" min="0" value="${track.track_total ?? ''}" placeholder="12"></label>
        <label class="field"><span>碟号</span><input data-field="disc" type="number" min="0" value="${track.disc ?? ''}" placeholder="1"></label>
        <label class="field"><span>碟总数</span><input data-field="disc_total" type="number" min="0" value="${track.disc_total ?? ''}" placeholder="1"></label>
        <label class="field"><span>发行日期</span><input data-field="date" value="${escapeHtml(track.date)}" placeholder="2005-11-01"></label>
        <label class="field"><span>流派</span><input data-field="genres" value="${escapeHtml(track.genre)}" placeholder="流行；华语"></label>
        <label class="field"><span>作曲</span><input data-field="composers" value="${escapeHtml((track.composers || []).join(separator))}" placeholder="作曲者"></label>
        <label class="field"><span>作词</span><input data-field="lyricists" value="${escapeHtml((track.lyricists || []).join(separator))}" placeholder="作词者"></label>
        <label class="field span-2"><span>注释</span><input data-field="comment" value="${escapeHtml(track.comment)}" placeholder="备注 / Comment"></label>
      </div>
      <details class="advanced">
        <summary>更多标签与标识</summary>
        <div class="form-grid editor">
          <label class="field"><span>BPM</span><input data-field="bpm" type="number" min="0" value="${track.bpm ?? ''}" placeholder="120"></label>
          <label class="field"><span>ISRC</span><input data-field="isrc" value="${escapeHtml(track.isrc)}" placeholder="CNS..."></label>
          <label class="field"><span>唱片公司</span><input data-field="label" value="${escapeHtml(track.label)}" placeholder="Label"></label>
          <label class="field"><span>目录号</span><input data-field="catalog_number" value="${escapeHtml(track.catalog_number)}" placeholder="Catalog No."></label>
          <label class="field"><span>MusicBrainz Recording ID</span><input data-field="mb_track_id" class="mono" value="${escapeHtml(track.mb_track_id)}"></label>
          <label class="field"><span>MusicBrainz Release ID</span><input data-field="mb_album_id" class="mono" value="${escapeHtml(track.mb_album_id)}"></label>
          <label class="field span-2"><span>MusicBrainz Artist ID</span><input data-field="mb_artist_id" class="mono" value="${escapeHtml(track.mb_artist_id)}"></label>
        </div>
      </details>
      <div style="color:var(--muted-2);font-size:10.5px;margin-top:16px;word-break:break-all">${escapeHtml(track.rel_path || track.path)}</div>
    </form>`;
  const footer = `
    <button class="btn btn-ghost" data-editor-action="lyrics">${icon('lyrics')}歌词</button>
    <button class="btn btn-ghost" data-editor-action="scrape">${icon('wand')}刮削</button>
    <span style="flex:1"></span>
    <button class="btn btn-ghost" data-close-modal>取消</button>
    <button class="btn btn-primary" id="save-track">${icon('check')}保存标签</button>`;
  openModal({ title: '编辑歌曲', subtitle: '修改会直接写入音频文件', body, footer });
  $('#save-track').addEventListener('click', () => saveTrack(id));
  $$('[data-editor-action]', $('#modal-root')).forEach((button) => {
    button.addEventListener('click', () => {
      closeModal();
      if (button.dataset.editorAction === 'scrape') openScrape([id]);
      if (button.dataset.editorAction === 'lyrics') openLyricsForTrack(id);
    });
  });
}

async function saveTrack(id) {
  const form = $('#track-form');
  const value = (name) => $(`[data-field="${name}"]`, form)?.value?.trim() ?? '';
  const split = (name) => splitMulti(value(name), state.settings?.config?.split_separators || []);
  const patch = {
    title: value('title'),
    artist: value('artist'),
    artists: split('artists'),
    albumartist: split('albumartist'),
    album: value('album'),
    track: value('track'),
    track_total: value('track_total'),
    disc: value('disc'),
    disc_total: value('disc_total'),
    date: value('date'),
    genres: split('genres'),
    composers: split('composers'),
    lyricists: split('lyricists'),
    comment: value('comment'),
    bpm: value('bpm'),
    isrc: value('isrc'),
    label: value('label'),
    catalog_number: value('catalog_number'),
    mb_track_id: value('mb_track_id'),
    mb_album_id: value('mb_album_id'),
    mb_artist_id: value('mb_artist_id'),
  };
  try {
    await withLoading('写入标签…', () => api(`/api/tracks/${id}`, { method: 'PATCH', body: patch }));
    toast('标签已写入文件');
    closeModal();
    await Promise.all([loadTracks(), loadStats()]);
  } catch (error) {
    toast(error.message, 'error', 6000);
  }
}

/* ------------------------------------------------------------------ */
/* Metadata scraping                                                   */
/* ------------------------------------------------------------------ */
function providerLabel(source) {
  return ({
    musicbrainz: 'MusicBrainz',
    itunes: 'iTunes',
    deezer: 'Deezer',
    netease: '网易云',
  }[source] || source);
}

const METADATA_SOURCES = ['musicbrainz', 'itunes', 'deezer', 'netease'];
const ENDPOINT_FIELDS = [
  ['ep-musicbrainz', 'musicbrainz'],
  ['ep-coverart', 'coverart'],
  ['ep-itunes', 'itunes'],
  ['ep-deezer', 'deezer'],
  ['ep-lrclib', 'lrclib'],
  ['ep-netease', 'netease'],
];

async function openScrape(ids) {
  if (!ids.length) return;
  const id = ids[0];
  let track = state.trackMap.get(id);
  if (!track) {
    try { track = await api(`/api/tracks/${id}`); } catch (error) { toast(error.message, 'error'); return; }
  }
  state.scrapeTrackId = id;
  state.scrapeCandidates = [];
  const providers = state.settings?.config?.providers || {};
  state.scrapeSources = new Set(METADATA_SOURCES.filter((name) => providers[name] !== false));
  const body = `
    <div class="scrape-query">
      <label class="field"><span>歌曲标题</span><input id="scrape-title" value="${escapeHtml(track.title)}" placeholder="必填"></label>
      <label class="field"><span>歌手</span><input id="scrape-artist" value="${escapeHtml(track.artist)}" placeholder="可选"></label>
      <label class="field"><span>专辑</span><input id="scrape-album" value="${escapeHtml(track.album)}" placeholder="可选"></label>
      <button class="btn btn-primary" id="run-scrape">${icon('search')}搜索</button>
    </div>
    <div class="scrape-toolbar">
      <div class="source-toggles" id="scrape-sources">
        ${METADATA_SOURCES.map((name) => `<button class="source-toggle ${state.scrapeSources.has(name) ? 'on' : ''}" data-source="${name}"><i class="toggle-check"></i>${providerLabel(name)}</button>`).join('')}
      </div>
      <div style="display:flex;gap:14px;flex-wrap:wrap">
        <label class="check-label"><input type="checkbox" id="scrape-overwrite" checked>覆盖已有标签</label>
        <label class="check-label"><input type="checkbox" id="scrape-no-lyrics" checked>只改元数据（不生成歌词）</label>
      </div>
    </div>
    <div id="scrape-results" class="candidate-list">
      <div class="empty-inline">输入关键词后搜索，或使用「智能刮削」自动匹配。</div>
    </div>`;
  const footer = `<span style="flex:1;color:var(--muted-2);font-size:10.5px">${escapeHtml(track.rel_path)}</span><button class="btn btn-ghost" data-close-modal>关闭</button>`;
  openModal({ title: '刮削元数据', subtitle: '从开放音乐数据库匹配并覆盖标签', body, footer, wide: true });
  $$('#scrape-sources [data-source]').forEach((button) => {
    button.addEventListener('click', () => {
      const name = button.dataset.source;
      if (state.scrapeSources.has(name)) state.scrapeSources.delete(name);
      else state.scrapeSources.add(name);
      button.classList.toggle('on', state.scrapeSources.has(name));
    });
  });
  $('#run-scrape').addEventListener('click', () => runScrapeSearch(id));
  $('#scrape-title').addEventListener('keydown', (event) => { if (event.key === 'Enter') runScrapeSearch(id); });
  if (track.title) runScrapeSearch(id);
}

async function runScrapeSearch(id) {
  const title = $('#scrape-title').value.trim();
  const artist = $('#scrape-artist').value.trim();
  const album = $('#scrape-album').value.trim();
  if (!title) { toast('请输入歌曲标题', 'warn'); return; }
  const results = $('#scrape-results');
  results.innerHTML = `<div class="modal-loading"><span class="spinner"></span><span>正在搜索多个音乐源…</span></div>`;
  try {
    const params = new URLSearchParams({ title, artist, album });
    if (state.scrapeSources.size) params.set('sources', [...state.scrapeSources].join(','));
    const data = await api(`/api/scrape/search?${params}`);
    if (!document.querySelector('#scrape-results')) return;
    state.scrapeCandidates = data.candidates || [];
    renderScrapeCandidates(id);
  } catch (error) {
    if (results) results.innerHTML = `<div class="empty-inline">${escapeHtml(error.message)}</div>`;
  }
}

function renderScrapeCandidates(trackId) {
  const container = $('#scrape-results');
  if (!state.scrapeCandidates.length) {
    container.innerHTML = `<div class="empty-inline">没有找到候选结果。可以只填标题，或换一个音乐源。</div>`;
    return;
  }
  container.innerHTML = state.scrapeCandidates.map((candidate, index) => {
    const cover = candidate.cover_url
      ? `<img src="${escapeHtml(candidate.cover_url)}" alt="" loading="lazy" onerror="this.remove()">`
      : icon('disc');
    const tags = [
      candidate.album ? `<span class="tag-mini">${escapeHtml(candidate.album)}</span>` : '',
      candidate.year ? `<span class="tag-mini">${candidate.year}</span>` : '',
      candidate.track ? `<span class="tag-mini">Track ${candidate.track}${candidate.track_total ? `/${candidate.track_total}` : ''}</span>` : '',
      candidate.duration_ms ? `<span class="tag-mini">${fmtDuration(candidate.duration_ms)}</span>` : '',
    ].join('');
    return `
      <div class="candidate">
        <div class="candidate-cover">${cover}</div>
        <div class="candidate-info">
          <div class="candidate-title">${escapeHtml(candidate.title || '未命名')}</div>
          <div class="candidate-sub">${escapeHtml(candidate.artist || (candidate.artists || []).join(' / ') || '未知歌手')} · ${escapeHtml(candidate.album || '未知专辑')}</div>
          <div class="candidate-tags"><span class="source-badge ${escapeHtml(candidate.source)}">${escapeHtml(candidate.source)}</span>${tags}</div>
        </div>
        <div class="score-pill">
          <span class="score ${candidate.match_score < 0.62 ? 'low' : ''}">${pct(candidate.match_score)}</span>
          <button class="btn btn-sm btn-soft" data-apply-candidate="${index}">应用</button>
        </div>
      </div>`;
  }).join('');
  $$('[data-apply-candidate]', container).forEach((button) => {
    button.addEventListener('click', () => applyCandidate(trackId, Number(button.dataset.applyCandidate), button));
  });
}

async function applyCandidate(trackId, index, button) {
  const candidate = state.scrapeCandidates[index];
  if (!candidate) return;
  const overwrite = $('#scrape-overwrite')?.checked ?? true;
  const noLyrics = $('#scrape-no-lyrics')?.checked ?? true;
  button.disabled = true;
  button.innerHTML = `<span class="spinner"></span>`;
  try {
    await withLoading('写入元数据与封面…', () => api('/api/scrape/apply', {
      method: 'POST',
      body: { track_id: trackId, candidate, overwrite, cover: true },
    }));
    let lyricsMessage = '';
    if (!noLyrics) {
      try {
        const lyrics = await withLoading('刮削歌词…', () => api('/api/lyrics/batch', {
          method: 'POST',
          body: {
            ids: [trackId],
            bilingual: state.settings?.config?.lyrics?.bilingual ?? true,
            embed: state.settings?.config?.lyrics?.embed ?? true,
            overwrite: state.settings?.config?.lyrics?.overwrite ?? true,
          },
        }));
        lyricsMessage = lyrics.updated
          ? '，并写入歌词'
          : '，但没有匹配到歌词';
      } catch (error) {
        lyricsMessage = `；歌词未写入：${error.message}`;
      }
    }
    toast(`已应用来自 ${providerLabel(candidate.source)} 的元数据${lyricsMessage}`);
    closeModal();
    await Promise.all([loadTracks(), loadStats()]);
  } catch (error) {
    toast(error.message, 'error', 6000);
    button.disabled = false;
    button.textContent = '应用';
  }
}

function openAutoScrapeDialog(ids) {
  if (!ids.length) return;
  const total = Math.min(ids.length, 50);
  const threshold = Math.round((state.settings?.config?.auto_match_threshold ?? 0.62) * 100);
  const body = `
    <div class="batch-summary">
      <div class="summary-number">${fmtNumber(total)}<small> 首</small></div>
      <p class="hint">按标题、艺术家、专辑和时长自动匹配，匹配度低于 <b>${threshold}%</b> 的歌曲会跳过。</p>
      <label class="check-label" style="margin-top:14px">
        <input type="checkbox" id="auto-no-lyrics" checked>
        只改元数据（不生成歌词文件）
      </label>
      <p class="hint" style="margin-top:8px">取消勾选会额外为匹配成功的歌曲刮削歌词，耗时更长。</p>
    </div>`;
  const footer = `<button class="btn btn-ghost" data-close-modal>取消</button><button class="btn btn-primary" id="auto-start">${icon('wand')}开始智能刮削</button>`;
  openModal({ title: '智能刮削', subtitle: '开始后可以随时停止', body, footer });
  $('#auto-start').addEventListener('click', () => {
    const noLyrics = $('#auto-no-lyrics')?.checked ?? true;
    closeModal();
    runAutoScrapeJob(ids.slice(0, 50), noLyrics);
  });
}

async function runAutoScrapeJob(ids, noLyrics) {
  if (!ids.length) return;
  let cancelled = false;
  let done = 0;
  let updated = 0;
  let lyricsUpdated = 0;
  const failures = [];
  const successful = [];

  const body = `
    <div class="auto-job">
      <div class="auto-job-top"><span id="auto-current">准备中…</span><b id="auto-count">0 / ${ids.length}</b></div>
      <div class="progress"><i id="auto-bar"></i></div>
      <p class="hint" id="auto-hint">正在逐首匹配，可随时点击“停止”。当前歌曲会先完成。</p>
    </div>`;
  openModal({
    title: '智能刮削中',
    subtitle: '全部智能刮削任务',
    body,
    footer: `<button class="btn btn-danger" id="auto-stop">${icon('close')}停止</button>`,
    locked: true,
  });

  const update = () => {
    const percent = Math.round((done / ids.length) * 100);
    $('#auto-count').textContent = `${done} / ${ids.length}`;
    $('#auto-bar').style.width = `${percent}%`;
    if (done > 0) $('#auto-current').textContent = `已处理 ${done} 首，成功 ${updated} 首`;
  };

  $('#auto-stop').addEventListener('click', () => {
    cancelled = true;
    const button = $('#auto-stop');
    if (button) {
      button.disabled = true;
      button.textContent = '正在停止…';
    }
    const hint = $('#auto-hint');
    if (hint) hint.textContent = '将在当前歌曲处理完成后停止。';
  });

  for (const id of ids) {
    if (cancelled) break;
    const track = state.trackMap.get(id);
    const label = track ? (track.title || track.filename) : `#${id}`;
    const current = $('#auto-current');
    if (current) current.textContent = `正在匹配：${label}`;
    try {
      const result = await api('/api/scrape/auto', {
        method: 'POST',
        body: {
          ids: [id],
          threshold: state.settings?.config?.auto_match_threshold ?? 0.62,
          overwrite: true,
          cover: true,
        },
      });
      const item = result.results?.[0];
      if (item?.ok) {
        updated += 1;
        successful.push(id);
      } else {
        failures.push(item || { id, message: '未匹配' });
      }
    } catch (error) {
      failures.push({ id, title: label, message: error.message });
    }
    done += 1;
    update();
  }

  if (!cancelled && !noLyrics && successful.length) {
    const hint = $('#auto-hint');
    if (hint) hint.textContent = '歌词匹配中，仍可点击停止。';
    for (let index = 0; index < successful.length; index += 1) {
      if (cancelled) break;
      const current = $('#auto-current');
      if (current) {
        current.textContent = `正在刮削歌词：${index + 1} / ${successful.length}`;
      }
      try {
        const lyrics = await api('/api/lyrics/batch', {
          method: 'POST',
          body: {
            ids: [successful[index]],
            bilingual: state.settings?.config?.lyrics?.bilingual ?? true,
            embed: state.settings?.config?.lyrics?.embed ?? true,
            overwrite: state.settings?.config?.lyrics?.overwrite ?? true,
          },
        });
        lyricsUpdated += lyrics.updated || 0;
      } catch (error) {
        toast(`歌词刮削失败：${error.message}`, 'warn', 5000);
      }
    }
  }

  closeModal();
  const title = cancelled ? '智能刮削已停止' : (noLyrics ? '智能刮削完成' : '智能刮削 + 歌词完成');
  showBatchResult(
    `${title}${lyricsUpdated ? `，歌词写入 ${lyricsUpdated} 首` : ''}`,
    updated,
    ids.length,
    failures,
    '应用',
  );
  if (cancelled) toast(`已停止，完成 ${updated} 首`, 'warn');
  await Promise.all([loadTracks(), loadStats()]);
}

function showBatchResult(title, updated, total, failures, unit = '处理') {
  const body = `
    <div class="batch-summary">
      <div class="summary-number">${fmtNumber(updated)}<small> / ${fmtNumber(total)}</small></div>
      <p class="hint">${escapeHtml(title)}：成功${unit} ${fmtNumber(updated)} 首${failures.length ? `，${failures.length} 首未处理` : '。'}</p>
      ${failures.length ? `<div class="fail-list">${failures.slice(0, 30).map((item) => `
        <div class="fail-item"><span>${escapeHtml(item.title || `#${item.id}`)}</span><small>${escapeHtml(item.message || '未匹配')}</small></div>`).join('')}</div>` : ''}
    </div>`;
  openModal({ title, subtitle: '批量任务结果', body, footer: `<button class="btn btn-primary" data-close-modal>知道了</button>` });
}

/* ------------------------------------------------------------------ */
/* Lyrics                                                              */
/* ------------------------------------------------------------------ */
async function openLyricsForTrack(id) {
  let track = state.trackMap.get(id);
  if (!track) {
    try { track = await api(`/api/tracks/${id}`); } catch (error) { toast(error.message, 'error'); return; }
  }
  state.currentTrack = track;
  state.lyricsCandidates = [];
  state.activeLyric = 0;
  const body = `
    <div class="lyrics-layout">
      <div class="lyrics-preview-pane">
        <div class="pane-head"><span>歌词预览</span><span id="lyrics-status">${track.has_lyrics ? '已有歌词' : '尚无歌词'}</span></div>
        <div class="lyrics-content" id="lyrics-preview"><div class="empty-inline">加载中…</div></div>
      </div>
      <div class="lyrics-results-pane">
        <div class="pane-head"><span>搜索结果</span><button class="btn btn-sm btn-ghost" id="search-lyrics">${icon('search')}重新搜索</button></div>
        <div id="lyrics-results" style="overflow:auto;max-height:500px"><div class="modal-loading"><span class="spinner"></span><span>搜索同步歌词与翻译…</span></div></div>
      </div>
    </div>
    <div class="lyrics-options">
      <label class="check-label"><input type="checkbox" id="lyrics-bilingual-opt" ${state.settings?.config?.lyrics?.bilingual ? 'checked' : ''}>双语合并</label>
      <label class="check-label"><input type="checkbox" id="lyrics-embed-opt" ${state.settings?.config?.lyrics?.embed ? 'checked' : ''}>嵌入音频标签</label>
      <label class="check-label"><input type="checkbox" id="lyrics-file-opt" checked>新建 .lrc 文件</label>
      <span style="color:var(--muted-2);font-size:10.5px">不新建文件时仅写入标签；翻译来自网易云音乐。</span>
    </div>`;
  const footer = `<button class="btn btn-ghost" data-close-modal>关闭</button><button class="btn btn-primary" id="apply-lyric" disabled>${icon('lyrics')}应用歌词</button>`;
  openModal({ title: '歌词刮削', subtitle: `${track.title || track.filename} · ${track.artist || '未知歌手'}`, body, footer, wide: true });
  $('#search-lyrics').addEventListener('click', () => searchLyricsForTrack(id));
  $('#apply-lyric').addEventListener('click', () => applySelectedLyrics(id));
  $('#lyrics-file-opt').addEventListener('change', (event) => {
    if (!event.target.checked) $('#lyrics-embed-opt').checked = true;
  });
  $('#lyrics-embed-opt').addEventListener('change', (event) => {
    if (!event.target.checked && !$('#lyrics-file-opt').checked) {
      $('#lyrics-file-opt').checked = true;
      toast('不新建文件时必须嵌入标签，已恢复 .lrc 文件选项', 'warn');
    }
  });
  const current = await api(`/api/tracks/${id}/lyrics`).catch(() => ({ lyrics: '' }));
  if (!$('#lyrics-preview')) return;
  $('#lyrics-preview').textContent = current.lyrics || '当前没有歌词。';
  $('#lyrics-status').textContent = current.lyrics ? '当前歌词' : '尚无歌词';
  searchLyricsForTrack(id);
}

async function searchLyricsForTrack(id) {
  const results = $('#lyrics-results');
  results.innerHTML = `<div class="modal-loading"><span class="spinner"></span><span>搜索同步歌词与翻译…</span></div>`;
  try {
    const data = await api('/api/lyrics/search', { method: 'POST', body: { track_id: id } });
    if (!document.querySelector('#lyrics-results')) return;
    state.lyricsCandidates = data.candidates || [];
    state.activeLyric = 0;
    renderLyricCandidates(id);
  } catch (error) {
    if (results) results.innerHTML = `<div class="empty-inline">${escapeHtml(error.message)}</div>`;
  }
}

function renderLyricCandidates(trackId) {
  const container = $('#lyrics-results');
  if (!state.lyricsCandidates.length) {
    container.innerHTML = `<div class="empty-inline">没有找到歌词。确认标题与歌手后重试。</div>`;
    $('#apply-lyric').disabled = true;
    return;
  }
  container.innerHTML = state.lyricsCandidates.map((candidate, index) => `
    <div class="lyric-candidate ${index === state.activeLyric ? 'active' : ''}" data-lyric="${index}">
      <div class="lyric-candidate-top">
        <span class="source-badge ${escapeHtml(candidate.source)}">${escapeHtml(candidate.source)}</span>
        <span class="lyric-candidate-title">${escapeHtml(candidate.title || '未命名')}</span>
        <span class="score ${candidate.match_score < 0.5 ? 'low' : ''}">${pct(candidate.match_score)}</span>
      </div>
      <div class="lyric-candidate-preview">${escapeHtml(candidate.preview || '（无预览）')}</div>
      <div class="lyric-candidate-badges">
        ${candidate.has_synced ? statusLabel('同步', 'ok') : statusLabel('纯文本')}
        ${candidate.has_translation || (candidate.translated_lyrics || '').trim() ? statusLabel('翻译', 'trans') : ''}
        ${candidate.instrumental ? statusLabel('纯音乐') : ''}
        ${candidate.artist ? statusLabel(candidate.artist) : ''}
      </div>
    </div>`).join('');
  $$('[data-lyric]', container).forEach((node) => {
    node.addEventListener('click', () => {
      state.activeLyric = Number(node.dataset.lyric);
      $$('[data-lyric]', container).forEach((item) => item.classList.toggle('active', item === node));
      renderLyricPreview();
    });
  });
  $('#apply-lyric').disabled = false;
  renderLyricPreview();
}

function renderLyricPreview() {
  const candidate = state.lyricsCandidates[state.activeLyric];
  if (!candidate) return;
  const bilingual = $('#lyrics-bilingual-opt')?.checked;
  const original = candidate.synced_lyrics || candidate.plain_lyrics || '';
  const translated = candidate.translated_lyrics || '';
  let preview = original;
  if (bilingual && translated) {
    const lines = original.split('\n');
    const trans = translated.split('\n');
    preview = lines.map((line, index) => `${line}${trans[index] ? `\n${trans[index]}` : ''}`).join('\n');
  }
  $('#lyrics-preview').textContent = preview || '（空）';
  $('#lyrics-status').textContent = `来自 ${providerLabel(candidate.source)} · ${candidate.has_synced ? '同步歌词' : '纯文本'}`;
}

async function applySelectedLyrics(trackId) {
  const candidate = state.lyricsCandidates[state.activeLyric];
  if (!candidate) return;
  const bilingual = $('#lyrics-bilingual-opt')?.checked ?? true;
  let embed = $('#lyrics-embed-opt')?.checked ?? true;
  let writeFile = $('#lyrics-file-opt')?.checked ?? true;
  if (!writeFile && !embed) {
    embed = true;
    $('#lyrics-embed-opt').checked = true;
  }
  try {
    const result = await withLoading('写入歌词…', () => api('/api/lyrics/apply', {
      method: 'POST',
      body: {
        track_id: trackId,
        source: candidate.source,
        source_id: candidate.source_id,
        bilingual,
        embed,
        write_file: writeFile,
        overwrite: true,
      },
    }));
    $('#lyrics-preview').textContent = result.lyrics || '';
    const target = result.wrote_file
      ? (result.embedded ? '歌词与 .lrc 已写入' : '.lrc 歌词已写入')
      : '歌词已写入音频标签（未新建 .lrc）';
    $('#lyrics-status').textContent = target;
    toast(result.has_translation ? `双语${target}` : target);
    await Promise.all([loadTracks(), loadStats()]);
  } catch (error) {
    toast(error.message, 'error', 6000);
  }
}

function openBatchLyricsDialog(ids) {
  if (!ids.length) return;
  const total = Math.min(ids.length, 50);
  const body = `
    <div class="batch-summary">
      <div class="summary-number">${fmtNumber(total)}<small> 首</small></div>
      <p class="hint">为所选歌曲自动匹配歌词，匹配度过低会跳过。</p>
      <label class="check-label" style="margin-top:14px">
        <input type="checkbox" id="batch-lyrics-file" checked>
        新建 .lrc 歌词文件
      </label>
      <label class="check-label" style="margin-top:10px">
        <input type="checkbox" id="batch-lyrics-embed" ${state.settings?.config?.lyrics?.embed ? 'checked' : ''}>
        嵌入音频标签
      </label>
      <p class="hint" style="margin-top:8px">不新建文件时必须嵌入标签；两者都勾选会同时写入文件和标签。</p>
    </div>`;
  const footer = `<button class="btn btn-ghost" data-close-modal>取消</button><button class="btn btn-primary" id="batch-lyrics-start">${icon('lyrics')}开始刮削</button>`;
  openModal({ title: '批量歌词', subtitle: '选择歌词写入方式', body, footer });
  $('#batch-lyrics-file').addEventListener('change', (event) => {
    if (!event.target.checked) $('#batch-lyrics-embed').checked = true;
  });
  $('#batch-lyrics-embed').addEventListener('change', (event) => {
    if (!event.target.checked && !$('#batch-lyrics-file').checked) {
      $('#batch-lyrics-file').checked = true;
      toast('不新建文件时必须嵌入标签，已恢复 .lrc 文件选项', 'warn');
    }
  });
  $('#batch-lyrics-start').addEventListener('click', () => {
    let writeFile = $('#batch-lyrics-file').checked;
    let embed = $('#batch-lyrics-embed').checked;
    if (!writeFile && !embed) {
      embed = true;
      $('#batch-lyrics-embed').checked = true;
    }
    closeModal();
    batchLyrics(ids, { writeFile, embed });
  });
}

async function batchLyrics(ids, options = {}) {
  if (!ids.length) return;
  const writeFile = options.writeFile ?? true;
  let embed = options.embed ?? (state.settings?.config?.lyrics?.embed ?? true);
  if (!writeFile && !embed) embed = true;
  showLoading(`正在为 ${Math.min(ids.length, 50)} 首歌曲匹配歌词…`);
  try {
    const result = await api('/api/lyrics/batch', {
      method: 'POST',
      body: {
        ids: ids.slice(0, 50),
        bilingual: state.settings?.config?.lyrics?.bilingual ?? true,
        embed,
        write_file: writeFile,
        overwrite: state.settings?.config?.lyrics?.overwrite ?? true,
      },
    });
    hideLoading();
    const failed = (result.results || []).filter((item) => !item.ok);
    showBatchResult(
      writeFile ? '批量歌词' : '批量歌词（仅标签）',
      result.updated,
      result.total,
      failed,
      '写入',
    );
    await Promise.all([loadTracks(), loadStats()]);
  } catch (error) {
    hideLoading();
    toast(error.message, 'error', 6000);
  }
}

/* ------------------------------------------------------------------ */
/* Delimiter normalization                                             */
/* ------------------------------------------------------------------ */
function openDelimiterModal(ids) {
  if (!ids.length) return;
  const config = state.settings?.config || {};
  const first = state.trackMap.get(ids[0]);
  const sample = first ? [first.artist, first.genre].filter(Boolean).join('  ·  ') : '';
  const body = `
    <div class="delimiter-fields">
      ${[
        ['artist', '艺术家（显示）'],
        ['artists', '艺术家列表'],
        ['albumartist', '专辑艺术家'],
        ['genres', '流派'],
        ['composers', '作曲'],
        ['lyricists', '作词'],
      ].map(([value, label]) => `<label class="delimiter-field"><input type="checkbox" value="${value}" checked>${label}</label>`).join('')}
    </div>
    <div class="form-grid">
      <label class="field"><span>识别这些分隔符</span><input id="norm-input" class="mono" value="${escapeHtml((config.split_separators || []).join(' '))}" placeholder="; ； 、 | &amp; ,"></label>
      <label class="field"><span>统一写入为</span><input id="norm-output" class="mono" value="" placeholder="留空使用各字段默认分隔符"></label>
    </div>
    <p class="hint" style="margin-top:12px">预览：<span class="mono" id="norm-preview">${escapeHtml(sample || '—')}</span></p>
    <p class="hint">留空时使用设置中「艺术家列表 / 流派 / 作曲 / 作词」各自的默认分隔符。会直接重写所选歌曲的标签，建议先用一首歌验证。</p>`;
  const footer = `<button class="btn btn-ghost" data-close-modal>取消</button><button class="btn btn-primary" id="apply-normalize">${icon('delimiter')}批量改写 ${ids.length} 首</button>`;
  openModal({ title: '批量修改分隔符', subtitle: '把多值字段拆开，再用各字段默认分隔符写回', body, footer });
  $('#apply-normalize').addEventListener('click', async () => {
    const fields = $$('.delimiter-field input:checked').map((input) => input.value);
    if (!fields.length) { toast('请至少选择一个字段', 'warn'); return; }
    const inputSeparators = $('#norm-input').value.split(/\s+/).filter(Boolean);
    const outputSeparator = $('#norm-output').value;
    try {
      const result = await withLoading('重写标签…', () => api('/api/tracks/normalize', {
        method: 'POST',
        body: { ids, fields, input_separators: inputSeparators, output_separator: outputSeparator },
      }));
      closeModal();
      toast(`已更新 ${result.updated} 首歌曲${result.errors?.length ? `，${result.errors.length} 个错误` : ''}`);
      await Promise.all([loadTracks(), loadStats()]);
    } catch (error) {
      toast(error.message, 'error', 6000);
    }
  });
}

/* ------------------------------------------------------------------ */
/* Rename                                                              */
/* ------------------------------------------------------------------ */
async function loadNaming() {
  try {
    state.naming = await api('/api/naming/variables');
    renderNamingPickers();
  } catch (_) { /* non-fatal */ }
}

function renderNamingPickers() {
  if (!state.naming) return;
  const presets = state.naming.presets || [];
  $('#rename-presets').innerHTML = presets.map((preset) => `
    <button class="preset-chip" data-preset="${escapeHtml(preset.template)}">${escapeHtml(preset.name)}</button>`).join('');
  $$('#rename-presets [data-preset]').forEach((button) => {
    button.addEventListener('click', () => {
      $('#rename-template').value = button.dataset.preset;
      scheduleRenamePreview();
    });
  });
  const vars = state.naming.variables || [];
  $('#rename-vars').innerHTML = vars.map((variable) => `
    <button class="var-chip" data-var="${escapeHtml(variable.name)}" title="${escapeHtml(variable.description)}">{${escapeHtml(variable.name)}}</button>`).join('');
  $$('#rename-vars [data-var]').forEach((button) => {
    button.addEventListener('click', () => insertVariable(button.dataset.var));
  });
}

function insertVariable(name) {
  const input = $('#rename-template');
  const start = input.selectionStart ?? input.value.length;
  const end = input.selectionEnd ?? input.value.length;
  input.value = `${input.value.slice(0, start)}{${name}}${input.value.slice(end)}`;
  input.focus();
  input.selectionStart = input.selectionEnd = start + name.length + 2;
  scheduleRenamePreview();
}

function selectedIds() {
  return [...state.selected];
}

function renderRenamePage() {
  $('#rename-selected-count').textContent = `${state.selected.size} 首`;
  if (state.settings?.config?.naming_template && !$('#rename-template').value) {
    $('#rename-template').value = state.settings.config.naming_template;
  }
  if (!state.selected.size) {
    $('#rename-preview-list').innerHTML = `<div class="empty-inline">在音乐库勾选歌曲后，回到这里预览。<br><br><button class="btn btn-soft" id="goto-library">去选择歌曲</button></div>`;
    $('#goto-library')?.addEventListener('click', () => setView('library'));
    $('#rename-execute').disabled = true;
    state.renameItems = [];
    return;
  }
  scheduleRenamePreview();
}

function scheduleRenamePreview() {
  clearTimeout(state.renameTimer);
  state.renameTimer = setTimeout(previewRename, 380);
}

async function previewRename() {
  const ids = selectedIds();
  const template = $('#rename-template').value.trim();
  if (!ids.length || !template) return;
  try {
    const artistSeparator = $('#rename-artist-separator').value.trim();
    const data = await api('/api/rename/preview', {
      method: 'POST',
      body: { ids, template, artist_separator: artistSeparator },
    });
    state.renameItems = data.items || [];
    state.renameOk = !!data.ok;
    renderRenamePreview(data);
  } catch (error) {
    $('#rename-preview-list').innerHTML = `<div class="empty-inline">${escapeHtml(error.message)}</div>`;
    $('#rename-execute').disabled = true;
  }
}

function renderRenamePreview(data) {
  const container = $('#rename-preview-list');
  if (!state.renameItems.length) {
    container.innerHTML = `<div class="empty-inline">没有可预览的歌曲。</div>`;
    $('#rename-execute').disabled = true;
    return;
  }
  container.innerHTML = state.renameItems.map((item) => `
    <div class="rename-item ${item.valid ? '' : 'invalid'}">
      ${icon(item.valid ? 'check' : 'alert')}
      <div class="rename-paths">
        <span class="rename-old">${escapeHtml(item.old_path || '—')}</span>
        <span class="rename-new">${escapeHtml(item.new_path || '—')}</span>
        ${item.error ? `<span class="rename-err">${escapeHtml(item.error)}</span>` : ''}
      </div>
    </div>`).join('');
  $('#rename-execute').disabled = !data.ok;
}

async function executeRename() {
  const ids = selectedIds();
  const template = $('#rename-template').value.trim();
  if (!ids.length || !template) return;
  try {
    const artistSeparator = $('#rename-artist-separator').value.trim();
    const result = await withLoading('移动文件…', () => api('/api/rename', {
      method: 'POST',
      body: { ids, template, artist_separator: artistSeparator },
    }));
    if (!result.ok) {
      state.renameItems = result.items || state.renameItems;
      renderRenamePreview(result);
      toast(result.message || '预检失败，没有移动文件', 'error', 5000);
      return;
    }
    toast(`已重命名 ${result.moved} 首歌曲`);
    state.selected.clear();
    await Promise.all([loadTracks(), loadStats()]);
    renderRenamePage();
  } catch (error) {
    toast(error.message, 'error', 6000);
  }
}

/* ------------------------------------------------------------------ */
/* Modal                                                               */
/* ------------------------------------------------------------------ */
function openModal({ title, subtitle = '', body, footer = '', wide = false, locked = false }) {
  const root = $('#modal-root');
  state.modalLocked = locked;
  root.innerHTML = `
    <div class="modal-backdrop" data-backdrop>
      <div class="modal ${wide ? 'wide' : ''}" role="dialog" aria-modal="true" aria-label="${escapeHtml(title)}">
        <div class="modal-head">
          <div><h2>${escapeHtml(title)}</h2>${subtitle ? `<p>${escapeHtml(subtitle)}</p>` : ''}</div>
          ${locked ? '' : `<button class="icon-btn modal-close" data-close-modal aria-label="关闭">${icon('close')}</button>`}
        </div>
        <div class="modal-body">${body}</div>
        ${footer ? `<div class="modal-foot">${footer}</div>` : ''}
      </div>
    </div>`;
  $$('[data-close-modal]', root).forEach((button) => button.addEventListener('click', closeModal));
  const backdrop = $('[data-backdrop]', root);
  backdrop.addEventListener('mousedown', (event) => {
    if (!locked && event.target === backdrop) closeModal();
  });
  document.body.classList.add('modal-open');
}

function closeModal() {
  state.modalLocked = false;
  $('#modal-root').innerHTML = '';
  document.body.classList.remove('modal-open');
}

/* ------------------------------------------------------------------ */
/* Wiring                                                              */
/* ------------------------------------------------------------------ */
function wireEvents() {
  $('#login-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const error = $('#login-error');
    error.classList.add('hidden');
    try {
      await api('/api/auth/login', { method: 'POST', body: { password: $('#login-password').value } });
      showApp();
      bootData().catch((err) => toast(err.message, 'error', 6000));
    } catch (err) {
      error.textContent = err.message;
      error.classList.remove('hidden');
    }
  });

  $$('.nav-item').forEach((item) => item.addEventListener('click', () => setView(item.dataset.view)));
  $$('.mobile-nav-item').forEach((item) => {
    item.addEventListener('click', () => setView(item.dataset.mobileView));
  });
  $('#btn-scan').addEventListener('click', startScan);
  $('#side-scan').addEventListener('click', startScan);
  $('#scan-cancel').addEventListener('click', async () => {
    try {
      await api('/api/scan/cancel', { method: 'POST', body: {} });
      toast('正在取消扫描…', 'warn');
    } catch (error) {
      toast(error.message, 'error');
    }
  });
  $('#btn-refresh').addEventListener('click', () => Promise.all([loadTracks(), loadStats()]));
  $('#global-search').addEventListener('input', debouncedQuery);
  $('#filter-chips').addEventListener('click', (event) => {
    const chip = event.target.closest('[data-filter]');
    if (chip) setFilter(chip.dataset.filter);
  });
  $('#tracks-body').addEventListener('click', (event) => {
    const open = event.target.closest('[data-open]');
    if (open) { openEditor(Number(open.dataset.open)); return; }
  });
  $('#tracks-body').addEventListener('change', (event) => {
    const checkbox = event.target.closest('[data-select]');
    if (checkbox) toggleSelection(Number(checkbox.dataset.select), checkbox.checked);
  });
  $('#select-page').addEventListener('change', (event) => selectPage(event.target.checked));
  $('#select-page-head').addEventListener('change', (event) => selectPage(event.target.checked));
  $('#btn-edit-selected').addEventListener('click', () => {
    const ids = selectedIds();
    if (ids.length === 1) openEditor(ids[0]);
  });
  $('#btn-auto-scrape').addEventListener('click', () => openAutoScrapeDialog(selectedIds()));
  $('#btn-lyrics-selected').addEventListener('click', () => {
    const ids = selectedIds();
    if (ids.length === 1) openLyricsForTrack(ids[0]);
    else if (ids.length > 1) openBatchLyricsDialog(ids);
  });
  $('#btn-rename-selected').addEventListener('click', () => setView('rename'));
  $('#btn-delimiter').addEventListener('click', () => openDelimiterModal(selectedIds()));
  $('#btn-logout').addEventListener('click', async () => {
    await api('/api/auth/logout', { method: 'POST', body: {} }).catch(() => {});
    showLogin();
  });
  $('#avatar').addEventListener('click', () => setView('settings'));
  $('#prev-page').addEventListener('click', () => {
    if (state.offset === 0) return;
    state.offset = Math.max(0, state.offset - state.limit);
    state.selected.clear();
    loadTracks();
  });
  $('#next-page').addEventListener('click', () => {
    if (state.offset + state.limit >= state.total) return;
    state.offset += state.limit;
    state.selected.clear();
    loadTracks();
  });
  $('#empty-action').addEventListener('click', () => {
    if ($('#empty-action').dataset.action === 'clear') {
      $('#global-search').value = '';
      setFilter('');
    } else {
      startScan();
    }
  });

  $('#rename-template').addEventListener('input', scheduleRenamePreview);
  $('#rename-preview').addEventListener('click', previewRename);
  $('#rename-execute').addEventListener('click', executeRename);

  $('#add-root').addEventListener('click', () => {
    const input = $('#new-root');
    const value = input.value.trim();
    if (!value) return;
    const roots = state.settings.config.library_roots;
    if (!roots.includes(value)) roots.push(value);
    input.value = '';
    renderRoots(roots);
    markSettingsDirty();
  });
  $('#new-root').addEventListener('keydown', (event) => {
    if (event.key === 'Enter') { event.preventDefault(); $('#add-root').click(); }
  });
  $('#save-settings').addEventListener('click', saveSettings);
  $('#reset-endpoints')?.addEventListener('click', () => {
    ENDPOINT_FIELDS.forEach(([id]) => {
      const input = $(`#${id}`);
      if (input) input.value = input.dataset.default || '';
    });
    markSettingsDirty();
    toast('已恢复默认 API 地址，保存后生效');
  });
  $('#match-threshold').addEventListener('input', (event) => {
    $('#threshold-value').textContent = `${event.target.value}%`;
  });
  $('#view-settings').addEventListener('input', markSettingsDirty);
  $('#view-settings').addEventListener('change', markSettingsDirty);

  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && !state.modalLocked) closeModal();
    if (event.key === '/' && !['INPUT', 'TEXTAREA', 'SELECT'].includes(document.activeElement?.tagName)) {
      if (state.view === 'library') {
        event.preventDefault();
        $('#global-search').focus();
      }
    }
  });
}

document.addEventListener('DOMContentLoaded', () => {
  wireEvents();
  checkAuth();
  if ('serviceWorker' in navigator) {
    navigator.serviceWorker.register('/sw.js').catch(() => {
      // Service workers require HTTPS (or localhost); LAN HTTP is fine without one.
    });
  }
});
