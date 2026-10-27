import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import "./styles.css";
import { Button, Dialog, Icon } from "./ui";

type FileView = {
  id: string;
  name: string;
  size: number;
  version: number;
  encryptedAt: string;
  category: FileCategory;
  previewMime: string | null;
};
type ContainerView = {
  name: string;
  id: string;
  path: string;
  createdAt: string;
  files: FileView[];
};
type FileCategory = "photo" | "video" | "document" | "archive" | "other";
type ContainerSummary = { id: string; name: string; path: string };
type Space = { id: string; name: string; path: string; containers: ContainerSummary[] };
type Recent = { name: string; path: string };
type Screen = "home" | "create" | "open" | "container";




function readRecents(): Recent[] {
  try {
    const data: unknown = JSON.parse(localStorage.getItem("veil.recents") ?? "[]");
    return Array.isArray(data) ? data.filter((item): item is Recent => typeof item?.name === "string" && typeof item?.path === "string").slice(0, 6) : [];
  } catch { return []; }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024, index = 0;
  while (value >= 1024 && index < units.length - 1) { value /= 1024; index++; }
  return `${value.toFixed(1)} ${units[index]}`;
}
function formatDate(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : new Intl.DateTimeFormat("zh-CN", { year:"numeric", month:"short", day:"numeric" }).format(date);
}
function basename(path: string): string { return path.split("/").pop() || path; }
function parentName(path: string): string { return path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "根目录"; }
const categories: { id: FileCategory; label: string; icon: string }[] = [
  {id:"photo",label:"照片",icon:"image"}, {id:"video",label:"视频",icon:"video"},
  {id:"document",label:"文档",icon:"file"}, {id:"archive",label:"压缩包",icon:"archive"}, {id:"other",label:"其他",icon:"file"},
];
type DisplayItem = { kind: "folder"; name: string; path: string } | { kind: "file"; file: FileView };
type Preview = { file: FileView; loading: boolean; url?: string; text?: string; error?: string };
type DeleteTarget = { name: string; path: string; isFolder: boolean };

