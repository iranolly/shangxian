// 设置页：外观 / 开机自启 / 配置文件夹。

import { useEffect, useState } from "react";

import {
  getAutostart,
  getDataDir,
  getNotifyEnabled,
  openDataDir,
  setAutostart,
  setNotifyEnabled,
  testNotification,
} from "../api";

type Theme = "system" | "light" | "dark";

export function SettingsPage() {
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem("theme") as Theme) || "system",
  );
  const [autostart, setAutostartState] = useState(false);
  const [notify, setNotifyState] = useState(true);
  const [dataDir, setDataDir] = useState("");
  const [error, setError] = useState("");

  useEffect(() => {
    if (theme === "system") {
      delete document.documentElement.dataset.theme;
    } else {
      document.documentElement.dataset.theme = theme;
    }
    localStorage.setItem("theme", theme);
  }, [theme]);

  useEffect(() => {
    getAutostart()
      .then(setAutostartState)
      .catch(() => {});
    getNotifyEnabled()
      .then(setNotifyState)
      .catch(() => {});
    getDataDir()
      .then(setDataDir)
      .catch(() => {});
  }, []);

  const toggleAutostart = async (next: boolean) => {
    setError("");
    try {
      await setAutostart(next);
      setAutostartState(next);
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleNotify = async (next: boolean) => {
    setError("");
    try {
      await setNotifyEnabled(next);
      setNotifyState(next);
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="page">
      <header className="page-head">
        <h1 className="page-title">设置</h1>
      </header>
      <div className="service-list">
        <article className="card">
          <div className="field" style={{ marginBottom: 0 }}>
            <label className="field-label">外观</label>
            <div className="option-list" style={{ flexDirection: "row", gap: 8 }}>
              {(["system", "light", "dark"] as Theme[]).map((t) => (
                <div
                  key={t}
                  className={`option ${theme === t ? "selected" : ""}`}
                  style={{ flex: 1, justifyContent: "center" }}
                  onClick={() => setTheme(t)}
                >
                  {t === "system" ? "跟随系统" : t === "light" ? "浅色" : "深色"}
                </div>
              ))}
            </div>
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">开机自动启动</div>
              <div className="server-meta">
                登录电脑后自动在托盘里运行，不弹窗口。
              </div>
            </div>
            <button
              className={
                autostart ? "btn btn-ghost btn-sm" : "btn btn-primary btn-sm"
              }
              onClick={() => void toggleAutostart(!autostart)}
            >
              {autostart ? "取消" : "开启"}
            </button>
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">发布掉线提醒</div>
              <div className="server-meta">
                发布掉线超过 3 分钟、或发布恢复时，弹一条系统通知。
              </div>
            </div>
            <span className="row-actions">
              {notify && (
                <button
                  className="link-btn"
                  onClick={() => void testNotification()}
                >
                  试一下
                </button>
              )}
              <button
                className={
                  notify ? "btn btn-ghost btn-sm" : "btn btn-primary btn-sm"
                }
                onClick={() => void toggleNotify(!notify)}
              >
                {notify ? "取消" : "开启"}
              </button>
            </span>
          </div>
        </article>

        <article className="card">
          <div className="server-card">
            <div className="server-main">
              <div className="server-name">配置文件夹</div>
              <div className="server-meta mono">{dataDir}</div>
            </div>
            <button
              className="btn btn-ghost btn-sm"
              onClick={() => void openDataDir()}
            >
              打开
            </button>
          </div>
        </article>

        {error && <div className="error-line">{error}</div>}
      </div>
    </div>
  );
}
