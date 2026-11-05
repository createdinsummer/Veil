/* iOS 设计示意：只操作内存中的示例数据。 */
(() => {
  'use strict';
  const app = document.querySelector('#app');
  const dialog = document.querySelector('#dialog');
  const icon = name => `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">${window.VeilIcons[name] || window.VeilIcons.file}</svg>`;
  const escape = value => String(value).replace(/[&<>"']/g, c => ({ '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;' })[c]);
  const files = [
    { name:'山间清晨.jpg', category:'照片', img:'../assets/mountain.svg', detail:'旅行照片' },
    { name:'海岸线.png', category:'照片', img:'../assets/coast.svg', detail:'旅行照片' },
    { name:'旅行片段.mp4', category:'视频', img:'../assets/coast.svg', detail:'旅行照片' },
    { name:'项目说明.txt', category:'文档', text:'Veil · 个人文件\n\n把重要的文件，放在安心的地方。\n\n你的文件，你的空间。', detail:'根目录' },
    { name:'备份归档.zip', category:'压缩包', detail:'根目录' }
  ];
  const state = { page:'home', space:'默认空间', spaces:['默认空间','工作空间'], containers:['个人文件','旅行记录'], mode:'按类型', filter:'全部', layout:'grid', preview:null, query:'' };
  const button = (label, action, cls = '') => `<button class="button ${cls}" data-action="${action}">${label}</button>`;
  const nav = (left, title, right = '') => `<div class="nav"><button data-action="${left === '锁定' ? 'home' : 'back'}">${left}</button><strong>${escape(title)}</strong><span>${right}</span></div>`;
  function home() {
    return `<div class="page"><div class="home-head"><img class="brand" src="../assets/brand.png" alt=""><strong>veil</strong><span class="spacer"></span><details class="space-menu"><summary>${escape(state.space)} ﹀</summary><div class="menu"><span>切换空间</span>${state.spaces.map((space,i) => `<button data-action="space" data-id="${i}">${space === state.space ? '✓ ' : ''}${escape(space)}</button>`).join('')}<button data-action="new-space">＋ 新建空间</button><hr><span>其他操作</span><button data-action="import-container">导入加密备份</button><button class="danger" data-action="clear">清理数据</button></div></details></div><div class="section-head"><h2>${escape(state.space)}的容器</h2>${button('＋ 创建','create','primary')}${button('⇩ 导入','import-container')}</div>${state.containers.map(name => `<button class="card" data-action="unlock" data-name="${escape(name)}"><span class="symbol">${icon('shield')}</span><span><strong>${escape(name)}</strong><small>本机 / ${escape(state.space)}</small></span><span class="arrow">→</span></button>`).join('') || '<div class="card">这个空间还没有容器</div>'}<div class="footer"><span>VEIL / PRIVATE BY DESIGN</span><span>你的文件，你的空间</span></div></div>`;
  }
  function form(create) {
    return `${nav('取消',create ? '创建容器' : '解锁容器')}<div class="page"><div class="form-header"><span class="symbol">${icon(create ? 'plus' : 'unlock')}</span><div class="eyebrow">${create ? 'NEW CONTAINER' : 'OPEN CONTAINER'}</div><h1>${create ? '创建新容器' : escape(state.container || '个人文件')}</h1><p>${create ? '设置密码，为重要文件创建一个私密空间。' : '输入密码，打开你的私密空间。'}</p></div><form class="form-card" id="form">${create ? '<label class="field">容器名称<input name="name" placeholder="例如：个人文件" required></label>' : `<div class="card">${icon('shield')} ${escape(state.container || '个人文件')}</div>`}<label class="field">容器密码<input name="password" type="password" placeholder="${create ? '设置容器密码' : '输入密码解锁'}" required></label>${create ? '<label class="field">确认密码<input name="confirm" type="password" placeholder="再次输入密码" required></label>' : ''}<button class="button primary wide">${create ? '创建容器' : '解锁容器'} →</button></form><p class="note">♙ 密码用于本地解锁容器，请妥善保管。</p></div>`;
  }
  function thumbnail(file) {
    if (file.img) return `<img src="${file.img}" alt="">${file.category === '视频' ? '<span class="play">▶</span>' : ''}`;
    if (file.text) return `<pre>${escape(file.text)}</pre>`;
    return icon('archive');
  }
  function browser() {
    const shown = files.filter(file => (state.filter === '全部' || state.filter === file.category) && file.name.toLowerCase().includes(state.query.toLowerCase()));
    return `${nav('锁定',state.container || '个人文件','•••')}<div class="page"><div class="tabs"><button class="${state.mode === '按路径' ? 'active' : ''}" data-action="mode" data-id="按路径">按路径</button><button class="${state.mode === '按类型' ? 'active' : ''}" data-action="mode" data-id="按类型">按类型</button><span class="spacer"></span><button data-action="layout">${state.layout === 'grid' ? '☰' : '▦'}</button><button data-action="add">＋</button></div><label class="field" style="margin:14px 0"><input type="search" id="search" placeholder="搜索文件" value="${escape(state.query)}"></label>${state.mode === '按类型' ? `<div class="filters">${['全部','照片','视频','文档','压缩包','其他'].map(item => `<button class="${state.filter === item ? 'active' : ''}" data-action="filter" data-id="${item}">${item}</button>`).join('')}</div>` : ''}<section class="file-area"><div class="file-caption"><span>${state.mode === '按类型' ? `文件分类 / ${escape(state.filter)}` : '全部文件'}</span><span>${shown.length} 项</span></div><div class="grid ${state.layout === 'list' ? 'list' : ''}">${shown.map(file => `<button class="tile" data-action="preview" data-name="${escape(file.name)}"><div class="thumb">${thumbnail(file)}</div><strong>${escape(file.name)}</strong><small>${escape(file.detail)}</small></button>`).join('') || '没有找到文件'}</div></section><p class="note">长按文件可导出或删除</p></div>`;
  }
  function preview() {
    const file = state.preview;
    const content = file.category === '视频' ? `<div class="video"><img src="${file.img}" alt=""><span>▶</span></div>` : file.img ? `<img src="${file.img}" alt="图片示例">` : file.text ? `<pre>${escape(file.text)}</pre>` : '<p class="note">此类型暂不支持预览，可导出后打开。</p>';
    return `<div class="preview">${nav('关闭',file.name)}${content}</div>`;
  }
  function render() { app.innerHTML = state.page === 'home' ? home() : state.page === 'create' ? form(true) : state.page === 'unlock' ? form(false) : state.page === 'files' ? browser() : preview(); }
  function modal(title, body, action, label) { dialog.innerHTML = `<h2>${title}</h2><p>${body}</p><div class="actions">${button('取消','dismiss')}${button(label,action,'primary')}</div>`; dialog.showModal(); }
  document.addEventListener('click', event => {
    const target = event.target.closest('[data-action]'); if (!target) return;
    const action = target.dataset.action;
    if (action === 'dismiss') { dialog.close(); return; }
    if (action === 'space') state.space = state.spaces[Number(target.dataset.id)];
    else if (action === 'create' || action === 'home') state.page = action;
    else if (action === 'back') state.page = state.page === 'preview' ? 'files' : 'home';
    else if (action === 'unlock') { state.container = target.dataset.name; state.page = 'unlock'; }
    else if (action === 'mode') state.mode = target.dataset.id;
    else if (action === 'filter') state.filter = target.dataset.id;
    else if (action === 'layout') state.layout = state.layout === 'grid' ? 'list' : 'grid';
    else if (action === 'preview') { state.preview = files.find(file => file.name === target.dataset.name); state.page = 'preview'; }
    else if (action === 'add') { dialog.innerHTML = `<h2>添加内容</h2><p>选择内容来源。此设计稿不读取设备内容。</p><div class="actions">${button('取消','dismiss')}${button('从文件添加','dismiss')}${button('从相册添加','dismiss','primary')}</div>`; dialog.showModal(); return; }
    else if (action === 'clear') { modal('清理所有本机数据？','这会永久删除本机的空间、容器与文件。此处仅为设计演示。','clear-demo','清理示例'); return; }
    else if (action === 'clear-demo') { state.containers = []; dialog.close(); }
    else if (action === 'new-space') { modal('新建空间','空间保存在设备本机。此处仅展示界面流程。','dismiss','知道了'); return; }
    else if (action === 'import-container') { modal('导入容器','选择加密备份后输入密码导入。此设计稿不读取设备内容。','dismiss','知道了'); return; }
    render();
  });
  document.addEventListener('submit', event => {
    if (event.target.id !== 'form') return;
    event.preventDefault(); const data = new FormData(event.target);
    if (state.page === 'create' && data.get('password') !== data.get('confirm')) { modal('密码不一致','请检查两次输入的密码。','dismiss','知道了'); return; }
    state.container = state.page === 'create' ? data.get('name') : state.container;
    if (!state.containers.includes(state.container)) state.containers.push(state.container);
    state.page = 'files'; render();
  });
  document.addEventListener('input', event => { if (event.target.id === 'search') { state.query = event.target.value; const start = event.target.selectionStart; render(); const input = document.querySelector('#search'); input.focus(); try { input.setSelectionRange(start, start); } catch (_) {} } });
  render();
})();
