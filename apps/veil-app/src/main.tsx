import React, { useState } from "react";
import { createRoot } from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import "./styles.css";

type FileView = {
  id: string;
  name: string;
  size: number;
  version: number;
  encryptedAt: string;
};
type ContainerView = {
  name: string;
  id: string;
  path: string;
  createdAt: string;
  files: FileView[];
};
type Recent = { name: string; path: string };
type Screen = "home" | "create" | "open" | "container";

function Icon({ name, size = 20 }: { name: string; size?: number }) {
  const paths: Record<string, React.ReactNode> = {
    shield: (
      <>
        <path d="M12 2.8 19 5.5v5.7c0 4.4-2.8 7.7-7 9.8-4.2-2.1-7-5.4-7-9.8V5.5z" />
        <path d="m9.2 11.8 1.9 1.9 3.8-4" />
      </>
    ),
    grid: (
      <>
        <rect x="3" y="3" width="7" height="7" rx="1.5" />
        <rect x="14" y="3" width="7" height="7" rx="1.5" />
        <rect x="3" y="14" width="7" height="7" rx="1.5" />
        <rect x="14" y="14" width="7" height="7" rx="1.5" />
      </>
    ),
    plus: <path d="M12 5v14M5 12h14" />,
    folder: (
      <>
        <path d="M3 6.5A2.5 2.5 0 0 1 5.5 4H10l2 2h6.5A2.5 2.5 0 0 1 21 8.5v9A2.5 2.5 0 0 1 18.5 20h-13A2.5 2.5 0 0 1 3 17.5z" />
      </>
    ),
    arrow: <path d="M5 12h14m-6-6 6 6-6 6" />,
    left: <path d="M19 12H5m6-6-6 6 6 6" />,
    lock: (
      <>
        <rect x="5.5" y="10" width="13" height="10.5" rx="2" />
        <path d="M8.5 10V7a3.5 3.5 0 0 1 7 0v3M12 14v3" />
      </>
    ),
    unlock: (
      <>
        <rect x="5.5" y="10" width="13" height="10.5" rx="2" />
        <path d="M8.5 10V7a3.5 3.5 0 0 1 6.8-1.1M12 14v3" />
      </>
    ),
    file: (
      <>
        <path d="M6 3h8l4 4v14H6z" />
        <path d="M14 3v5h4M9 12h6M9 16h6" />
      </>
    ),
    refresh: (
      <>
        <path d="M20 11a8 8 0 0 0-14-5L4 8" />
        <path d="M4 4v4h4M4 13a8 8 0 0 0 14 5l2-2" />
        <path d="M20 20v-4h-4" />
      </>
    ),
    check: <path d="m5 12 4 4L19 6" />,
    clock: (
      <>
        <circle cx="12" cy="12" r="9" />
        <path d="M12 7v5l3 2" />
      </>
    ),
  };
  return (
    <svg
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      {paths[name]}
    </svg>
  );
}

function readRecents(): Recent[] {
  try {
    const value: unknown = JSON.parse(
      localStorage.getItem("veil.recents") ?? "[]",
    );
    if (!Array.isArray(value)) return [];
    return value
      .filter(
        (item): item is Recent =>
          typeof item?.name === "string" && typeof item?.path === "string",
      )
      .slice(0, 6);
  } catch {
    return [];
  }
}

function formatSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024,
    index = 0;
  while (value >= 1024 && index < units.length - 1) {
    value /= 1024;
    index++;
  }
  return `${value.toFixed(1)} ${units[index]}`;
}

function formatDate(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime())
    ? value
    : new Intl.DateTimeFormat("zh-CN", {
        year: "numeric",
        month: "short",
        day: "numeric",
      }).format(date);
}

