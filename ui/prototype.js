/* 此文件仅驱动设计稿，所有数据和交互均为内存中的示例。 */
(() => {
  'use strict';
  const $ = (selector) => document.querySelector(selector);
  const escape = (value) => String(value).replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]);
  const icon = (name, className = '') => `<svg class="icon ${className}" viewBox="0 0 24 24" aria-hidden="true">${window.VeilIcons[name] || window.VeilIcons.file}</svg>`;
  const button = (title, action, name = '', variant = 'secondary', attributes = '') => `<button type="button" class="button ${variant}" data-action="${action}" ${attributes}>${name ? icon(name) : ''}${title}</button>`;
  const categoryNames = { photo: '照片', video: '视频', document: '文档', archive: '压缩包', other: '其他' };
  const categoryIcons = { photo: 'image', video: 'video', document: 'file', archive: 'archive', other: 'file' };
  const seed = {
    spaces: [{ id: 'personal', name: '默认空间' }, { id: 'work', name: '工作空间' }],
    containers: [
      { id: 'private', space: 'personal', name: '个人文件', description: '本机 / 默认空间' },
      { id: 'travel', space: 'personal', name: '旅行记录', description: '本机 / 默认空间' },
      { id: 'archive', space: 'work', name: '工作档案', description: '本机 / 工作空间' }
    ],
    files: [
      { id: 'f1', name: '旅行照片/山间清晨.jpg', size: 1456128, category: 'photo', image: 'assets/mountain.svg', version: 1 },
      { id: 'f2', name: '旅行照片/海岸线.png', size: 1873920, category: 'photo', image: 'assets/coast.svg', version: 2 },
      { id: 'f3', name: '项目说明.txt', size: 2460, category: 'document', text: 'Veil · 个人文件\n\n把重要的文件，放在安心的地方。\n\n这里是设计稿中的示例内容。\n实际应用会在本机完成加密与解密，\n设计稿不会访问任何真实文件。\n\n— 你的文件，你的空间', version: 1 },
      { id: 'f4', name: '证件资料.pdf', size: 576512, category: 'document', version: 1 },
      { id: 'f5', name: '旅行照片/旅行片段.mp4', size: 12582912, category: 'video', version: 1 },
      { id: 'f6', name: '备份归档.zip', size: 4235264, category: 'archive', version: 3 },
      { id: 'f7', name: '工作资料/方案草稿.pdf', size: 167936, category: 'document', version: 2 },
      { id: 'f8', name: '工作资料/会议记录.txt', size: 3728, category: 'document', text: '会议记录（示例）\n\n1. 确认页面布局\n2. 对齐字体、颜色与间距\n3. 检查空内容、加载和错误状态', version: 1 }
    ],
    directories: ['旅行照片', '工作资料', '待整理']
  };
  let data = structuredClone(seed);
  let state = { page: 'home', space: 'personal', container: 'private', mode: 'type', layout: 'grid', selected: [], directory: '', search: '', status: 'normal', collapsed: new Set() };
  let toastTimer;
  let nextID = 1;
  const basename = (name) => name.split('/').pop();
  const parent = (name) => name.includes('/') ? name.slice(0, name.lastIndexOf('/')) : '根目录';
  const currentContainer = () => data.containers.find((item) => item.id === state.container) || data.containers[0];
  const currentSpace = () => data.spaces.find((space) => space.id === state.space) || data.spaces[0];
  const size = (bytes) => {
    if (bytes < 1024) return `${bytes} B`;
    let value = bytes / 1024, index = 0;
    const units = ['KB', 'MB', 'GB', 'TB'];
    while (value >= 1024 && index < units.length - 1) { value /= 1024; index++; }
    return `${value.toFixed(1)} ${units[index]}`;
  };
  const toast = (message) => {
    clearTimeout(toastTimer);
    $('#toast').textContent = message;
    $('#toast').hidden = false;
    toastTimer = setTimeout(() => { $('#toast').hidden = true; }, 3000);
  };
  function navigate(page) {
    state.page = page;
    if (location.hash !== `#${page}`) history.replaceState(null, '', `#${page}`);
    if ($('#modal').open) $('#modal').close();
    render();
  }
  function renderSidebar() {
    return `<aside class="app-sidebar" aria-label="空间导航">
      <a class="app-brand" href="#home"><img src="assets/brand.png" alt="Veil"><strong>veil</strong></a>
      <div class="sidebar-caption">空间</div><div class="spaces-list">${data.spaces.map((space) => `<div class="space-group">
        <div class="space-row ${state.page !== 'files' && space.id === state.space ? 'active' : ''}">
          <button class="disclosure" data-action="collapse" data-id="${space.id}" aria-label="${state.collapsed.has(space.id) ? '展开' : '收起'}${escape(space.name)}" aria-expanded="${!state.collapsed.has(space.id)}">${icon('chevron')}</button>
          <button class="space-name" data-action="space" data-id="${space.id}">${icon('folder')}<span>${escape(space.name)}</span></button>
          <button class="space-add" data-action="create-in-space" data-id="${space.id}" aria-label="在${escape(space.name)}新建容器">${icon('plus')}</button>
        </div>${state.collapsed.has(space.id) ? '' : `<div class="space-children">${data.containers.filter((item) => item.space === space.id).map((item) => `<button class="sidebar-container ${state.page === 'files' && item.id === state.container ? 'active' : ''}" data-action="open" data-id="${item.id}">${icon('shield')}<span>${escape(item.name)}</span></button>`).join('')}</div>`}
      </div>`).join('')}</div>
      <button class="new-space" data-action="new-space">${icon('plus')}新建空间</button>
      <div class="sidebar-local">${icon('lock')}本机</div>
    </aside>`;
  }
  function renderTopbar() {
    const titles = { home: '我的容器', files: currentContainer()?.name || '个人文件', create: '新建容器', unlock: '打开容器', foundations: '视觉规范' };
    const actions = state.page === 'files' ? `<div class="top-actions"><button class="toolbar-button" data-action="backup">${icon('download')}备份</button><button class="toolbar-button" data-action="refresh">${icon('refresh')}刷新</button><button class="toolbar-button" data-action="lock">${icon('lock')}锁定</button></div>` : '<span class="local-status"><span class="status-dot"></span>本地模式</span>';
    return `<header class="topbar"><div class="topbar-crumb"><span>${escape(currentSpace().name)}</span><span class="slash">/</span><b>${escape(titles[state.page])}</b></div>${actions}</header>`;
  }
  function containerCard(item) {
    return `<button class="recent-card" data-action="open" data-id="${item.id}"><span class="recent-icon">${icon('shield')}</span><span class="recent-info"><strong>${escape(item.name)}</strong><small>${escape(item.description)}</small></span>${icon('arrow', 'recent-arrow')}</button>`;
  }
  function home() {
    const cards = state.status === 'empty' ? [] : data.containers.filter((item) => item.space === state.space);
    return `<div class="page-content">
      ${state.status === 'error' ? `<div class="error-box" role="alert">${icon('alert')}空间列表暂时无法读取。<button class="text-button" data-action="retry">重试</button></div>` : ''}
      <div class="page-heading"><div><div class="eyebrow">YOUR PRIVATE SPACE</div><h1>我的容器</h1><p>${escape(currentSpace().name)} · 为重要文件留一个安心的地方。</p></div>${button('新建容器', 'create', 'plus', 'primary')}</div>
      <section class="welcome-card"><div class="welcome-copy"><span class="pill"><span class="status-dot"></span>一切都在你的掌控中</span><h2>安全，从这里开始。</h2><p>创建一个加密容器，或打开已有容器。<br>你的文件加密保存在本机，由你掌管。</p><div class="button-row">${button('创建容器', 'create', 'plus', 'primary')}${button('打开容器', 'open-first', 'folder')}</div></div><img class="hero-art" src="assets/vault.svg" alt="" aria-hidden="true"></section>
      <section class="recent-section"><div class="section-heading"><div><h3>${escape(currentSpace().name)}的容器</h3><p>选择容器并输入密码打开</p></div><button class="text-button" data-action="open-first">打开其他容器 ${icon('arrow')}</button></div>
        ${state.status === 'loading' ? '<div class="recent-grid" aria-busy="true" aria-label="正在加载容器"><div class="skeleton" style="height:74px"></div><div class="skeleton" style="height:74px"></div></div>' : cards.length ? `<div class="recent-grid">${cards.map(containerCard).join('')}</div>` : `<div class="recent-empty"><span class="recent-icon">${icon('folder')}</span><div><strong>这个空间还没有容器</strong><p>创建容器后，它会显示在这里。</p></div></div>`}
      </section>
    </div>`;
  }
  function entries() {
    if (state.status === 'empty') return [];
    let result;
    if (state.mode === 'type') {
      result = data.files.filter((file) => !state.selected.length || state.selected.includes(file.category)).map((file) => ({ kind: 'file', file, name: basename(file.name), path: file.name }));
    } else {
      const prefix = state.directory ? `${state.directory}/` : '';
      const folders = new Set();
      for (const path of data.directories) {
        if (path.startsWith(prefix) && path !== state.directory) {
          const child = path.slice(prefix.length).split('/')[0];
          if (child) folders.add(prefix + child);
        }
      }
      const files = [];
      for (const file of data.files) {
        if (!file.name.startsWith(prefix)) continue;
        const tail = file.name.slice(prefix.length);
        if (tail.includes('/')) folders.add(prefix + tail.split('/')[0]);
        else files.push({ kind: 'file', file, name: tail, path: file.name });
      }
      result = [...folders].sort((a, b) => a.localeCompare(b, 'zh-CN')).map((path) => ({ kind: 'folder', path, name: basename(path) })).concat(files);
    }
    return result.filter((entry) => entry.path.toLocaleLowerCase().includes(state.search.toLocaleLowerCase()));
  }
  function results() {
    if (state.status === 'loading') return `<div class="file-grid" aria-busy="true" aria-label="正在加载文件">${Array.from({ length: 8 }, () => '<div><div class="skeleton" style="height:105px"></div><div class="skeleton skeleton-line"></div></div>').join('')}</div>`;
    if (state.status === 'error') return `<div class="files-empty"><span class="empty-symbol">${icon('alert')}</span><strong>文件列表加载失败</strong><p>容器内容未发生变化，请稍后重试。</p>${button('重新加载', 'retry', 'refresh')}</div>`;
    const rows = entries();
    if (!rows.length) return `<div class="files-empty"><span class="empty-symbol">${icon('folder')}</span><strong>${state.status === 'empty' ? '这里还是空的' : '这里没有文件'}</strong><p>${state.status === 'empty' ? '添加文件后会显示在这里。' : '试试选择其他分类、目录或搜索词。'}</p>${button('添加文件', 'add', 'plus')}</div>`;
    const rowAction = (row) => `data-action="${row.kind === 'folder' ? 'directory' : 'preview'}" data-id="${escape(row.kind === 'folder' ? row.path : row.file.id)}"`;
    if (state.layout === 'grid') return `<div class="file-grid">${rows.map((row) => `<button class="file-tile" ${rowAction(row)}><span class="tile-preview">${row.kind === 'file' && row.file.image ? `<img src="${row.file.image}" alt="">` : icon(row.kind === 'folder' ? 'folder' : categoryIcons[row.file.category])}</span><span class="tile-name" title="${escape(row.path)}">${escape(row.name)}</span><small>${row.kind === 'folder' ? '文件夹' : escape(state.mode === 'type' ? parent(row.file.name) : size(row.file.size))}</small></button>`).join('')}</div>`;
    return `<div class="file-list"><div class="list-head"><span>名称</span><span>大小</span><span class="file-version">版本</span><span class="file-date">加密时间</span><span>操作</span></div>${rows.map((row) => `<div class="file-row"><button class="file-name" ${rowAction(row)}>${icon(row.kind === 'folder' ? 'folder' : categoryIcons[row.file.category])}<span>${escape(row.name)}${row.kind === 'file' && state.mode === 'type' ? `<small>${escape(parent(row.file.name))}</small>` : ''}</span></button><span>${row.kind === 'file' ? size(row.file.size) : '—'}</span><span class="file-version">${row.kind === 'file' ? `v${row.file.version}` : '—'}</span><span class="file-date">${row.kind === 'file' ? '2026年10月29日' : '—'}</span><span class="row-actions">${row.kind === 'file' ? `<button class="icon-action" data-action="export" aria-label="导出 ${escape(row.name)}">${icon('download')}</button>` : ''}<button class="icon-action" data-action="delete" data-id="${escape(row.path)}" aria-label="删除 ${escape(row.name)}">${icon('trash')}</button></span></div>`).join('')}</div>`;
  }
  function breadcrumbs() {
    if (state.mode === 'type') return `<span>文件分类</span><span class="slash">/</span><span>${state.selected.length ? state.selected.map((id) => categoryNames[id]).join('、') : '全部'}</span>`;
    let path = '';
    return `<button data-action="directory" data-id="">全部文件</button>${state.directory.split('/').filter(Boolean).map((part) => { path = path ? `${path}/${part}` : part; return `${icon('chevron')}<button data-action="directory" data-id="${escape(path)}">${escape(part)}</button>`; }).join('')}`;
  }
  function files() {
    return `<div class="page-content browser-content">
      <div class="browse-tabs" role="tablist" aria-label="浏览方式"><button role="tab" data-action="mode" data-id="path" aria-selected="${state.mode === 'path'}">按路径</button><button role="tab" data-action="mode" data-id="type" aria-selected="${state.mode === 'type'}">按类型</button><label class="search-box">${icon('search')}<input id="search" type="search" placeholder="搜索文件" aria-label="搜索文件" value="${escape(state.search)}"></label></div>
      ${state.mode === 'type' ? `<div class="type-filters" aria-label="文件类型，可多选"><button data-action="filter" data-id="all" aria-pressed="${!state.selected.length}">全部</button>${Object.entries(categoryNames).map(([id, label]) => `<button data-action="filter" data-id="${id}" aria-pressed="${state.selected.includes(id)}">${label}</button>`).join('')}</div>` : ''}
      <section class="browser-area" aria-label="文件浏览"><div class="browser-toolbar"><div class="breadcrumbs">${breadcrumbs()}</div><div class="browser-actions"><div class="layout-switch" aria-label="展示方式"><button data-action="layout" data-id="list" aria-pressed="${state.layout === 'list'}">${icon('list')}列表</button><button data-action="layout" data-id="grid" aria-pressed="${state.layout === 'grid'}">${icon('grid')}大图标</button></div><button class="toolbar-button" data-action="add">${icon('plus')}添加</button></div></div><div id="browser-results">${results()}</div></section>
    </div>`;
  }
  function form(create) {
    const loading = state.status === 'loading';
    return `<div class="page-content"><div class="form-wrap"><button class="back-button" data-action="home">${icon('left')}返回容器</button><div class="form-heading"><span class="form-icon">${icon(create ? 'plus' : 'unlock')}</span><div class="eyebrow">${create ? 'NEW CONTAINER' : 'OPEN CONTAINER'}</div><h1>${create ? '创建新容器' : '解锁容器'}</h1><p>${create ? `在${escape(currentSpace().name)}中创建容器。` : '输入密码，打开你的私密空间。'}</p></div><form class="form-card" id="container-form" data-kind="${create ? 'create' : 'unlock'}"><div class="error-box" id="form-error" role="alert" ${state.status === 'error' ? '' : 'hidden'}>${icon('alert')}<span>${create ? '两次输入的密码不一致' : '密码不正确，请重试'}</span></div>
      ${create ? `<label class="field">容器名称<input name="name" placeholder="例如：个人文件" autocomplete="off" maxlength="100" ${loading ? 'disabled' : ''} required></label>` : `<div class="selected-container">${icon('shield')}<span><strong>${escape(currentContainer()?.name || '个人文件')}</strong><small>本机 / ${escape(currentSpace().name)}</small></span></div>`}
      <label class="field">容器密码<input name="password" type="password" placeholder="${create ? '设置容器密码' : '输入密码解锁'}" autocomplete="${create ? 'new-password' : 'current-password'}" ${loading ? 'disabled' : ''} required>${create ? '' : '<small class="field-hint">设计稿演示密码：demo。请勿输入真实密码。</small>'}</label>
      ${create ? `<label class="field">确认密码<input name="confirmation" type="password" placeholder="再次输入密码" autocomplete="new-password" ${loading ? 'disabled' : ''} required></label>` : ''}
      <button type="submit" class="button primary submit-button" ${loading ? 'disabled' : ''}>${loading ? '正在处理…' : create ? '创建容器' : '解锁容器'}${icon('arrow')}</button><p class="demo-caption">仅演示界面流程，不创建真实容器，不保存密码。</p></form><div class="privacy-note">${icon('lock')}密码用于本地解锁容器，请妥善保管。</div></div></div>`;
  }
  function foundations() {
    const tokens = window.VeilDesignTokens;
    const theme = document.documentElement.dataset.theme;
    const swatches = ['canvas', 'sidebar', 'surface', 'text', 'secondary', 'muted', 'border', 'selected', 'primary', 'danger'];
    return `<div class="page-content foundations"><div class="page-heading"><div><div class="eyebrow">DESIGN FOUNDATIONS</div><h1>让细节保持一致。</h1><p>页面共享设计参数，交互遵循各平台的使用习惯。</p></div></div>
      <section class="foundation-section"><h2>颜色</h2><div class="swatch-grid">${swatches.map((key) => `<div class="swatch"><span class="swatch-color" style="background:var(--color-${key})"></span><strong>${key}</strong><code>${tokens.color[theme][key]}</code></div>`).join('')}</div><p class="foundation-note">当前为${theme === 'dark' ? '深色' : '浅色'}外观。点击右上角切换，查看对应颜色。</p></section>
      <section class="foundation-section"><h2>文字层级</h2>${[['我的容器', 30, 700, '页面标题'], ['安全，从这里开始。', 25, 600, '引导标题'], ['默认空间的容器', 15, 600, '章节标题'], ['为重要文件留一个安心的地方。', 13, 400, '说明文字'], ['文件已添加', 12, 400, '正文'], ['本机 / 默认空间', 11, 400, '辅助信息']].map(([text, fontSize, weight, label]) => `<div class="type-specimen"><span style="font-size:${fontSize}px;font-weight:${weight}">${text}</span><code>${label} · ${fontSize}px / ${weight}</code></div>`).join('')}</section>
      <section class="foundation-section"><h2>按钮与状态</h2><div class="component-board">${button('创建容器', 'component', 'plus', 'primary')}${button('打开容器', 'component', 'folder')}${button('确认删除', 'component', 'trash', 'danger')}${button('暂不可用', 'component', '', 'primary', 'disabled')}</div><p class="foundation-note">按钮高度 ${tokens.layout['button-height']}，圆角 ${tokens.radius.button}；支持悬停、按下、键盘焦点和禁用状态。</p></section>
      <section class="foundation-section"><h2>间距</h2><div class="spacing-board">${[4, 8, 12, 16, 20, 24, 32, 40, 48].map((value) => `<div class="spacing-item"><i style="height:${value}px"></i><span>${value}px</span></div>`).join('')}</div></section>
      <section class="foundation-section"><h2>图标与插画</h2><div class="component-board">${Object.keys(window.VeilIcons).map((name) => `<span title="${name}" style="padding:8px;color:var(--color-secondary)">${icon(name)}</span>`).join('')}</div><p class="foundation-note"><a href="assets/icons.svg" target="_blank" rel="noopener">SVG 图标集 ↗</a>　<a href="assets/vault.svg" target="_blank" rel="noopener">保险箱插画 ↗</a></p></section>
    </div>`;
  }
  function render() {
    const screens = { home, files, create: () => form(true), unlock: () => form(false), foundations };
    if (!screens[state.page]) state.page = 'home';
    $('#app').innerHTML = `${renderSidebar()}<div class="app-main">${renderTopbar()}${screens[state.page]()}<footer class="app-footer"><span>VEIL <span class="slash">/</span> PRIVATE BY DESIGN</span><span>你的文件，你的空间</span></footer></div>`;
    document.querySelectorAll('[data-page]').forEach((link) => {
      if (link.dataset.page === state.page) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current');
    });
    $('#page-state').disabled = state.page === 'foundations';
    $('#page-state').value = state.status;
    $('[data-action="theme"]').innerHTML = icon(document.documentElement.dataset.theme === 'dark' ? 'sun' : 'moon');
    $('[data-action="theme"]').setAttribute('aria-label', document.documentElement.dataset.theme === 'dark' ? '切换浅色外观' : '切换深色外观');
    resize();
  }
  function resize() {
    const width = Number($('#viewport').value);
    const stage = $('.stage-scroll');
    const inset = parseFloat(getComputedStyle(stage).paddingLeft) * 2;
    const actual = Math.min(width, stage.clientWidth - inset);
    $('#design-frame').style.width = `${width}px`;
    $('#design-frame').dataset.size = actual <= 650 ? '390' : actual < 1000 ? '820' : '1160';
  }
  function showDialog(content, preview = false) {
    const modal = $('#modal');
    modal.classList.toggle('preview-modal', preview);
    modal.innerHTML = content;
    if (!modal.open) modal.showModal();
  }
  function showPreview(id) {
    const file = data.files.find((item) => item.id === id);
    if (!file) return;
    let content;
    if (file.image) content = `<img src="${file.image}" alt="${escape(basename(file.name))}">`;
    else if (file.text) content = `<pre>${escape(file.text)}</pre>`;
    else if (file.name.endsWith('.pdf')) content = `<div class="document-preview">${icon('file')}<strong>${escape(basename(file.name))}</strong>${Array.from({ length: 7 }, (_, i) => `<span class="document-line" style="width:${i % 3 === 0 ? '70' : '100'}%"></span>`).join('')}<p class="demo-caption">PDF 预览布局示意</p></div>`;
    else content = `<div class="files-empty"><span class="empty-symbol">${icon(categoryIcons[file.category])}</span><strong>${file.category === 'video' ? '视频预览区域' : '此类型暂不支持内置预览'}</strong><p>${file.category === 'video' ? '设计稿未加载真实视频文件。' : '可以导出后使用系统应用打开。'}</p></div>`;
    showDialog(`<div class="preview-heading"><div><h2 id="modal-title">${escape(basename(file.name))}</h2><p>${escape(parent(file.name))} · ${size(file.size)} · v${file.version}</p></div><button class="icon-action" data-action="close-dialog" aria-label="关闭预览">${icon('close')}</button></div><div class="preview-content">${content}</div><div class="modal-actions">${button('导出', 'export', 'download')}${button('删除', 'delete', 'trash', 'secondary', `data-id="${escape(file.name)}"`)}</div>`, true);
  }
  document.addEventListener('click', (event) => {
    const target = event.target instanceof Element ? event.target.closest('[data-action]') : null;
    if (!target || target.disabled) return;
    const { action, id } = target.dataset;
    switch (action) {
      case 'home': navigate('home'); break;
      case 'create': navigate('create'); break;
      case 'create-in-space': state.space = id; navigate('create'); break;
      case 'open': {
        const item = data.containers.find((entry) => entry.id === id);
        if (!item) break;
        state.container = item.id; state.space = item.space; navigate('unlock'); break;
      }
      case 'open-first': {
        const item = data.containers.find((entry) => entry.space === state.space) || data.containers[0];
        if (item) { state.container = item.id; state.space = item.space; navigate('unlock'); }
        break;
      }
      case 'space': state.space = id; navigate('home'); break;
      case 'collapse': if (state.collapsed.has(id)) state.collapsed.delete(id); else state.collapsed.add(id); render(); break;
      case 'mode': state.mode = id; render(); break;
      case 'layout': state.layout = id; render(); break;
      case 'filter': state.selected = id === 'all' ? [] : state.selected.includes(id) ? state.selected.filter((value) => value !== id) : [...state.selected, id]; render(); break;
      case 'directory': state.directory = id; state.search = ''; render(); break;
      case 'preview': showPreview(id); break;
      case 'close-dialog': $('#modal').close(); break;
      case 'lock': state.directory = ''; state.selected = []; state.search = ''; navigate('home'); toast('已演示锁定容器'); break;
      case 'refresh': toast('文件列表已刷新（示例）'); break;
      case 'retry': state.status = 'normal'; render(); break;
      case 'export': case 'backup': toast(action === 'backup' ? '加密备份导出流程示意，不生成真实文件。' : '文件导出流程示意，不生成真实文件。'); break;
      case 'component': toast('按钮交互示例'); break;
      case 'new-space': showDialog(`<h2 id="modal-title">新建空间</h2><p>用不同空间整理你的加密容器。</p><form id="space-form"><label class="field">空间名称<input name="name" placeholder="例如：工作文件" maxlength="80" autocomplete="off" required></label><section class="space-location" aria-label="空间存储位置"><div class="location-heading"><span>存储位置</span><button type="button" class="text-button" data-action="reset-space-directory" hidden>恢复默认位置</button></div><div class="location-card">${icon('folder')}<span class="location-copy"><strong id="space-directory-name">默认位置</strong><small id="space-directory-path">由 Veil 自动管理</small></span>${button('选择目录…', 'select-space-directory')}</div><input type="hidden" name="path" value=""><p class="field-hint">此目录用于存放该空间内新建的容器。</p><div class="directory-examples" id="space-directory-options" aria-label="示例目录" hidden><p>选择示例目录</p>${button('文稿 / Veil', 'use-space-directory', 'folder', 'secondary', 'data-path="/Users/you/Documents/Veil"')}${button('外置磁盘 / Veil', 'use-space-directory', 'folder', 'secondary', 'data-path="/Volumes/Personal/Veil"')}</div></section><div id="space-error" class="error-box" role="alert" hidden></div><p class="demo-caption">设计稿使用示例目录，不访问本机文件。</p><div class="modal-actions">${button('取消', 'close-dialog')}<button type="submit" class="button primary">创建空间</button></div></form>`); break;
      case 'select-space-directory': $('#space-directory-options').hidden = !$('#space-directory-options').hidden; break;
      case 'use-space-directory': {
        const path = target.dataset.path;
        $('#space-form input[name="path"]').value = path;
        $('#space-directory-name').textContent = basename(path);
        $('#space-directory-path').textContent = path;
        $('[data-action="reset-space-directory"]').hidden = false;
        $('#space-directory-options').hidden = true;
        $('#space-error').hidden = true;
        break;
      }
      case 'reset-space-directory':
        $('#space-form input[name="path"]').value = '';
        $('#space-directory-name').textContent = '默认位置';
        $('#space-directory-path').textContent = '由 Veil 自动管理';
        target.hidden = true; $('#space-directory-options').hidden = true; $('#space-error').hidden = true; break;
      case 'add': showDialog(`<h2 id="modal-title">添加到容器</h2><p>添加到${state.mode === 'path' && state.directory ? `「${escape(state.directory)}」` : '根目录'}。同名文件不会被覆盖。</p><div class="recent-empty">${icon('file')}<span>示例文件.txt <small>· 2.4 KB</small></span></div><p class="demo-caption">设计稿仅添加示例条目，不读取本机文件。</p><div class="modal-actions">${button('取消', 'close-dialog')}${button('添加示例文件', 'confirm-add', 'plus', 'primary')}</div>`); break;
      case 'confirm-add': {
        const name = `示例文件${nextID++}.txt`;
        const path = state.mode === 'path' && state.directory ? `${state.directory}/${name}` : name;
        data.files.push({ id: `added-${nextID}`, name: path, category: 'document', size: 2460, version: 1, text: '这是一个设计稿中的示例文件。' });
        state.status = 'normal'; $('#modal').close(); render(); toast('已添加示例文件'); break;
      }
      case 'delete': showDialog(`<h2 id="modal-title">删除确认</h2><p>确定从容器中删除「${escape(basename(id))}」${data.directories.includes(id) ? '及其全部文件' : ''}？</p><p class="demo-caption">仅移除临时示例条目，可通过「重置示例」恢复。</p><div class="modal-actions">${button('取消', 'close-dialog')}${button('确认删除', 'confirm-delete', 'trash', 'danger', `data-id="${escape(id)}"`)}</div>`); break;
      case 'confirm-delete': data.files = data.files.filter((file) => file.name !== id && !file.name.startsWith(`${id}/`)); data.directories = data.directories.filter((path) => path !== id && !path.startsWith(`${id}/`)); $('#modal').close(); render(); toast('已移除示例条目'); break;
      case 'theme': document.documentElement.dataset.theme = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'; render(); break;
      case 'reset': data = structuredClone(seed); state = { ...state, space: 'personal', container: 'private', mode: 'type', layout: 'grid', selected: [], directory: '', search: '', status: 'normal', collapsed: new Set() }; nextID = 1; navigate('home'); toast('示例数据已重置'); break;
    }
  });
  document.addEventListener('submit', (event) => {
    event.preventDefault();
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    const values = new FormData(form);
    if (form.id === 'space-form') {
      const name = String(values.get('name') || '').trim();
      if (!name || data.spaces.some((space) => space.name === name)) {
        $('#space-error').hidden = false; $('#space-error').textContent = name ? '空间名称已存在' : '请输入空间名称'; return;
      }
      const path = String(values.get('path') || '');
      if (path && data.spaces.some((space) => space.path === path)) {
        $('#space-error').hidden = false; $('#space-error').textContent = '此目录已用于其他空间，请选择其他目录'; return;
      }
      const id = `space-${nextID++}`;
      data.spaces.push({ id, name, path }); state.space = id; form.reset(); $('#modal').close(); navigate('home'); return;
    }
    if (form.id !== 'container-form') return;
    const create = form.dataset.kind === 'create';
    const name = String(values.get('name') || '').trim();
    const password = String(values.get('password') || '');
    let message = '';
    if (!password) message = '请输入容器密码';
    else if (create && !name) message = '请输入容器名称';
    else if (create && password !== String(values.get('confirmation') || '')) message = '两次输入的密码不一致';
    else if (!create && password !== 'demo') message = '演示密码为 demo，请勿使用真实密码';
    if (message) { $('#form-error').hidden = false; $('#form-error span').textContent = message; return; }
    if (create) {
      const id = `container-${nextID++}`;
      data.containers.push({ id, space: state.space, name, description: `本机 / ${currentSpace().name}` }); state.container = id;
    }
    form.reset(); state.status = 'normal'; state.directory = ''; state.selected = []; state.search = ''; navigate('files'); toast(create ? '已创建示例容器，以下为演示文件。' : '已解锁示例容器');
  });
  document.addEventListener('input', (event) => {
    if (event.target.id === 'search') { state.search = event.target.value; $('#browser-results').innerHTML = results(); }
  });
  $('#viewport').addEventListener('change', resize);
  $('#page-state').addEventListener('change', (event) => { state.status = event.target.value; render(); });
  window.addEventListener('resize', resize);
  window.addEventListener('hashchange', () => { state.page = location.hash.slice(1) || 'home'; render(); });
  state.page = location.hash.slice(1) || 'home';
  render();
})();
