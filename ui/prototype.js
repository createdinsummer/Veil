/*
 * Veil Web 原型交互控制器。
 * 所属模块：ui 原型层，负责侧栏、容器首页、文件浏览、表单、弹窗和设计规范页的示例交互。
 * 核心数据：seed 定义示例空间 / 容器 / 文件；data 是当前可变的演示副本；state 保存页面、筛选、布局、
 * 目录、搜索词和加载状态；HTML 模板由各渲染函数根据这些数据生成。
 * 核心函数：entries 整理待显示条目；results / files 生成浏览界面；render 组合页面；事件委托处理创建、导入、
 * 删除、预览和筛选动作；导航、弹窗和布局函数提供共同的 UI 行为。
 * 依赖能力：浏览器 DOM、结构化克隆、FormData、设计 token 和 SVG 图标资源。
 * 业务目标：演示 Veil 的文件和容器界面流程，不接入服务、不访问真实用户文件，也不保存示例密码或数据。
 */
(() => {
  'use strict';
  /**
   * 函数功能：按 CSS 选择器查找首个页面元素。
   * @param {string} selector 合法 CSS 选择器。
   * @returns {Element|null} 首个匹配节点，无匹配返回 null。
   * 异常：{DOMException} CSS 选择器语法无效。
   * 注意事项：依赖当前 document；调用方负责处理 null。
   */
  const $ = (selector) => document.querySelector(selector);
  /**
   * 函数功能：将动态文本转义后放入 HTML 模板，避免文件名或输入内容被解释成标记。
   * @param {*} value 待放入 HTML 的可字符串化值。
   * @returns {string} 替换五种 HTML 特殊字符后的文本。
   * 异常：{Error} 对象自定义字符串转换失败时向上传播。
   * 注意事项：仅适用于 HTML 文本或带引号属性，不适用于 URL、CSS 或脚本。
   */
  const escape = (value) => String(value).replace(/[&<>"']/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]);
  /**
   * 函数功能：根据图标名生成 SVG；未知名称回退到通用文件图标。
   * @param {string} name VeilIcons 中的图标键。
   * @param {string} [className=""] 可信 CSS 类文本。
   * @returns {string} SVG 标记；未知图标名回退 file。
   * 异常：{TypeError} 全局 VeilIcons 未加载。
   * 注意事项：图形片段和类名不清洗，必须使用可信资源。
   */
  const icon = (name, className = '') => `<svg class="icon ${className}" viewBox="0 0 24 24" aria-hidden="true">${window.VeilIcons[name] || window.VeilIcons.file}</svg>`;
  /**
   * 函数功能：生成统一按钮模板。参数依次为文案、业务动作、可选图标、样式变体和额外属性；返回 HTML 字符串。
   * @param {string} title 可信 HTML 按钮文案。
   * @param {string} action 委托事件识别的动作键。
   * @param {string} [name=""] 可选图标名。
   * @param {string} [variant="secondary"] 按钮样式键。
   * @param {string} [attributes=""] 可信原始 HTML 属性片段。
   * @returns {string} 按钮 HTML。
   * 异常：{TypeError} 需要图标时，图标依赖缺失会向上传播。
   * 注意事项：不执行动作；参数不自动转义，用户输入须先 escape。
   */
  const button = (title, action, name = '', variant = 'secondary', attributes = '') => `<button type="button" class="button ${variant}" data-action="${action}" ${attributes}>${name ? icon(name) : ''}${title}</button>`;
  /** 文件分类键到中文筛选名称的映射；筛选按钮、面包屑和设计规范展示共用。 */
  const categoryNames = { photo: '照片', video: '视频', document: '文档', archive: '压缩包', other: '其他' };
  /** 文件分类键到图标资源键的映射；没有专属图标的类型使用通用文件图标。 */
  const categoryIcons = { photo: 'image', video: 'video', document: 'file', archive: 'archive', other: 'file' };
  /** 原型启动或重置时使用的只读初始示例：空间、容器、文件元数据与目录路径。 */
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
  /** 当前内存中的示例业务数据；用户的原型操作只改变此副本。 */
  // spaces 每项为 id/name；containers 每项为 id/space/name/description，space 关联空间 ID。
  // files 每项为 id/name（逻辑路径）/size（示例字节数）/category/version，可选 image 或 text 供预览；所有容器共用同一 files，不按容器隔离。
  // directories 是逻辑目录路径字符串数组，用于保留没有文件的目录；上述数据不来自服务或真实文件系统。
  let data = structuredClone(seed);
  /** 当前界面状态：页签、所选空间/容器、浏览方式、筛选、搜索、加载错误和侧栏折叠信息。 */
  // selected 为类别键数组，空数组表示全部；collapsed 为折叠空间 ID 的 Set，渲染时每个空间查询是否属于集合。
  // status 只模拟 normal/empty/loading/error 界面，不对应真实异步服务状态；directory/search 分别是逻辑目录与搜索文本。
  let state = { page: 'home', space: 'personal', container: 'private', mode: 'type', layout: 'grid', selected: [], directory: '', search: '', status: 'normal', sidebarVisible: true, collapsed: new Set() };
  /** 提示条自动关闭定时器；新提示显示时先取消旧计时。 */
  let toastTimer;
  /** 本次原型会话中新建的示例空间、容器和文件使用的递增 ID。 */
  let nextID = 1;
  /**
   * 函数功能：从容器内相对路径取最后一段名称。
   * @param {string} name 使用 / 分隔的示例路径。
   * @returns {string} 路径末段，末尾为 / 时返回空串。
   * 异常：{TypeError} name 不是支持 split 的字符串。
   * 注意事项：不验证磁盘路径，不处理 Windows 反斜杠。
   */
  const basename = (name) => name.split('/').pop();
  /**
   * 函数功能：取得条目父目录文案；根目录条目返回中文“根目录”。
   * @param {string} name 使用 / 分隔的示例路径。
   * @returns {string} 最后一个 / 前的部分，无 / 返回“根目录”。
   * 异常：{TypeError} name 不支持字符串路径操作。
   * 注意事项：前导 / 可得到空串；不规范化 . 或 ..。
   */
  const parent = (name) => name.includes('/') ? name.slice(0, name.lastIndexOf('/')) : '根目录';
  /**
   * 函数功能：按当前 state.container 查找活动容器，失配时回退到第一个示例容器。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {Object|undefined} 当前 ID 对应容器；无匹配取首项，空集合返回 undefined。
   * 异常：{TypeError} 容器集合结构被破坏时可能抛出。
   * 注意事项：仅查询内存，不校验空间归属或打开权限。
   */
  const currentContainer = () => data.containers.find((item) => item.id === state.container) || data.containers[0];
  /**
   * 函数功能：按当前 state.space 查找活动空间，失配时回退到第一个示例空间。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {Object|undefined} 当前 ID 对应空间；无匹配取首项，空集合返回 undefined。
   * 异常：{TypeError} 空间集合结构被破坏时可能抛出。
   * 注意事项：部分渲染方直接读取 name，原型需保留至少一个空间。
   */
  const currentSpace = () => data.spaces.find((space) => space.id === state.space) || data.spaces[0];
  /**
   * 函数功能：将字节数格式化为适合列表展示的单位。
   * @param {number} bytes 预期为非负文件字节数。
   * @returns {string} 按 1024 进位的大小文案，大单位保留一位小数。
   * 异常：无显式异常或范围错误返回。
   * 注意事项：不主动拒绝负数、NaN、Infinity；KB 等标签实际使用二进制倍率。
   */
  const size = (bytes) => {
    if (bytes < 1024) return `${bytes} B`;
    let value = bytes / 1024, index = 0;
    const units = ['KB', 'MB', 'GB', 'TB'];
    // 每轮进位到下一个单位，直到数值可读或已经到达 TB。
    while (value >= 1024 && index < units.length - 1) { value /= 1024; index++; }
    return `${value.toFixed(1)} ${units[index]}`;
  };
  /**
   * 函数功能：显示短暂操作结果提示；同一时刻只保留最新提示。
   * @param {string} message 要展示的临时提示。
   * @returns {undefined} 更新提示并安排三秒后隐藏。
   * 异常：{TypeError} #toast 节点不存在。
   * 注意事项：使用 textContent；取消旧定时器，不持久化提示。
   */
  const toast = (message) => {
    clearTimeout(toastTimer);
    $('#toast').textContent = message;
    $('#toast').hidden = false;
    toastTimer = setTimeout(() => { $('#toast').hidden = true; }, 3000);
  };
  /**
   * 函数功能：切换主页面、同步 URL hash、关闭弹窗并重新绘制。
   * @param {string} page 页面键，调用方应使用已知页面名。
   * @returns {undefined} 修改页面状态、URL hash、关闭弹窗并重新渲染。
   * 异常：{Error} DOM 操作或后续渲染失败时向上传播。
   * 注意事项：不验证页面键；replaceState 不新增浏览历史。
   */
  function navigate(page) {
    state.page = page;
    if (location.hash !== `#${page}`) history.replaceState(null, '', `#${page}`);
    if ($('#modal').open) $('#modal').close();
    render();
  }
  /**
   * 函数功能：根据空间、容器和折叠状态生成左侧导航 HTML。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 各空间及其容器的侧栏 HTML。
   * 异常：{TypeError} 示例数据或图标依赖缺失。
   * 注意事项：每轮生成一个空间行，再按 containers.space 筛选其容器；collapsed 控制子项显示，不执行解锁。
   */
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
  /**
   * 函数功能：生成当前空间名、页面标题和文件页工具栏。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 当前页面顶栏 HTML，文件页附带操作入口。
   * 异常：{TypeError} 空间为空或其他必需状态缺失。
   * 注意事项：至少需要一个空间；生成入口不执行备份或锁定。
   */
  function renderTopbar() {
    const titles = { home: '我的容器', files: currentContainer()?.name || '个人文件', create: '新建容器', unlock: '打开容器', foundations: '视觉规范' };
    const actions = state.page === 'files' ? `<div class="top-actions"><button class="toolbar-button" data-action="backup">${icon('download')}备份</button><button class="toolbar-button" data-action="refresh">${icon('refresh')}刷新</button><button class="toolbar-button" data-action="lock">${icon('lock')}锁定</button></div>` : '<span class="local-status"><span class="status-dot"></span>本地模式</span>';
    return `<header class="topbar"><div class="topbar-crumb"><button class="toolbar-button sidebar-toggle" data-action="sidebar" aria-label="${state.sidebarVisible ? '收起侧栏' : '展开侧栏'}" title="${state.sidebarVisible ? '收起侧栏' : '展开侧栏'}">${icon('sidebar')}</button><span>${escape(currentSpace().name)}</span><span class="slash">/</span><b>${escape(titles[state.page])}</b></div>${actions}</header>`;
  }
  /**
   * 函数功能：把一条容器摘要渲染成可点击卡片。
   * @param {Object} item 示例容器，含 id:string、name:string、description:string 等展示字段。
   * @returns {string} 可点击的容器卡片 HTML。
   * 异常：{TypeError} 对象或图标表缺失。
   * 注意事项：名称和描述会转义，id 直接用于属性，必须为可信内部标识。
   */
  function containerCard(item) {
    return `<button class="recent-card" data-action="open" data-id="${item.id}"><span class="recent-icon">${icon('shield')}</span><span class="recent-info"><strong>${escape(item.name)}</strong><small>${escape(item.description)}</small></span>${icon('arrow', 'recent-arrow')}</button>`;
  }
  /**
   * 函数功能：生成容器首页，并按当前空间和空、加载、错误状态选择对应内容。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 当前空间的首页 HTML，含空、加载和错误等展示状态。
   * 异常：{TypeError} 当前空间或图标资源缺失。
   * 注意事项：cards 每项为当前空间的容器；empty 强制显示空集合，loading 显示骨架，不发网络请求。
   */
  function home() {
    const cards = state.status === 'empty' ? [] : data.containers.filter((item) => item.space === state.space);
    return `<div class="page-content">
      ${state.status === 'error' ? `<div class="error-box" role="alert">${icon('alert')}空间列表暂时无法读取。<button class="text-button" data-action="retry">重试</button></div>` : ''}
      <div class="page-heading"><div><div class="eyebrow">YOUR PRIVATE SPACE</div><h1>我的容器</h1><p>${escape(currentSpace().name)} · 为重要文件留一个安心的地方。</p></div>${button('新建容器', 'create', 'plus', 'primary')}</div>
      <section class="welcome-card"><div class="welcome-copy"><span class="pill"><span class="status-dot"></span>一切都在你的掌控中</span><h2>安全，从这里开始。</h2><p>创建一个加密容器，或导入已有容器。<br>你的文件加密保存在本机，由你掌管。</p><div class="button-row">${button('创建容器', 'create', 'plus', 'primary')}${button('导入容器', 'import-container', 'folder')}</div></div><img class="hero-art" src="assets/vault.svg" alt="" aria-hidden="true"></section>
      <section class="recent-section"><div class="section-heading"><div><h3>${escape(currentSpace().name)}的容器</h3><p>选择容器并输入密码打开</p></div><button class="text-button" data-action="import-container">导入容器 ${icon('arrow')}</button></div>
        ${state.status === 'loading' ? '<div class="recent-grid" aria-busy="true" aria-label="正在加载容器"><div class="skeleton" style="height:74px"></div><div class="skeleton" style="height:74px"></div></div>' : cards.length ? `<div class="recent-grid">${cards.map(containerCard).join('')}</div>` : `<div class="recent-empty"><span class="recent-icon">${icon('folder')}</span><div><strong>这个空间还没有容器</strong><p>创建或导入容器后，它会显示在这里。</p></div></div>`}
      </section>
    </div>`;
  }
  /**
   * 函数功能：根据按类型 / 按路径模式合成可显示的文件与文件夹条目。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {Array<Object>} 文件项含 kind:"file"、file、name、path；目录项含 kind:"folder"、name、path。
   * 异常：{TypeError} 文件字段缺失或类型错误。
   * 注意事项：类型模式保持源顺序；路径模式先排目录再接文件，最后按完整路径搜索。无磁盘访问，不隐藏点文件。
   */
  function entries() {
    if (state.status === 'empty') return [];
    let result;
    if (state.mode === 'type') {
      result = data.files.filter((file) => !state.selected.length || state.selected.includes(file.category)).map((file) => ({ kind: 'file', file, name: basename(file.name), path: file.name }));
    } else {
      const prefix = state.directory ? `${state.directory}/` : '';
      // 去重后的直接子目录完整路径集合，每项对应当前目录下的一层目录，而不是全部后代文件。
      const folders = new Set();
      // 从容器目录记录中提取当前层的第一段子目录，Set 用来合并重复目录名。
      for (const path of data.directories) {
        if (path.startsWith(prefix) && path !== state.directory) {
          const child = path.slice(prefix.length).split('/')[0];
          if (child) folders.add(prefix + child);
        }
      }
      // 当前目录直接包含的文件行对象集合，保留 data.files 中的原始顺序。
      const files = [];
      // 每轮一个共享示例文件，并未按当前容器 ID 隔离；更深路径贡献直接子目录，当前层文件加入 files。
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
  /**
   * 函数功能：生成文件浏览区域的加载、错误、空状态或文件网格 / 列表。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 按当前布局生成的文件或目录结果区 HTML。
   * 异常：{Error} 数据整理、图标或子模板异常向上传播。
   * 注意事项：loading/error 优先；rows 每轮处理一个条目，日期为固定示例，不加载真实缩略图或导出文件。
   */
  function results() {
    if (state.status === 'loading') return `<div class="file-grid" aria-busy="true" aria-label="正在加载文件">${Array.from({ length: 8 }, () => '<div><div class="skeleton" style="height:105px"></div><div class="skeleton skeleton-line"></div></div>').join('')}</div>`;
    if (state.status === 'error') return `<div class="files-empty"><span class="empty-symbol">${icon('alert')}</span><strong>文件列表加载失败</strong><p>容器内容未发生变化，请稍后重试。</p>${button('重新加载', 'retry', 'refresh')}</div>`;
    // 统一取得筛选后的直接子项，网格和列表共用同一批业务条目。
    const rows = entries();
    if (!rows.length) return `<div class="files-empty"><span class="empty-symbol">${icon('folder')}</span><strong>${state.status === 'empty' ? '这里还是空的' : '这里没有文件'}</strong><p>${state.status === 'empty' ? '添加文件后会显示在这里。' : '试试选择其他分类、目录或搜索词。'}</p>${button('添加文件', 'add', 'plus')}</div>`;
    const rowAction = (row) => `data-action="${row.kind === 'folder' ? 'directory' : 'preview'}" data-id="${escape(row.kind === 'folder' ? row.path : row.file.id)}"`;
    if (state.layout === 'grid') return `<div class="file-grid">${rows.map((row) => `<button class="file-tile" ${rowAction(row)}><span class="tile-preview">${row.kind === 'file' && row.file.image ? `<img src="${row.file.image}" alt="">` : icon(row.kind === 'folder' ? 'folder' : categoryIcons[row.file.category])}</span><span class="tile-name" title="${escape(row.path)}">${escape(row.name)}</span><small>${row.kind === 'folder' ? '文件夹' : escape(state.mode === 'type' ? parent(row.file.name) : size(row.file.size))}</small></button>`).join('')}</div>`;
    return `<div class="file-list"><div class="list-head"><span>名称</span><span>大小</span><span class="file-version">版本</span><span class="file-date">加密时间</span><span>操作</span></div>${rows.map((row) => `<div class="file-row"><button class="file-name" ${rowAction(row)}>${icon(row.kind === 'folder' ? 'folder' : categoryIcons[row.file.category])}<span>${escape(row.name)}${row.kind === 'file' && state.mode === 'type' ? `<small>${escape(parent(row.file.name))}</small>` : ''}</span></button><span>${row.kind === 'file' ? size(row.file.size) : '—'}</span><span class="file-version">${row.kind === 'file' ? `v${row.file.version}` : '—'}</span><span class="file-date">${row.kind === 'file' ? '2026年10月29日' : '—'}</span><span class="row-actions">${row.kind === 'file' ? `<button class="icon-action" data-action="export" aria-label="导出 ${escape(row.name)}">${icon('download')}</button>` : ''}<button class="icon-action" data-action="delete" data-id="${escape(row.path)}" aria-label="删除 ${escape(row.name)}">${icon('trash')}</button></span></div>`).join('')}</div>`;
  }
  /**
   * 函数功能：生成文件分类路径或当前目录的逐级面包屑。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 类型标签或累计路径的导航 HTML。
   * 异常：{TypeError} 路径或相关状态不是预期类型。
   * 注意事项：路径模式每轮处理一个非空路径段；仅生成动作属性，不检查目录存在。
   */
  function breadcrumbs() {
    if (state.mode === 'type') return `<span>文件分类</span><span class="slash">/</span><span>${state.selected.length ? state.selected.map((id) => categoryNames[id]).join('、') : '全部'}</span>`;
    let path = '';
    return `<button data-action="directory" data-id="">全部文件</button>${state.directory.split('/').filter(Boolean).map((part) => { path = path ? `${path}/${part}` : part; return `${icon('chevron')}<button data-action="directory" data-id="${escape(path)}">${escape(part)}</button>`; }).join('')}`;
  }
  /**
   * 函数功能：生成文件浏览页框架、模式切换、筛选、搜索和结果区。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 文件页框架、筛选项及结果区域 HTML。
   * 异常：{Error} 数据或子模板缺失时向上传播。
   * 注意事项：每个类别生成一个按钮；条目统计和内容各调用 entries 一次，无异步间隔，不请求服务。
   */
  function files() {
    return `<div class="page-content browser-content">
      <div class="browser-controls">
        <div class="browse-tabs" role="tablist" aria-label="浏览方式"><button role="tab" data-action="mode" data-id="path" aria-selected="${state.mode === 'path'}">按路径</button><button role="tab" data-action="mode" data-id="type" aria-selected="${state.mode === 'type'}">按类型</button></div>
        <label class="search-box">${icon('search')}<input id="search" type="search" placeholder="搜索文件" aria-label="搜索文件" value="${escape(state.search)}"></label>
        <div class="browser-actions"><div class="layout-switch" aria-label="展示方式"><button data-action="layout" data-id="list" aria-label="列表" title="列表" aria-pressed="${state.layout === 'list'}">${icon('list')}</button><button data-action="layout" data-id="grid" aria-label="大图标" title="大图标" aria-pressed="${state.layout === 'grid'}">${icon('grid')}</button></div><button class="toolbar-button" data-action="add">${icon('plus')}添加</button></div>
      </div>
      ${state.mode === 'type' ? `<div class="type-filters" aria-label="文件类型，可多选"><button data-action="filter" data-id="all" aria-pressed="${!state.selected.length}">全部</button>${Object.entries(categoryNames).map(([id, label]) => `<button data-action="filter" data-id="${id}" aria-pressed="${state.selected.includes(id)}">${label}</button>`).join('')}</div>` : ''}
      <section class="browser-area" aria-label="文件浏览"><div class="browser-toolbar"><div class="breadcrumbs">${breadcrumbs()}</div><span class="file-count">${entries().length} 项</span></div><div id="browser-results">${results()}</div></section>
    </div>`;
  }
  /**
   * 函数功能：生成创建容器或解锁容器表单。
   * @param {boolean} create true 展示创建字段；false 展示解锁字段。
   * @returns {string} 对应模式的表单 HTML。
   * 异常：{TypeError} 所需状态或渲染依赖缺失时向上传播。
   * 注意事项：只生成字段；不校验密码，也不创建或解锁真实容器。
   */
  function form(create) {
    const loading = state.status === 'loading';
    return `<div class="page-content"><div class="form-wrap"><button class="back-button" data-action="home">${icon('left')}返回容器</button><div class="form-heading"><span class="form-icon">${icon(create ? 'plus' : 'unlock')}</span><div class="eyebrow">${create ? 'NEW CONTAINER' : 'OPEN CONTAINER'}</div><h1>${create ? '创建新容器' : '解锁容器'}</h1><p>${create ? `在${escape(currentSpace().name)}中创建容器。` : '输入密码，打开你的私密空间。'}</p></div><form class="form-card" id="container-form" data-kind="${create ? 'create' : 'unlock'}"><div class="error-box" id="form-error" role="alert" ${state.status === 'error' ? '' : 'hidden'}>${icon('alert')}<span>${create ? '两次输入的密码不一致' : '密码不正确，请重试'}</span></div>
      ${create ? `<label class="field">容器名称<input name="name" placeholder="例如：个人文件" autocomplete="off" maxlength="100" ${loading ? 'disabled' : ''} required></label>` : `<div class="selected-container">${icon('shield')}<span><strong>${escape(currentContainer()?.name || '个人文件')}</strong><small>本机 / ${escape(currentSpace().name)}</small></span></div>`}
      <label class="field">容器密码<input name="password" type="password" placeholder="${create ? '设置容器密码' : '输入密码解锁'}" autocomplete="${create ? 'new-password' : 'current-password'}" ${loading ? 'disabled' : ''} required>${create ? '' : '<small class="field-hint">设计稿演示密码：demo。请勿输入真实密码。</small>'}</label>
      ${create ? `<label class="field">确认密码<input name="confirmation" type="password" placeholder="再次输入密码" autocomplete="new-password" ${loading ? 'disabled' : ''} required></label>` : ''}
      <button type="submit" class="button primary submit-button" ${loading ? 'disabled' : ''}>${loading ? '正在处理…' : create ? '创建容器' : '解锁容器'}${icon('arrow')}</button><p class="demo-caption">仅演示界面流程，不创建真实容器，不保存密码。</p></form><div class="privacy-note">${icon('lock')}密码用于本地解锁容器，请妥善保管。</div></div></div>`;
  }
  /**
   * 函数功能：使用设计 token 和图标映射生成视觉规范展示页。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {string} 颜色、字体、按钮、间距和图标规范示例 HTML。
   * 异常：{TypeError} 当前主题无法定位 token 颜色分组或其他资源缺失。
   * 注意事项：只读取 VeilDesignTokens；每个示例数组项生成一项展示，不修改设计源。
   */
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
  /**
   * 函数功能：以当前 state 重新生成整页，并同步无障碍状态、主题按钮和视口适配。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {undefined} 替换 #app 并更新导航标记与视口。
   * 异常：{Error} DOM、图标、token 或子模板异常向上传播。
   * 注意事项：未知页面回退 home；整页重建会丢弃旧节点局部状态和焦点。
   */
  function render() {
    const screens = { home, files, create: () => form(true), unlock: () => form(false), foundations };
    if (!screens[state.page]) state.page = 'home';
    $('#app').classList.toggle('browser-open', state.page === 'files');
    $('#app').classList.toggle('sidebar-hidden', !state.sidebarVisible);
    $('#app').innerHTML = `${renderSidebar()}<div class="app-main">${renderTopbar()}${screens[state.page]()}</div>`;
    document.querySelectorAll('[data-page]').forEach((link) => {
      if (link.dataset.page === state.page) link.setAttribute('aria-current', 'page'); else link.removeAttribute('aria-current');
    });
    $('#page-state').disabled = state.page === 'foundations';
    $('#page-state').value = state.status;
    $('[data-action="theme"]').innerHTML = icon(document.documentElement.dataset.theme === 'dark' ? 'sun' : 'moon');
    $('[data-action="theme"]').setAttribute('aria-label', document.documentElement.dataset.theme === 'dark' ? '切换浅色外观' : '切换深色外观');
    resize();
  }
  /**
   * 函数功能：按模拟视口宽度调整设计画布，并标记窄屏 / 平板 / 桌面断点。
   * 入参：无；读取当前内存状态和页面资源。
   * @returns {undefined} 更新画布宽度和断点标记。
   * 异常：{TypeError} 必需 DOM 节点缺失。
   * 注意事项：读取 #viewport 与滚动区宽度，用相同左右 padding 估算空间；不验证负宽度或非法数值，不是设备检测。
   */
  function resize() {
    const width = Number($('#viewport').value);
    const stage = $('.stage-scroll');
    const inset = parseFloat(getComputedStyle(stage).paddingLeft) * 2;
    const actual = Math.min(width, stage.clientWidth - inset);
    $('#design-frame').style.width = `${width}px`;
    $('#design-frame').dataset.size = actual <= 650 ? '390' : actual < 1000 ? '820' : '1160';
  }
  /**
   * 函数功能：更新并显示共用对话框。
   * @param {string} content 可信 HTML 对话框内容。
   * @param {boolean} [preview=false] 是否启用预览弹窗样式。
   * @returns {undefined} 更新共用 dialog，尚未打开时调用 showModal。
   * 异常：{TypeError|DOMException} 节点缺失或浏览器拒绝对话框状态操作。
   * 注意事项：content 直接写入 innerHTML，用户文本须事先转义。
   */
  function showDialog(content, preview = false) {
    const modal = $('#modal');
    modal.classList.toggle('preview-modal', preview);
    modal.innerHTML = content;
    if (!modal.open) modal.showModal();
  }
  /**
   * 函数功能：校验原型中选择的备份文件并更新导入表单状态。
   * @param {File|null|undefined} file 用户选择的首个候选备份文件。
   * @returns {undefined} 合法后缀更新导入名和按钮；非法仅显示错误。
   * 异常：{TypeError} 导入弹窗 DOM 尚未创建或文件对象不符合预期。
   * 注意事项：只核对 .veil 后缀，不读字节或校验包；非法选择不清除旧 importName 和按钮状态。
   */
  function selectImportFile(file) {
    const error = $('#import-file-error');
    if (!file || !file.name.toLowerCase().endsWith('.veil')) {
      error.hidden = false;
      return;
    }
    state.importName = file.name;
    $('#import-file-name').textContent = file.name;
    $('#import-file-hint').textContent = '点击更换文件';
    $('#import-next').disabled = false;
    error.hidden = true;
  }
  /**
   * 函数功能：按文件 ID 展示示例预览；不读取或解密真实文件。
   * @param {string} id data.files 中的示例文件 ID。
   * @returns {undefined} 匹配时打开预览；无匹配直接返回。
   * 异常：{TypeError|DOMException} 文件字段、图标依赖或对话框操作失败。
   * 注意事项：图片和文本来自可信示例；PDF 仅画示意线，其他类别回退图标，不下载或解密用户文件。
   */
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
  // 点击回调入参为 MouseEvent，返回 undefined；从事件元素向上寻找最近 data-action，禁用按钮忽略，未知动作无操作。
  // 统一事件委托在整页重建后仍有效；分支同步修改共享内存，缺 DOM 或异常字段可抛错，无集中异常处理。
  document.addEventListener('click', (event) => {
    const target = event.target instanceof Element ? event.target.closest('[data-action]') : null;
    if (!target || target.disabled) return;
    const { action, id } = target.dataset;
    // 依据按钮动作更新内存状态或打开确认弹窗；只有演示数据会被修改。
    switch (action) {
      case 'home': navigate('home'); break;
      case 'create': navigate('create'); break;
      case 'create-in-space': state.space = id; navigate('create'); break;
      case 'open': {
        const item = data.containers.find((entry) => entry.id === id);
        if (!item) break;
        state.container = item.id; state.space = item.space; navigate('unlock'); break;
      }
      case 'import-container':
        state.importName = '';
        showDialog(`<h2 id="modal-title">导入容器</h2><p>选择 .veil 文件，导入到「${escape(currentSpace().name)}」。</p><button type="button" class="recent-empty import-dropzone" data-action="choose-import-file" id="import-dropzone">${icon('file')}<span><strong id="import-file-name">选择或拖入文件</strong><small id="import-file-hint">支持 .veil 文件</small></span></button><div id="import-file-error" class="error-box" role="alert" hidden>请选择 .veil 文件</div><p class="demo-caption">界面演示不读取文件内容，不导入真实容器。</p><div class="modal-actions">${button('取消', 'close-dialog')}${button('继续', 'import-next', '', 'primary', 'id="import-next" disabled')}</div>`); break;
      case 'choose-import-file': {
        const picker = document.createElement('input');
        picker.type = 'file';
        picker.accept = '.veil';
        picker.addEventListener('change', () => {
          if (picker.files?.[0] && $('#import-dropzone')) selectImportFile(picker.files[0]);
        }, { once: true });
        picker.click();
        break;
      }
      case 'import-next':
        if (!state.importName) break;
        showDialog(`<h2 id="modal-title">导入容器</h2><p>输入容器密码，导入到「${escape(currentSpace().name)}」。</p><form id="import-form"><div class="selected-container">${icon('shield')}<span><strong>${escape(state.importName)}</strong><small>导入到 ${escape(currentSpace().name)}</small></span></div><label class="field">容器密码<input name="password" type="password" placeholder="输入容器密码" autocomplete="off" required><small class="field-hint">演示密码：demo。请勿输入真实密码。</small></label><div id="import-error" class="error-box" role="alert" hidden>演示密码为 demo，请勿使用真实密码</div><p class="demo-caption">仅演示界面流程，不读取文件内容，不导入真实容器。</p><div class="modal-actions">${button('取消', 'close-dialog')}<button type="submit" class="button primary">导入容器</button></div></form>`); break;
      case 'space': state.space = id; navigate('home'); break;
      case 'collapse': if (state.collapsed.has(id)) state.collapsed.delete(id); else state.collapsed.add(id); render(); break;
      case 'sidebar': state.sidebarVisible = !state.sidebarVisible; render(); break;
      // 只切换模式或布局，不清空目录、分类及搜索；与原生客户端的切换规则不完全相同。
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
      // 根据确认时的当前目录生成示例路径，不是打开弹窗时冻结目录；不读取真实文件，也不独立检查同名冲突。
      case 'confirm-add': {
        const name = `示例文件${nextID++}.txt`;
        const path = state.mode === 'path' && state.directory ? `${state.directory}/${name}` : name;
        data.files.push({ id: `added-${nextID}`, name: path, category: 'document', size: 2460, version: 1, text: '这是一个设计稿中的示例文件。' });
        state.status = 'normal'; $('#modal').close(); render(); toast('已添加示例文件'); break;
      }
      case 'delete': showDialog(`<h2 id="modal-title">删除确认</h2><p>确定从容器中删除「${escape(basename(id))}」${data.directories.includes(id) ? '及其全部文件' : ''}？</p><p class="demo-caption">仅移除临时示例条目，可通过「重置示例」恢复。</p><div class="modal-actions">${button('取消', 'close-dialog')}${button('确认删除', 'confirm-delete', 'trash', 'danger', `data-id="${escape(id)}"`)}</div>`); break;
      // 两次 filter 每轮一项共享文件/目录：删除精确路径及带 / 边界的后代，避免误删相似前缀。
      // 因数据未按容器隔离，此处删除会影响所有演示容器的浏览结果。
      case 'confirm-delete': data.files = data.files.filter((file) => file.name !== id && !file.name.startsWith(`${id}/`)); data.directories = data.directories.filter((path) => path !== id && !path.startsWith(`${id}/`)); $('#modal').close(); render(); toast('已移除示例条目'); break;
      case 'theme': document.documentElement.dataset.theme = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark'; render(); break;
      // 重置业务示例与主要浏览状态；spread 保留未显式覆盖的扩展字段（如 importName），主题也不恢复默认。
      case 'reset': data = structuredClone(seed); state = { ...state, space: 'personal', container: 'private', mode: 'type', layout: 'grid', selected: [], directory: '', search: '', status: 'normal', sidebarVisible: true, collapsed: new Set() }; nextID = 1; navigate('home'); toast('示例数据已重置'); break;
    }
  });
  // 导入区域拖拽悬停时阻止浏览器打开文件，并显示可放置状态。
  document.addEventListener('dragover', (event) => {
    if (event.target instanceof Element && event.target.closest('#import-dropzone')) {
      event.preventDefault();
      $('#import-dropzone').classList.add('dragging');
    }
  });
  document.addEventListener('dragleave', (event) => {
    const zone = $('#import-dropzone');
    if (zone && event.target instanceof Node && zone.contains(event.target) && !zone.contains(event.relatedTarget)) zone.classList.remove('dragging');
  });
  // 只消费导入区域中的拖放文件；其他位置仍使用浏览器默认行为。
  document.addEventListener('drop', (event) => {
    if (!(event.target instanceof Element) || !event.target.closest('#import-dropzone')) return;
    event.preventDefault();
    $('#import-dropzone').classList.remove('dragging');
    selectImportFile(event.dataTransfer?.files[0]);
  });
  // 提交回调入参为 SubmitEvent，返回 undefined；先阻止默认提交，只有 HTMLFormElement 才读取 FormData。
  // 仅接受已知表单 ID，错误保留输入；通过后重置字段并修改示例数组，无网络请求，密码没有保存到 data。
  // FormData 和局部字符串仍短暂持有输入，reset 不保证浏览器内存副本被安全擦除。
  document.addEventListener('submit', (event) => {
    event.preventDefault();
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    const values = new FormData(form);
    // 导入示例流程只接受演示密码，并创建一条内存中的容器摘要。
    if (form.id === 'import-form') {
      if (values.get('password') !== 'demo') { $('#import-error').hidden = false; return; }
      const id = `container-${nextID++}`;
      data.containers.push({ id, space: state.space, name: state.importName.replace(/\.veil$/i, '') || '容器', description: `本机 / ${currentSpace().name}` });
      state.container = id; state.status = 'normal'; state.directory = ''; state.selected = []; state.search = '';
      form.reset(); $('#modal').close(); navigate('files'); toast('已导入示例容器'); return;
    }
    // 新建空间先按完全相同字符串检查名称和非空路径唯一性，不规范化大小写、别名或实际磁盘路径。
    // 每次 some 遍历一条已有空间记录，发现首个冲突即停止；通过后追加到本会话数组。
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
    // 其余表单必须是创建 / 解锁容器表单，避免处理对话框内的无关提交。
    if (form.id !== 'container-form') return;
    const create = form.dataset.kind === 'create';
    const name = String(values.get('name') || '').trim();
    const password = String(values.get('password') || '');
    let message = '';
    if (!password) message = '请输入容器密码';
    else if (create && !name) message = '请输入容器名称';
    else if (create && password !== String(values.get('confirmation') || '')) message = '两次输入的密码不一致';
    else if (!create && password !== 'demo') message = '演示密码为 demo，请勿使用真实密码';
    // 按业务条件逐项验证密码、容器名和确认密码；失败时保留表单并展示首条错误。
    if (message) { $('#form-error').hidden = false; $('#form-error span').textContent = message; return; }
    if (create) {
      const id = `container-${nextID++}`;
      data.containers.push({ id, space: state.space, name, description: `本机 / ${currentSpace().name}` }); state.container = id;
    }
    form.reset(); state.status = 'normal'; state.directory = ''; state.selected = []; state.search = ''; navigate('files'); toast(create ? '已创建示例容器，以下为演示文件。' : '已解锁示例容器');
  });
  // input 回调接收页面输入事件，只处理 id=search；更新搜索字符串后同步重绘结果和计数，返回 undefined。
  // 不重建搜索框以保留焦点；依赖浏览页节点存在，未做节流，每次输入都重新遍历示例条目。
  document.addEventListener('input', (event) => {
    if (event.target.id === 'search') { state.search = event.target.value; $('#browser-results').innerHTML = results(); $('.file-count').textContent = `${entries().length} 项`; }
  });
  $('#viewport').addEventListener('change', resize);
  $('#page-state').addEventListener('change', (event) => { state.status = event.target.value; render(); });
  window.addEventListener('resize', resize);
  window.addEventListener('hashchange', () => { state.page = location.hash.slice(1) || 'home'; render(); });
  state.page = location.hash.slice(1) || 'home';
  render();
})();