function App() {
  const [screen, setScreen] = useState<Screen>("home");
  const [recents, setRecents] = useState<Recent[]>(readRecents);
  const [container, setContainer] = useState<ContainerView | null>(null);
  const [path, setPath] = useState("");
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  function navigate(next: Screen, selectedPath = "") {
    setScreen(next);
    setPath(selectedPath);
    setName("");
    setPassword("");
    setConfirmation("");
    setError("");
  }

  function remember(view: ContainerView) {
    const next = [
      { name: view.name, path: view.path },
      ...recents.filter((item) => item.path !== view.path),
    ].slice(0, 6);
    setRecents(next);
    try {
      localStorage.setItem("veil.recents", JSON.stringify(next));
    } catch {
      /* 最近记录不可用时不影响容器操作 */
    }
  }

  async function chooseDirectory() {
    setError("");
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: screen === "create" ? "选择容器保存位置" : "选择容器目录",
      });
      if (typeof selected === "string") setPath(selected);
    } catch (reason) {
      setError(`无法选择目录：${String(reason)}`);
    }
  }

  async function submit(event: React.FormEvent) {
    event.preventDefault();
    setError("");
    if (!path) {
      setError(screen === "create" ? "请选择保存位置" : "请选择容器目录");
      return;
    }
    if (!password) {
      setError("请输入密码");
      return;
    }
    if (screen === "create" && !name.trim()) {
      setError("请输入容器名称");
      return;
    }
    if (screen === "create" && password !== confirmation) {
      setError("两次输入的密码不一致");
      return;
    }
    const secret = password;
    setPassword("");
    setConfirmation("");
    setBusy(true);
    try {
      const view =
        screen === "create"
          ? await invoke<ContainerView>("create_container", {
              parent: path,
              name: name.trim(),
              password: secret,
            })
          : await invoke<ContainerView>("open_container", {
              path,
              password: secret,
            });
      setContainer(view);
      remember(view);
      setScreen("container");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function lock() {
    setBusy(true);
    setError("");
    try {
      await invoke("lock_container");
      setContainer(null);
      navigate("home");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function refresh() {
    setBusy(true);
    setError("");
    try {
      setContainer(await invoke<ContainerView>("refresh_container"));
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  const total = container?.files.reduce((sum, file) => sum + file.size, 0) ?? 0;
  return (
    <div className="app-shell">
      <aside className="sidebar">
        <button
          className="brand"
          disabled={busy}
          onClick={() => {
            if (container) void lock();
            else navigate("home");
          }}
          aria-label="Veil 首页"
        >
          <span className="brand-mark">
            <Icon name="shield" size={22} />
          </span>
          <span>
            <strong>veil</strong>
            <small>你的私密空间</small>
          </span>
        </button>
        <div className="nav-caption">工作空间</div>
        <button
          className="nav-item active"
          disabled={busy}
          onClick={() => {
            if (container) void lock();
            else navigate("home");
          }}
        >
          <Icon name="grid" size={18} /> 我的容器
        </button>
        <button
          className="nav-item"
          disabled={busy || !!container}
          onClick={() => navigate("create")}
        >
          <Icon name="plus" size={19} /> 新建容器
        </button>
        <div className="sidebar-bottom">
          <span className="privacy-icon">
            <Icon name="lock" size={16} />
          </span>
          <div>
            <strong>本地保存</strong>
            <small>文件由你掌控</small>
          </div>
        </div>
      </aside>

      <main className="main">
        <header className="topbar">
          <span>
            工作空间 <span className="crumb">/</span>{" "}
            {screen === "container"
              ? container?.name
              : screen === "create"
                ? "新建容器"
                : screen === "open"
                  ? "打开容器"
                  : "我的容器"}
          </span>
          <span className="top-label">
            <span className="status-dot" /> 本地模式
          </span>
        </header>
        <div className="main-content">
          {screen === "home" && (
            <>
              <div className="page-heading">
                <div>
                  <div className="eyebrow">YOUR PRIVATE SPACE</div>
                  <h1>我的容器</h1>
                  <p>为重要文件留一个安心的地方。</p>
                </div>
                <button
                  className="button primary"
                  onClick={() => navigate("create")}
                >
                  <Icon name="plus" size={17} /> 新建容器
                </button>
              </div>
              <section className="welcome-card">
                <div className="welcome-copy">
                  <span className="pill">
                    <span className="status-dot" /> 一切都在你的掌控中
                  </span>
                  <h2>私密，从这里开始。</h2>
                  <p>
                    创建一个加密容器，或打开已有容器。
                    <br />
                    你的文件保存在你选择的位置。
                  </p>
                  <div className="button-row">
                    <button
                      className="button primary"
                      onClick={() => navigate("create")}
                    >
                      <Icon name="plus" size={17} /> 创建容器
                    </button>
                    <button
                      className="button secondary"
                      onClick={() => navigate("open")}
                    >
                      <Icon name="folder" size={17} /> 打开容器
                    </button>
                  </div>
                </div>
                <div className="hero-art" aria-hidden="true">
                  <div className="orbit orbit-outer" />
                  <div className="orbit orbit-inner" />
                  <div className="hero-vault">
                    <div className="vault-top">
                      <span />
                      <span />
                      <span />
                    </div>
                    <div className="vault-center">
                      <Icon name="shield" size={53} />
                    </div>
                    <div className="vault-lines">
                      <i />
                      <i />
                    </div>
                  </div>
                  <span className="art-dot dot-a" />
                  <span className="art-dot dot-b" />
                  <span className="art-square" />
                </div>
              </section>
              <section className="recent-section">
                <div className="section-heading">
                  <div>
                    <h3>最近的容器</h3>
                    <p>从上次停下的地方继续</p>
                  </div>
                  <button
                    className="text-button"
                    onClick={() => navigate("open")}
                  >
                    打开其他容器 <Icon name="arrow" size={15} />
                  </button>
                </div>
                {recents.length === 0 ? (
                  <div className="recent-empty">
                    <span className="empty-icon">
                      <Icon name="clock" size={20} />
                    </span>
                    <div>
                      <strong>还没有最近打开的容器</strong>
                      <p>创建或打开容器后，它会显示在这里。</p>
                    </div>
                  </div>
                ) : (
                  <div className="recent-grid">
                    {recents.map((item) => (
                      <button
                        className="recent-card"
                        key={item.path}
                        onClick={() => navigate("open", item.path)}
                      >
                        <span className="recent-icon">
                          <Icon name="shield" size={20} />
                        </span>
                        <span className="recent-info">
                          <strong>{item.name}</strong>
                          <small title={item.path}>{item.path}</small>
                        </span>
                        <Icon name="arrow" size={16} />
                      </button>
                    ))}
                  </div>
                )}
              </section>
            </>
          )}

          {(screen === "create" || screen === "open") && (
            <div className="form-wrap">
              <button
                className="back-button"
                onClick={() => navigate("home")}
                disabled={busy}
              >
                <Icon name="left" size={17} /> 返回容器
              </button>
              <div className="form-heading">
                <span className="form-icon">
                  <Icon
                    name={screen === "create" ? "plus" : "unlock"}
                    size={23}
                  />
                </span>
                <div className="eyebrow">
                  {screen === "create" ? "NEW CONTAINER" : "OPEN CONTAINER"}
                </div>
                <h1>{screen === "create" ? "创建新容器" : "打开容器"}</h1>
                <p>
                  {screen === "create"
                    ? "为你的文件创建一个私密空间。"
                    : "选择已有容器目录，输入密码解锁。"}
                </p>
              </div>
              <form className="form-card" onSubmit={submit}>
                {screen === "create" && (
                  <label>
                    容器名称
                    <input
                      value={name}
                      onChange={(event) => setName(event.target.value)}
                      placeholder="例如：个人文件"
                      maxLength={100}
                      autoComplete="off"
                      disabled={busy}
                    />
                    <small>为容器起一个便于识别的名称。</small>
                  </label>
                )}
                <label>
                  {screen === "create" ? "保存位置" : "容器目录"}
                  <span className="path-input">
                    <input
                      value={path}
                      onChange={(event) => setPath(event.target.value)}
                      placeholder={
                        screen === "create"
                          ? "选择容器的保存位置"
                          : "选择包含 .veil-info 的目录"
                      }
                      autoComplete="off"
                      disabled={busy}
                    />
                    <button
                      type="button"
                      onClick={chooseDirectory}
                      disabled={busy}
                    >
                      选择目录
                    </button>
                  </span>
                  {screen === "create" && (
                    <small>将在此位置创建一个独立的容器目录。</small>
                  )}
                </label>
                <label>
                  容器密码
                  <input
                    type="password"
                    value={password}
                    onChange={(event) => setPassword(event.target.value)}
                    placeholder={
                      screen === "create" ? "设置容器密码" : "输入容器密码"
                    }
                    autoComplete={
                      screen === "create" ? "new-password" : "current-password"
                    }
                    disabled={busy}
                  />
                </label>
                {screen === "create" && (
                  <label>
                    确认密码
                    <input
                      type="password"
                      value={confirmation}
                      onChange={(event) => setConfirmation(event.target.value)}
                      placeholder="再次输入容器密码"
                      autoComplete="new-password"
                      disabled={busy}
                    />
                  </label>
                )}
                {error && (
                  <div className="error" role="alert">
                    {error}
                  </div>
                )}
                <button
                  className="button primary submit-button"
                  disabled={busy}
                >
                  {busy
                    ? "正在处理…"
                    : screen === "create"
                      ? "创建容器"
                      : "解锁容器"}
                  <Icon name="arrow" size={17} />
                </button>
              </form>
              <div className="privacy-note">
                <Icon name="lock" size={15} />{" "}
                密码用于本地解锁容器，请妥善保管。
              </div>
            </div>
          )}

          {screen === "container" && container && (
            <>
              <div className="page-heading container-heading">
                <div>
                  <div className="eyebrow">UNLOCKED CONTAINER</div>
                  <h1>{container.name}</h1>
                  <p className="path-caption" title={container.path}>
                    {container.path}
                  </p>
                </div>
                <div className="button-row">
                  <button
                    className="button secondary"
                    onClick={refresh}
                    disabled={busy}
                  >
                    <Icon name="refresh" size={16} /> 刷新
                  </button>
                  <button
                    className="button secondary"
                    onClick={lock}
                    disabled={busy}
                  >
                    <Icon name="lock" size={16} /> 锁定
                  </button>
                </div>
              </div>
              {error && (
                <div className="error spaced" role="alert">
                  {error}
                </div>
              )}
              <div className="stats">
                <div className="stat-card">
                  <span className="stat-icon">
                    <Icon name="file" size={18} />
                  </span>
                  <span>
                    <small>文件数量</small>
                    <strong>
                      {container.files.length}
                      <em> 个文件</em>
                    </strong>
                  </span>
                </div>
                <div className="stat-card">
                  <span className="stat-icon">
                    <Icon name="folder" size={18} />
                  </span>
                  <span>
                    <small>文件总大小</small>
                    <strong>{formatSize(total)}</strong>
                  </span>
                </div>
                <div className="stat-card">
                  <span className="stat-icon">
                    <Icon name="clock" size={18} />
                  </span>
                  <span>
                    <small>创建时间</small>
                    <strong>{formatDate(container.createdAt)}</strong>
                  </span>
                </div>
              </div>
              <section className="files-section">
                <div className="section-heading">
                  <div>
                    <h3>容器文件</h3>
                    <p>查看当前容器中的文件</p>
                  </div>
                  <span className="count-label">
                    {container.files.length} 个文件
                  </span>
                </div>
                {container.files.length === 0 ? (
                  <div className="files-empty">
                    <span className="empty-folder">
                      <Icon name="folder" size={29} />
                    </span>
                    <strong>这里还是空的</strong>
                    <p>这个容器中暂时没有文件。</p>
                  </div>
                ) : (
                  <div className="file-table">
                    <div className="table-head">
                      <span>名称</span>
                      <span>大小</span>
                      <span>版本</span>
                      <span>加密时间</span>
                    </div>
                    {container.files.map((file) => (
                      <div className="file-row" key={file.id}>
                        <span className="file-name">
                          <span className="file-icon">
                            <Icon name="file" size={18} />
                          </span>
                          <span title={file.name}>{file.name}</span>
                        </span>
                        <span>{formatSize(file.size)}</span>
                        <span>v{file.version}</span>
                        <span>{formatDate(file.encryptedAt)}</span>
                      </div>
                    ))}
                  </div>
                )}
              </section>
              <div className="container-footer">
                <Icon name="check" size={14} /> 容器已解锁 <span>·</span> ID：
                {container.id}
              </div>
            </>
          )}
        </div>
        <footer className="footer">
          <span>
            VEIL <span className="footer-divider">/</span> PRIVATE BY DESIGN
          </span>
          <span>你的文件，你的空间</span>
        </footer>
      </main>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
