/*
 * Veil iOS 界面交互原型。
 * 所属模块：移动端 UI 示意层，演示空间切换、容器创建 / 解锁、文件筛选、列表布局和预览。
 * 核心数据：files 保存静态示例文件；state 保存当前页面、空间、容器、筛选条件、布局、预览项和搜索词。
 * 核心函数：home / form / browser / preview 生成各页面；render 写入页面；委托事件更新演示状态或打开提示框。
 * 依赖能力：浏览器 DOM、对话框、FormData 和共享 SVG 图标。
 * 业务目标：提供 iOS 视觉与交互流程预览；数据只存在内存，不连接容器服务、不读取设备文件或保存密码。
 */
(() => {
  'use strict';
  /** 根视图容器；每次 render 都用当前 state 生成整页内容。 */
  const app = document.querySelector('#app');
  /** 用于显示确认、导入和操作提示的原生对话框元素。 */
  const dialog = document.querySelector('#dialog');
  /**
   * 函数功能：按名称生成 SVG 图标；未知名称回退为通用文件图标。
   * @param {string} name 共享图标表中的键。
   * @returns {string} SVG 标记，未知键回退 file。
   * 异常：{TypeError} window.VeilIcons 未加载。
   * 注意事项：图形片段视为可信资源，不做清洗。
   */
  const icon = name => `<svg class="icon" viewBox="0 0 24 24" aria-hidden="true">${window.VeilIcons[name] || window.VeilIcons.file}</svg>`;
  /**
   * 函数功能：转义动态文本后嵌入 HTML，避免示例名称被浏览器当作标记解析。
   * @param {*} value 待放入 HTML 的可字符串化值。
   * @returns {string} 替换五种 HTML 特殊字符后的文本。
   * 异常：{Error} 对象自定义字符串转换失败时向上传播。
   * 注意事项：仅适用于 HTML 文本或带引号属性，不适用于 URL、CSS 或脚本。
   */
  const escape = value => String(value).replace(/[&<>"']/g, c => ({ '&':'&amp;', '<':'&lt;', '>':'&gt;', '"':'&quot;', "'":'&#39;' })[c]);
  /** 静态示例文件集合；字段分别表示文件名、分类、示例图片或文本及所在目录说明。 */
  const files = [
    { name:'山间清晨.jpg', category:'照片', img:'../assets/mountain.svg', detail:'旅行照片' },
    { name:'海岸线.png', category:'照片', img:'../assets/coast.svg', detail:'旅行照片' },
    { name:'旅行片段.mp4', category:'视频', img:'../assets/coast.svg', detail:'旅行照片' },
    { name:'项目说明.txt', category:'文档', text:'Veil · 个人文件\n\n把重要的文件，放在安心的地方。\n\n你的文件，你的空间。', detail:'根目录' },
    { name:'备份归档.zip', category:'压缩包', detail:'根目录' }
  ];
  /** 当前演示状态：页面、所选空间 / 容器、浏览模式、分类筛选、布局、预览文件和搜索文本。 */
  // spaces/containers 为名称数组，没有稳定业务 ID 或空间归属；preview 为 files 中对象引用，query 为按名称搜索词。
  // container 在用户选择后才动态加入状态，锁定样式按钮只切到 home，不清空预览、搜索、筛选或容器名。
  const state = { page:'home', space:'默认空间', spaces:['默认空间','工作空间'], containers:['个人文件','旅行记录'], mode:'按类型', filter:'全部', layout:'grid', preview:null, query:'' };
  /**
   * 函数功能：生成共用按钮模板。
   * @param {string} label 可信 HTML 按钮文字。
   * @param {string} action 委托事件动作键。
   * @param {string} [cls=""] 可信样式类文本。
   * @returns {string} 按钮 HTML。
   * 异常：无显式异常；对象隐式转换等底层错误向上传播。
   * 注意事项：不执行点击动作；参数不转义，不可直接使用未转义用户输入。
   */
  const button = (label, action, cls = '') => `<button class="button ${cls}" data-action="${action}">${label}</button>`;
  /**
   * 函数功能：生成移动端顶部导航。
   * @param {string} left 可信左按钮 HTML；精确等于“锁定”映射 home，否则 back。
   * @param {string} title 导航标题，会转义。
   * @param {string} [right=""] 可信右侧 HTML。
   * @returns {string} 导航栏 HTML。
   * 异常：{Error} 标题字符串转换失败时向上传播。
   * 注意事项：左右 HTML 不转义；按钮没有真实会话锁定能力。
   */
  const nav = (left, title, right = '') => `<div class="nav"><button data-action="${left === '锁定' ? 'home' : 'back'}">${left}</button><strong>${escape(title)}</strong><span>${right}</span></div>`;
  /**
   * 函数功能：生成空间首页和容器卡片；集合为所有演示空间共用，不按当前空间筛选。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 空间、容器及操作入口的首页 HTML。
   * 异常：{TypeError} 内存状态或图标资源不完整。
   * 注意事项：spaces 每项是空间名，containers 每项是容器名，两者无归属关联，各空间显示同一组容器。
   */
  function home() {
    return `<div class="page"><div class="home-head"><img class="brand" src="../assets/brand.png" alt=""><strong>veil</strong><span class="spacer"></span><details class="space-menu"><summary>${escape(state.space)} ﹀</summary><div class="menu"><span>切换空间</span>${state.spaces.map((space,i) => `<button data-action="space" data-id="${i}">${space === state.space ? '✓ ' : ''}${escape(space)}</button>`).join('')}<button data-action="new-space">＋ 新建空间</button><hr><span>其他操作</span><button data-action="import-container">导入加密备份</button><button class="danger" data-action="clear">清理数据</button></div></details></div><div class="section-head"><h2>${escape(state.space)}的容器</h2>${button('＋ 创建','create','primary')}${button('⇩ 导入','import-container')}</div>${state.containers.map(name => `<button class="card" data-action="unlock" data-name="${escape(name)}"><span class="symbol">${icon('shield')}</span><span><strong>${escape(name)}</strong><small>本机 / ${escape(state.space)}</small></span><span class="arrow">→</span></button>`).join('') || '<div class="card">这个空间还没有容器</div>'}<div class="footer"><span>VEIL / PRIVATE BY DESIGN</span><span>你的文件，你的空间</span></div></div>`;
  }
  /**
   * 函数功能：生成创建或解锁容器表单。
   * @param {boolean} create true 展示创建字段；false 展示解锁字段。
   * @returns {string} 对应模式的表单 HTML。
   * 异常：{TypeError} 所需状态或渲染依赖缺失时向上传播。
   * 注意事项：只生成字段；不校验密码，也不创建或解锁真实容器。
   */
  function form(create) {
    return `${nav('取消',create ? '创建容器' : '解锁容器')}<div class="page"><div class="form-header"><span class="symbol">${icon(create ? 'plus' : 'unlock')}</span><div class="eyebrow">${create ? 'NEW CONTAINER' : 'OPEN CONTAINER'}</div><h1>${create ? '创建新容器' : escape(state.container || '个人文件')}</h1><p>${create ? '设置密码，为重要文件创建一个私密空间。' : '输入密码，打开你的私密空间。'}</p></div><form class="form-card" id="form">${create ? '<label class="field">容器名称<input name="name" placeholder="例如：个人文件" required></label>' : `<div class="card">${icon('shield')} ${escape(state.container || '个人文件')}</div>`}<label class="field">容器密码<input name="password" type="password" placeholder="${create ? '设置容器密码' : '输入密码解锁'}" required></label>${create ? '<label class="field">确认密码<input name="confirm" type="password" placeholder="再次输入密码" required></label>' : ''}<button class="button primary wide">${create ? '创建容器' : '解锁容器'} →</button></form><p class="note">♙ 密码用于本地解锁容器，请妥善保管。</p></div>`;
  }
  /**
   * 函数功能：按示例文件数据生成缩略图。
   * @param {Object} file 示例文件，含 name、category、detail 以及可选 img、text 字段。
   * @returns {string} 优先图片、其次文本，否则 archive 图标的 HTML。
   * 异常：{TypeError} file 为空或图标依赖缺失。
   * 注意事项：视频只叠加播放符号，不生成实际视频缩略图。
   */
  function thumbnail(file) {
    if (file.img) return `<img src="${file.img}" alt="">${file.category === '视频' ? '<span class="play">▶</span>' : ''}`;
    if (file.text) return `<pre>${escape(file.text)}</pre>`;
    return icon('archive');
  }
  /**
   * 函数功能：生成文件浏览页，并按分类筛选和搜索条件计算可见文件集合。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 根据分类和文件名搜索得到的文件浏览页 HTML。
   * 异常：{TypeError} 文件或搜索字段类型错误。
   * 注意事项：shown 每项是同时匹配条件的静态文件，保持源顺序；按路径模式仍应用隐藏 filter，没有真实目录导航。
   */
  function browser() {
    // 本轮仅显示满足分类与搜索条件的文件；shown 同时用于结果网格和“几项”计数。
    const shown = files.filter(file => (state.filter === '全部' || state.filter === file.category) && file.name.toLowerCase().includes(state.query.toLowerCase()));
    return `${nav('锁定',state.container || '个人文件','•••')}<div class="page"><div class="tabs"><button class="${state.mode === '按路径' ? 'active' : ''}" data-action="mode" data-id="按路径">按路径</button><button class="${state.mode === '按类型' ? 'active' : ''}" data-action="mode" data-id="按类型">按类型</button><span class="spacer"></span><button data-action="layout">${state.layout === 'grid' ? '☰' : '▦'}</button><button data-action="add">＋</button></div><label class="field" style="margin:14px 0"><input type="search" id="search" placeholder="搜索文件" value="${escape(state.query)}"></label>${state.mode === '按类型' ? `<div class="filters">${['全部','照片','视频','文档','压缩包','其他'].map(item => `<button class="${state.filter === item ? 'active' : ''}" data-action="filter" data-id="${item}">${item}</button>`).join('')}</div>` : ''}<section class="file-area"><div class="file-caption"><span>${state.mode === '按类型' ? `文件分类 / ${escape(state.filter)}` : '全部文件'}</span><span>${shown.length} 项</span></div><div class="grid ${state.layout === 'list' ? 'list' : ''}">${shown.map(file => `<button class="tile" data-action="preview" data-name="${escape(file.name)}"><div class="thumb">${thumbnail(file)}</div><strong>${escape(file.name)}</strong><small>${escape(file.detail)}</small></button>`).join('') || '没有找到文件'}</div></section><p class="note">长按文件可导出或删除</p></div>`;
  }
  /**
   * 函数功能：生成当前示例文件的预览内容；不读取文件系统，也不执行真实解密。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} state.preview 对应的示例预览 HTML。
   * 异常：{TypeError} 尚未选择有效文件或依赖缺失。
   * 注意事项：优先视频示意、图片、文本，再兜底提示；视频只有图片和符号，不播放媒体。
   */
  function preview() {
    const file = state.preview;
    const content = file.category === '视频' ? `<div class="video"><img src="${file.img}" alt=""><span>▶</span></div>` : file.img ? `<img src="${file.img}" alt="图片示例">` : file.text ? `<pre>${escape(file.text)}</pre>` : '<p class="note">此类型暂不支持预览，可导出后打开。</p>';
    return `<div class="preview">${nav('关闭',file.name)}${content}</div>`;
  }
  /**
   * 函数功能：按 state.page 选择页面模板并整体更新根视图。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {undefined} 使用当前 page 替换整个 app 内容。
   * 异常：{TypeError} 根节点或所选页面必需状态缺失。
   * 注意事项：home/create/unlock/files 以外均进入 preview，没有未知页兜底；整体替换会丢失旧节点和焦点。
   */
  function render() { app.innerHTML = state.page === 'home' ? home() : state.page === 'create' ? form(true) : state.page === 'unlock' ? form(false) : state.page === 'files' ? browser() : preview(); }
  /**
   * 函数功能：设置并显示统一提示对话框。
   * @param {string} title 可信标题 HTML。
   * @param {string} body 可信说明 HTML。
   * @param {string} action 确认按钮动作键。
   * @param {string} label 可信确认按钮 HTML。
   * @returns {undefined} 替换共用 dialog 并调用 showModal。
   * 异常：{TypeError|DOMException} 对话框节点缺失或状态非法。
   * 注意事项：所有内容须来自可信模板；不清洗 HTML，也不执行确认动作。
   */
  function modal(title, body, action, label) { dialog.innerHTML = `<h2>${title}</h2><p>${body}</p><div class="actions">${button('取消','dismiss')}${button(label,action,'primary')}</div>`; dialog.showModal(); }
  // 通过 data-action 委托处理所有按钮，避免页面重新生成后逐个重新绑定。
  document.addEventListener('click', event => {
    // 忽略没有业务动作的点击；其余分支按动作键更新对应演示状态。
    const target = event.target.closest('[data-action]'); if (!target) return;
    const action = target.dataset.action;
    // 关闭动作提前结束，避免继续执行后面的状态更新逻辑。
    if (action === 'dismiss') { dialog.close(); return; }
    if (action === 'space') state.space = state.spaces[Number(target.dataset.id)];
    else if (action === 'create' || action === 'home') state.page = action;
    else if (action === 'back') state.page = state.page === 'preview' ? 'files' : 'home';
    else if (action === 'unlock') { state.container = target.dataset.name; state.page = 'unlock'; }
    else if (action === 'mode') state.mode = target.dataset.id;
    else if (action === 'filter') state.filter = target.dataset.id;
    else if (action === 'layout') state.layout = state.layout === 'grid' ? 'list' : 'grid';
    else if (action === 'preview') { state.preview = files.find(file => file.name === target.dataset.name); state.page = 'preview'; }
    // 添加、导入、新建空间等流程只显示原型提示，不访问设备文件或真正保存数据。
    else if (action === 'add') { dialog.innerHTML = `<h2>添加内容</h2><p>选择内容来源。此设计稿不读取设备内容。</p><div class="actions">${button('取消','dismiss')}${button('从文件添加','dismiss')}${button('从相册添加','dismiss','primary')}</div>`; dialog.showModal(); return; }
    else if (action === 'clear') { modal('清理所有本机数据？','这会永久删除本机的空间、容器与文件。此处仅为设计演示。','clear-demo','清理示例'); return; }
    // 只清空容器名称数组，静态 files、空间列表和其他状态仍保留，不是真实本机数据删除。
    else if (action === 'clear-demo') { state.containers = []; dialog.close(); }
    else if (action === 'new-space') { modal('新建空间','空间保存在设备本机。此处仅展示界面流程。','dismiss','知道了'); return; }
    else if (action === 'import-container') { modal('导入容器','选择加密备份后输入密码导入。此设计稿不读取设备内容。','dismiss','知道了'); return; }
    render();
  });
  // 提交回调入参 SubmitEvent，返回 undefined；仅处理 id=form，创建时核对两次密码，其余依赖浏览器 required。
  // 解锁不检查任何已存密码，非空输入即可进入；密码不加入状态数组，但 FormData/DOM 仍短暂持有，未安全擦除。
  document.addEventListener('submit', event => {
    if (event.target.id !== 'form') return;
    event.preventDefault(); const data = new FormData(event.target);
    // 创建容器时先确认两次密码一致；校验失败仅提示，不切换页面。
    if (state.page === 'create' && data.get('password') !== data.get('confirm')) { modal('密码不一致','请检查两次输入的密码。','dismiss','知道了'); return; }
    state.container = state.page === 'create' ? data.get('name') : state.container;
    if (!state.containers.includes(state.container)) state.containers.push(state.container);
    state.page = 'files'; render();
  });
  // 更新搜索时整页会重绘，因此记录光标位置并在绘制后恢复输入焦点。
  document.addEventListener('input', event => { if (event.target.id === 'search') { state.query = event.target.value; const start = event.target.selectionStart; render(); const input = document.querySelector('#search'); input.focus(); try { input.setSelectionRange(start, start); } catch (_) {} } });
  render();
})();