function App() {
  const [screen, setScreen] = useState<Screen>("home");
  const [spaces, setSpaces] = useState<Space[]>([]);
  const [spaceId, setSpaceId] = useState("default");
  const [expanded, setExpanded] = useState<Record<string, boolean>>({default:true});
  const [recents, setRecents] = useState<Recent[]>(readRecents);
  const [container, setContainer] = useState<ContainerView | null>(null);
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const [spaceDialog, setSpaceDialog] = useState(false);
  const [spaceName, setSpaceName] = useState("");
  const [importDialog, setImportDialog] = useState(false);
  const [deleteTarget, setDeleteTarget] = useState<DeleteTarget | null>(null);
  const [viewMode, setViewMode] = useState<"type" | "path">("type");
  const [layout, setLayout] = useState<"grid" | "list">("grid");
  const [selectedTypes, setSelectedTypes] = useState<FileCategory[]>([]);
  const [directory, setDirectory] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const previewUrl = useRef<string | null>(null);
  const previewSequence = useRef(0);
  const [thumbnails, setThumbnails] = useState<Record<string, string>>({});
  const activeSpace = spaces.find((space) => space.id === spaceId);
  const actualSpace = container ? spaces.find((space) => space.containers.some((item) => item.path === container.path)) : undefined;

  async function loadSpaces() {
    const result = await invoke<Space[]>("list_spaces");
    setSpaces(result);
    return result;
  }
  useEffect(() => { let alive = true; invoke<Space[]>("list_spaces").then((result) => { if (alive) setSpaces(result); }).catch((reason) => { if (alive) setError(String(reason)); }); return () => { alive = false; }; }, []);
  useEffect(() => () => { if (previewUrl.current) URL.revokeObjectURL(previewUrl.current); }, []);

  function clearPreview() {
    previewSequence.current++;
    if (previewUrl.current) URL.revokeObjectURL(previewUrl.current);
    previewUrl.current = null;
    setPreview(null);
  }
  function navigate(next: Screen, selectedPath = "") {
    setScreen(next); setPath(selectedPath); setName(""); setPassword(""); setConfirmation(""); setError(""); setNotice("");
  }
  function remember(view: ContainerView) {
    const next = [{name:view.name,path:view.path}, ...recents.filter((item) => item.path !== view.path)].slice(0, 6);
    setRecents(next);
    try { localStorage.setItem("veil.recents", JSON.stringify(next)); } catch { /* 最近记录不可用时不影响容器操作 */ }
  }
  async function leaveContainer(): Promise<boolean> {
    if (!container) return true;
    clearPreview();
    try { await invoke("lock_container"); setContainer(null); return true; }
    catch (reason) { setError(String(reason)); return false; }
  }
  async function switchSpace(id: string, next: Screen = "home") {
    if (busy) return;
    setBusy(true);
    try {
      if (!(await leaveContainer())) return;
      setSpaceId(id); setExpanded((current) => ({...current,[id]:true})); setDirectory(""); setSelectedTypes([]); navigate(next);
    } finally { setBusy(false); }
  }
  async function chooseContainer(id: string, item: ContainerSummary) {
    if (busy) return;
    if (container?.path === item.path) { setSpaceId(id); setScreen("container"); return; }
    setBusy(true);
    try { if (!(await leaveContainer())) return; setSpaceId(id); navigate("open", item.path); }
    finally { setBusy(false); }
  }
  async function chooseDirectory() {
    try { const selected = await open({directory:true,multiple:false,title:"选择容器目录"}); if (typeof selected === "string") setPath(selected); }
    catch (reason) { setError(`无法选择目录：${String(reason)}`); }
  }
  async function submit(event: React.FormEvent) {
    event.preventDefault(); setError("");
    if (!password) { setError("请输入容器密码"); return; }
    if (screen === "open" && !path) { setError("请选择容器目录"); return; }
    if (screen === "create" && !name.trim()) { setError("请输入容器名称"); return; }
    if (screen === "create" && password !== confirmation) { setError("两次输入的密码不一致"); return; }
    const secret = password; setPassword(""); setConfirmation(""); setBusy(true);
    try {
      const view = screen === "create"
        ? await invoke<ContainerView>("create_container", {space:spaceId,name:name.trim(),password:secret})
        : await invoke<ContainerView>("open_container", {path,password:secret});
      setContainer(view); remember(view); setDirectory(""); setSelectedTypes([]); setViewMode("type"); setScreen("container");
      try { const next = await loadSpaces(); const matched = next.find((space) => space.containers.some((item) => item.path === view.path)); if (matched) { setSpaceId(matched.id); setExpanded((current) => ({...current,[matched.id]:true})); } }
      catch (reason) { setNotice(`容器已打开，但空间列表刷新失败：${String(reason)}`); }
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }
  async function lock() {
    setBusy(true); setError("");
    try { if (await leaveContainer()) { setDirectory(""); navigate("home"); } }
    finally { setBusy(false); }
  }
  async function refresh() {
    setBusy(true); setError(""); clearPreview();
    try { setContainer(await invoke<ContainerView>("refresh_container")); setNotice("文件列表已刷新"); }
    catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }
  async function createSpace(event: React.FormEvent) {
    event.preventDefault(); setError(""); setBusy(true);
    try {
      const next = await invoke<Space[]>("create_space", {name:spaceName});
      setSpaces(next);
      const created = next.find((space) => space.name === spaceName.trim());
      if (!created) throw new Error("空间已创建，但无法从列表中找到");
      if (!(await leaveContainer())) return;
      setSpaceId(created.id); setExpanded((current) => ({...current,[created.id]:true})); setSpaceDialog(false); setSpaceName(""); navigate("home");
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }

  const visible = useMemo<DisplayItem[]>(() => {
    if (!container) return [];
    if (viewMode === "type") return container.files.filter((file) => selectedTypes.length === 0 || selectedTypes.includes(file.category)).map((file) => ({kind:"file",file}));
    const prefix = directory ? `${directory}/` : "";
    const folders = new Map<string, DisplayItem>(); const files: DisplayItem[] = [];
    for (const file of container.files) {
      if (!file.name.startsWith(prefix)) continue;
      const tail = file.name.slice(prefix.length); if (!tail) continue;
      const slash = tail.indexOf("/");
      if (slash >= 0) { const child=tail.slice(0,slash); folders.set(child,{kind:"folder",name:child,path:`${prefix}${child}`}); }
      else files.push({kind:"file",file});
    }
    return [...[...folders.values()].sort((a,b) => (a.kind === "folder" ? a.name : "").localeCompare(b.kind === "folder" ? b.name : "", "zh-CN")), ...files.sort((a,b) => (a.kind === "file" ? a.file.name : "").localeCompare(b.kind === "file" ? b.file.name : "", "zh-CN"))];
  }, [container, directory, viewMode, selectedTypes]);

  useEffect(() => {
    if (screen !== "container" || layout !== "grid") { setThumbnails({}); return; }
    let active = true; const urls: string[] = [];
    setThumbnails({});
    const candidates = visible.filter((item): item is {kind:"file";file:FileView} => item.kind === "file" && !!item.file.previewMime?.startsWith("image/") && item.file.size <= 2 * 1024 * 1024).slice(0, 12);
    void (async () => { for (const item of candidates) {
      if (!active) break;
      try {
        const bytes = await invoke<ArrayBuffer>("preview_file", {name:item.file.name});
        if (!active) break;
        const url = URL.createObjectURL(new Blob([bytes], {type:item.file.previewMime || "application/octet-stream"})); urls.push(url);
        setThumbnails((previous) => ({...previous,[item.file.id]:url}));
      } catch { /* 缩略图不可用时保留类型图标 */ }
    } })();
    return () => { active = false; for (const url of urls) URL.revokeObjectURL(url); };
  }, [visible, layout, screen]);

  function toggleType(type: FileCategory) { setSelectedTypes((current) => current.includes(type) ? current.filter((item) => item !== type) : [...current,type]); }
  async function importItems(kind: "files" | "directory") {
    setImportDialog(false); setError("");
    try {
      const selected = await open({directory:kind === "directory",multiple:kind === "files",title:kind === "directory" ? "选择文件夹" : "选择文件"});
      const sources = typeof selected === "string" ? [selected] : selected || [];
      if (!sources.length) return;
      setBusy(true);
      const result = await invoke<ContainerView>("import_files", {sources,directory:viewMode === "path" ? directory : ""});
      setContainer(result); setNotice("文件已添加");
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }
  async function previewFile(file: FileView) {
    clearPreview(); const sequence = ++previewSequence.current;
    if (!file.previewMime) { setPreview({file,loading:false,error:"此类型暂不支持内置预览，可导出后查看。"}); return; }
    setPreview({file,loading:true});
    try {
      const bytes = await invoke<ArrayBuffer>("preview_file", {name:file.name});
      if (sequence !== previewSequence.current) return;
      if (file.previewMime === "text/plain") { setPreview({file,loading:false,text:new TextDecoder().decode(bytes)}); return; }
      const url = URL.createObjectURL(new Blob([bytes], {type:file.previewMime}));
      previewUrl.current = url; setPreview({file,loading:false,url});
    } catch (reason) { if (sequence === previewSequence.current) setPreview({file,loading:false,error:String(reason)}); }
  }
  async function exportFile(file: FileView) {
    setError("");
    try {
      const destination = await save({defaultPath:basename(file.name), title:"导出文件"}); if (!destination) return;
      setBusy(true); await invoke("export_file", {name:file.name,destination}); setNotice("文件已导出");
    } catch (reason) { setError(String(reason)); }
    finally { setBusy(false); }
  }
  async function confirmDelete() {
    if (!deleteTarget) return;
    setBusy(true); setError("");
    try { const result = await invoke<ContainerView>("remove_path", {name:deleteTarget.path}); clearPreview(); setContainer(result); setDeleteTarget(null); setNotice("已删除"); }
    catch (reason) { setError(String(reason)); setDeleteTarget(null); }
    finally { setBusy(false); }
  }

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><span className="brand-mark"><Icon name="shield" size={16}/></span><strong>veil</strong></div>
      <div className="sidebar-caption">空间</div>
      <div className="spaces-list">
        {spaces.map((space) => <div className="space-group" key={space.id}>
          <div className={`space-row ${space.id === spaceId && screen === "home" ? "active" : ""}`}>
            <button className="space-disclosure" aria-label={`${expanded[space.id] ? "收起" : "展开"}${space.name}`} aria-expanded={!!expanded[space.id]} onClick={() => setExpanded((value) => ({...value,[space.id]:!value[space.id]}))}>{expanded[space.id] ? "▾" : "▸"}</button>
            <button className="space-name" disabled={busy} onClick={() => void switchSpace(space.id)}><Icon name="folder" size={16}/><span>{space.name}</span></button>
            <button className="space-add" aria-label={`在${space.name}新建容器`} disabled={busy} onClick={() => void switchSpace(space.id,"create")}><Icon name="plus" size={16}/></button>
          </div>
          {expanded[space.id] && <div className="space-containers">{space.containers.map((item) => <button key={item.id} className={`sidebar-container ${container?.path === item.path && screen === "container" ? "active" : ""}`} disabled={busy} onClick={() => void chooseContainer(space.id,item)}><Icon name="shield" size={15}/><span>{item.name}</span></button>)}</div>}
        </div>)}
      </div>
      <button className="new-space" disabled={busy} onClick={() => { setSpaceName(""); setError(""); setSpaceDialog(true); }}><span>＋</span> 新建空间</button>
      <div className="sidebar-local"><Icon name="lock" size={14}/> 本机</div>
    </aside>
    <main className="main">
      <header className="topbar"><span>{actualSpace?.name || activeSpace?.name || "默认空间"} <span className="crumb">/</span> {screen === "container" ? container?.name : screen === "create" ? "新建容器" : screen === "open" ? "打开容器" : "我的容器"}</span>
        {screen === "container" ? <div className="top-actions"><button className="toolbar-button" disabled={busy} onClick={() => void refresh()}><Icon name="refresh" size={15}/> 刷新</button><button className="toolbar-button" disabled={busy} onClick={() => void lock()}><Icon name="lock" size={15}/> 锁定</button></div> : <span className="top-label"><span className="status-dot"/> 本地模式</span>}
      </header>
      <div className={`main-content ${screen === "container" ? "container-content" : ""}`}>
        {error && !spaceDialog && <div className="error spaced" role="alert">{error}</div>}
        {notice && <div className="notice" role="status">{notice}<button onClick={() => setNotice("")} aria-label="关闭提示">×</button></div>}
        {screen === "home" && <>
          <div className="page-heading"><div><div className="eyebrow">YOUR PRIVATE SPACE</div><h1>我的容器</h1><p>{activeSpace?.name || "默认空间"} · 为重要文件留一个安心的地方。</p></div><Button variant="primary" disabled={!activeSpace || busy} onClick={() => navigate("create")}><Icon name="plus" size={17}/> 新建容器</Button></div>
          <section className="welcome-card"><div className="welcome-copy"><span className="pill"><span className="status-dot"/> 一切都在你的掌控中</span><h2>私密，从这里开始。</h2><p>创建一个加密容器，或打开已有容器。<br/>你的文件保存在你选择的位置。</p><div className="button-row"><Button variant="primary" disabled={!activeSpace || busy} onClick={() => navigate("create")}><Icon name="plus" size={17}/> 创建容器</Button><Button variant="secondary" onClick={() => navigate("open")}><Icon name="folder" size={17}/> 打开容器</Button></div></div><div className="hero-art" aria-hidden="true"><div className="orbit orbit-outer"/><div className="orbit orbit-inner"/><div className="hero-vault"><div className="vault-top"><span/><span/><span/></div><div className="vault-center"><Icon name="shield" size={53}/></div><div className="vault-lines"><i/><i/></div></div><span className="art-dot dot-a"/><span className="art-dot dot-b"/><span className="art-square"/></div></section>
          <section className="recent-section"><div className="section-heading"><div><h3>{activeSpace?.name || "默认空间"}的容器</h3><p>选择容器并输入密码打开</p></div><button className="text-button" onClick={() => navigate("open")}>打开其他容器 <Icon name="arrow" size={15}/></button></div>
            {!activeSpace?.containers.length ? <div className="recent-empty"><span className="empty-icon"><Icon name="folder"/></span><div><strong>这个空间还没有容器</strong><p>创建容器后，它会显示在这里。</p></div></div> : <div className="recent-grid">{activeSpace.containers.map((item) => <button className="recent-card" key={item.id} onClick={() => void chooseContainer(activeSpace.id,item)}><span className="recent-icon"><Icon name="shield"/></span><span className="recent-info"><strong>{item.name}</strong><small title={item.path}>{item.path}</small></span><Icon name="arrow" size={16}/></button>)}</div>}
          </section>
          {recents.some((recent) => !spaces.some((space) => space.containers.some((item) => item.path === recent.path))) && <section className="recent-section"><div className="section-heading"><div><h3>最近打开</h3><p>包括尚未登记到空间的容器</p></div></div><div className="recent-grid">{recents.filter((recent) => !spaces.some((space) => space.containers.some((item) => item.path === recent.path))).map((item) => <button className="recent-card" key={item.path} onClick={() => navigate("open",item.path)}><span className="recent-icon"><Icon name="shield"/></span><span className="recent-info"><strong>{item.name}</strong><small>{item.path}</small></span><Icon name="arrow" size={16}/></button>)}</div></section>}
        </>}
        {(screen === "create" || screen === "open") && <div className="form-wrap"><button className="back-button" onClick={() => navigate("home")} disabled={busy}><Icon name="left" size={17}/> 返回容器</button><div className="form-heading"><span className="form-icon"><Icon name={screen === "create" ? "plus" : "unlock"} size={23}/></span><div className="eyebrow">{screen === "create" ? "NEW CONTAINER" : "OPEN CONTAINER"}</div><h1>{screen === "create" ? "创建新容器" : "打开容器"}</h1><p>{screen === "create" ? `在${activeSpace?.name || "当前空间"}中创建容器。` : "选择已有容器目录，输入密码解锁。"}</p></div><form className="form-card" onSubmit={(event) => void submit(event)}>
          {screen === "create" ? <><label>容器名称<input value={name} onChange={(event) => setName(event.target.value)} placeholder="例如：个人文件" maxLength={100} autoComplete="off" disabled={busy}/></label><label>保存位置<div className="location-value">{activeSpace?.path || "正在读取空间…"}</div><small>容器保存在当前空间的文件夹中。</small></label></> : <label>容器目录<span className="path-input"><input value={path} onChange={(event) => setPath(event.target.value)} placeholder="选择包含 .veil-info 的目录" autoComplete="off" disabled={busy}/><button type="button" onClick={() => void chooseDirectory()} disabled={busy}>选择目录</button></span></label>}
          <label>容器密码<input type="password" value={password} onChange={(event) => setPassword(event.target.value)} placeholder={screen === "create" ? "设置容器密码" : "输入容器密码"} autoComplete={screen === "create" ? "new-password" : "current-password"} disabled={busy}/></label>
          {screen === "create" && <label>确认密码<input type="password" value={confirmation} onChange={(event) => setConfirmation(event.target.value)} placeholder="再次输入容器密码" autoComplete="new-password" disabled={busy}/></label>}
          <Button variant="primary" className="submit-button" disabled={busy || (screen === "create" && !activeSpace)}>{busy ? "正在处理…" : screen === "create" ? "创建容器" : "解锁容器"}<Icon name="arrow" size={17}/></Button>
        </form><div className="privacy-note"><Icon name="lock" size={15}/> 密码用于本地解锁容器，请妥善保管。</div></div>}
        {screen === "container" && container && <>
          <div className="browse-tabs" aria-label="浏览方式"><button className={viewMode === "path" ? "active" : ""} aria-pressed={viewMode === "path"} onClick={() => setViewMode("path")}>按路径</button><button className={viewMode === "type" ? "active" : ""} aria-pressed={viewMode === "type"} onClick={() => setViewMode("type")}>按类型</button></div>
          {viewMode === "type" && <div className="type-filters" aria-label="文件类型，可多选"><button className={!selectedTypes.length ? "active" : ""} aria-pressed={!selectedTypes.length} onClick={() => setSelectedTypes([])}>全部</button>{categories.map((type) => <button key={type.id} className={selectedTypes.includes(type.id) ? "active" : ""} aria-pressed={selectedTypes.includes(type.id)} onClick={() => toggleType(type.id)}>{type.label}</button>)}</div>}
          <section className="browser-area"><div className="browser-toolbar"><div className="breadcrumbs">{viewMode === "type" ? <>文件分类 <span>/</span> {selectedTypes.length ? categories.filter((type) => selectedTypes.includes(type.id)).map((type) => type.label).join("、") : "全部"}</> : <><button onClick={() => setDirectory("")}>全部文件</button>{directory && directory.split("/").map((part,index,parts) => <React.Fragment key={parts.slice(0,index+1).join("/")}><span>/</span><button onClick={() => setDirectory(parts.slice(0,index+1).join("/"))}>{part}</button></React.Fragment>)}</>}</div>
            <div className="browser-actions"><div className="layout-switch" aria-label="展示方式"><button className={layout === "list" ? "active" : ""} aria-pressed={layout === "list"} onClick={() => setLayout("list")}><Icon name="list" size={15}/> 列表</button><button className={layout === "grid" ? "active" : ""} aria-pressed={layout === "grid"} onClick={() => setLayout("grid")}><Icon name="grid" size={15}/> 大图标</button></div><button className="toolbar-button" disabled={busy} onClick={() => setImportDialog(true)}>＋ 添加</button></div>
          </div>
          {visible.length === 0 ? <div className="files-empty"><span className="empty-folder"><Icon name="folder" size={28}/></span><strong>{container.files.length ? "这里没有文件" : "这里还是空的"}</strong><p>{container.files.length ? "试试选择其他分类或目录。" : "添加文件后会显示在这里。"}</p></div> : layout === "grid" ? <div className="file-grid">{visible.map((item) => item.kind === "folder" ? <button key={`d:${item.path}`} className="file-tile" onClick={() => setDirectory(item.path)}><span className="tile-preview folder"><Icon name="folder" size={40}/></span><span className="tile-name">{item.name}</span><small>文件夹</small></button> : <button key={item.file.id} className="file-tile" onClick={() => void previewFile(item.file)}><span className={`tile-preview ${item.file.category}`}>{thumbnails[item.file.id] ? <img src={thumbnails[item.file.id]} alt=""/> : <Icon name={categories.find((type) => type.id === item.file.category)?.icon || "file"} size={36}/>}</span><span className="tile-name" title={item.file.name}>{basename(item.file.name)}</span><small>{viewMode === "type" ? parentName(item.file.name) : formatSize(item.file.size)}</small></button>)}</div> : <div className="browser-list"><div className="browser-list-head"><span>名称</span><span>大小</span><span>版本</span><span>加密时间</span><span>操作</span></div>{visible.map((item) => item.kind === "folder" ? <div className="browser-row" key={`d:${item.path}`}><button className="browser-name" onClick={() => setDirectory(item.path)}><Icon name="folder" size={18}/><span>{item.name}</span></button><span>—</span><span>—</span><span>—</span><div className="row-buttons"><button aria-label={`删除 ${item.name}`} disabled={busy} onClick={() => setDeleteTarget({name:item.name,path:item.path,isFolder:true})}><Icon name="trash" size={16}/></button></div></div> : <div className="browser-row" key={item.file.id}><button className="browser-name" onClick={() => void previewFile(item.file)}><Icon name={categories.find((type) => type.id === item.file.category)?.icon || "file"} size={18}/><span>{basename(item.file.name)}{viewMode === "type" && <small>{parentName(item.file.name)}</small>}</span></button><span>{formatSize(item.file.size)}</span><span>v{item.file.version}</span><span>{formatDate(item.file.encryptedAt)}</span><div className="row-buttons"><button aria-label={`导出 ${item.file.name}`} disabled={busy} onClick={() => void exportFile(item.file)}><Icon name="download" size={16}/></button><button aria-label={`删除 ${item.file.name}`} disabled={busy} onClick={() => setDeleteTarget({name:basename(item.file.name),path:item.file.name,isFolder:false})}><Icon name="trash" size={16}/></button></div></div>)}</div>}
          </section>
        </>}
      </div>
      <footer className="footer"><span>VEIL <span className="footer-divider">/</span> PRIVATE BY DESIGN</span><span>你的文件，你的空间</span></footer>
    </main>
    {spaceDialog && <Dialog label="新建空间" onSubmit={(event) => void createSpace(event)}><h2>新建空间</h2><p>空间对应一个本地文件夹，容器保存在其中。</p><label>空间名称<input autoFocus value={spaceName} maxLength={80} onChange={(event) => setSpaceName(event.target.value)} placeholder="例如：工作文件" disabled={busy}/></label>{error && <div className="error" role="alert">{error}</div>}<div className="modal-actions"><Button variant="secondary" type="button" disabled={busy} onClick={() => {setSpaceDialog(false);setError("");}}>取消</Button><Button variant="primary" disabled={busy}>创建空间</Button></div></Dialog>}
    {importDialog && <Dialog label="添加到容器"><h2>添加到容器</h2><p>添加到{viewMode === "path" && directory ? `「${directory}」` : "根目录"}。同名文件不会被覆盖。</p><div className="modal-actions"><Button variant="secondary" onClick={() => setImportDialog(false)}>取消</Button><Button variant="secondary" onClick={() => void importItems("directory")}>选择文件夹</Button><Button variant="primary" onClick={() => void importItems("files")}>选择文件</Button></div></Dialog>}
    {deleteTarget && <Dialog label="删除确认"><h2>删除确认</h2><p>确定从容器中删除「{deleteTarget.name}」{deleteTarget.isFolder ? "及其全部文件" : ""}？</p><div className="modal-actions"><Button variant="secondary" disabled={busy} onClick={() => setDeleteTarget(null)}>取消</Button><Button variant="danger" disabled={busy} onClick={() => void confirmDelete()}>确认删除</Button></div></Dialog>}
    {preview && !deleteTarget && <Dialog className="preview-modal" label={`预览 ${basename(preview.file.name)}`}><div className="preview-heading"><div><h2>{basename(preview.file.name)}</h2><p>{parentName(preview.file.name)} · {formatSize(preview.file.size)} · v{preview.file.version}</p></div><button className="icon-action" aria-label="关闭预览" onClick={clearPreview}><Icon name="close" size={19}/></button></div>{error && <div className="error" role="alert">{error}</div>}<div className="preview-content">{preview.loading ? <span>正在解密预览…</span> : preview.error ? <span>{preview.error}</span> : preview.text !== undefined ? <pre>{preview.text}</pre> : preview.url && preview.file.previewMime?.startsWith("image/") ? <img src={preview.url} alt={basename(preview.file.name)}/> : preview.url && preview.file.previewMime?.startsWith("video/") ? <video src={preview.url} controls/> : preview.url && preview.file.previewMime === "application/pdf" ? <iframe src={preview.url} title={`预览 ${basename(preview.file.name)}`} sandbox=""/> : <span>暂不支持预览</span>}</div><div className="modal-actions"><Button variant="secondary" disabled={busy} onClick={() => void exportFile(preview.file)}><Icon name="download" size={16}/> 导出</Button><Button variant="secondary" disabled={busy} onClick={() => setDeleteTarget({name:basename(preview.file.name),path:preview.file.name,isFolder:false})}><Icon name="trash" size={16}/> 删除</Button></div></Dialog>}
  </div>;
}

createRoot(document.getElementById("root")!).render(<React.StrictMode><App/></React.StrictMode>);
