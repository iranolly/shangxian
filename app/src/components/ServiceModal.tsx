// 添加 / 编辑本机服务弹层（原来的 NewServiceWizard + EditServiceModal 合并成一个）。
// 只关心这台电脑上的东西：端口、启动程序/参数/工作目录、看护、开机自启、备注。
// 右侧给一份「本机正在监听的端口」，点一下就自动填好，填完还能手动改。

import { useEffect, useState } from "react";

import {
  listLocalPorts,
  saveService,
  type LocalPort,
  type ServiceView,
} from "../api";

interface Props {
  /** 传进来 = 编辑这一台；不传 = 新建 */
  service?: ServiceView | null;
  onClose: () => void;
  onSaved: () => void;
}

/** 进程名去掉 .exe，当作服务名字的建议 */
function nameFromProcess(process: string): string {
  return process.replace(/\.exe$/i, "");
}

export function ServiceModal({ service, onClose, onSaved }: Props) {
  const [name, setName] = useState(service?.name ?? "");
  const [nameEdited, setNameEdited] = useState(Boolean(service));
  const [localPort, setLocalPort] = useState(
    service ? String(service.localPort) : "",
  );
  const [launchExe, setLaunchExe] = useState(service?.launchExe ?? "");
  const [launchArgs, setLaunchArgs] = useState(service?.launchArgs ?? "");
  const [launchWorkdir, setLaunchWorkdir] = useState(
    service?.launchWorkdir ?? "",
  );
  const [watch, setWatch] = useState(service?.watch ?? false);
  const [autostart, setAutostart] = useState(service?.autostart ?? false);
  const [note, setNote] = useState(service?.note ?? "");
  const [ports, setPorts] = useState<LocalPort[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    listLocalPorts()
      .then(setPorts)
      .catch(() => setPorts([]));
  }, []);

  /** 点监听列表里的一行：端口 + 启动程序 + 名字建议一次填好 */
  const pick = (p: LocalPort) => {
    setLocalPort(String(p.port));
    if (p.exe) setLaunchExe(p.exe);
    if (!nameEdited && p.process) setName(nameFromProcess(p.process));
  };

  const save = async () => {
    setError("");
    const port = Number(localPort);
    if (!name.trim()) {
      setError("名字不能为空，填一个好认的（比如 pi-web）");
      return;
    }
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setError("本机端口要填 1~65535 的数字");
      return;
    }
    setBusy(true);
    try {
      await saveService({
        id: service?.id ?? null,
        name: name.trim(),
        localPort: port,
        launchExe: launchExe.trim() || null,
        launchArgs: launchArgs.trim() || null,
        launchWorkdir: launchWorkdir.trim() || null,
        watch,
        autostart,
        note: note.trim() || null,
      });
      onSaved();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" style={{ width: 620 }}>
        <div className="modal-head">
          <h2 className="modal-title">
            {service ? `编辑本机服务 · ${service.name}` : "添加本机服务"}
          </h2>
          <p className="modal-sub">
            本机服务就是这台电脑上的程序：填好端口和启动方式，才能帮你启动、看护。
          </p>
        </div>

        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}

          <div className="field-row">
            <div className="field">
              <label className="field-label">名字</label>
              <input
                className="input"
                placeholder="例如 pi-web"
                value={name}
                onChange={(e) => {
                  setName(e.target.value);
                  setNameEdited(true);
                }}
              />
              <p className="field-hint">这个名字只有你自己看得到。</p>
            </div>
            <div className="field">
              <label className="field-label">本机端口</label>
              <input
                className="input mono"
                inputMode="numeric"
                placeholder="例如 8080"
                value={localPort}
                onChange={(e) => setLocalPort(e.target.value)}
              />
              <p className="field-hint">就是这台程序的门牌号。</p>
            </div>
          </div>

          <div className="field">
            <label className="field-label">
              本机正在监听的端口（点一下就自动填好）
            </label>
            {ports === null ? (
              <p className="field-hint">正在读取…</p>
            ) : ports.length === 0 ? (
              <p className="field-hint">
                没读到正在监听的端口。先把程序跑起来，再回来点一下。
              </p>
            ) : (
              <div className="port-picker">
                {ports.map((p) => (
                  <div
                    key={p.port}
                    className="port-row"
                    onClick={() => pick(p)}
                  >
                    <span className="mono">{p.port}</span>
                    <span className="port-proc">
                      {p.process ? p.process : "读不到程序名"}
                    </span>
                    {p.exe && (
                      <span className="port-proc port-exe">{p.exe}</span>
                    )}
                  </div>
                ))}
              </div>
            )}
          </div>

          <div className="field">
            <label className="field-label">启动程序（可留空 = 只监控不启动）</label>
            <input
              className="input mono"
              placeholder="例如 C:\Python311\python.exe"
              value={launchExe}
              onChange={(e) => setLaunchExe(e.target.value)}
            />
            {!launchExe.trim() && (
              <p className="field-hint">
                留空的话我只能看状态：启动、看护、开机自启都用不上。
              </p>
            )}
          </div>

          <div className="field-row">
            <div className="field">
              <label className="field-label">启动参数（可留空）</label>
              <input
                className="input mono"
                placeholder="例如 -m pi_web"
                value={launchArgs}
                onChange={(e) => setLaunchArgs(e.target.value)}
              />
            </div>
            <div className="field">
              <label className="field-label">工作目录（可留空）</label>
              <input
                className="input mono"
                placeholder="例如 D:\code\pi-web"
                value={launchWorkdir}
                onChange={(e) => setLaunchWorkdir(e.target.value)}
              />
            </div>
          </div>

          <div className="field-row">
            <div className="field">
              <label className="field-label">看护</label>
              <label className="switch">
                <input
                  type="checkbox"
                  checked={watch}
                  onChange={(e) => setWatch(e.target.checked)}
                />
                程序掉了自动拉起来
              </label>
            </div>
            <div className="field">
              <label className="field-label">开机自启</label>
              <label className="switch">
                <input
                  type="checkbox"
                  checked={autostart}
                  onChange={(e) => setAutostart(e.target.checked)}
                />
                开机后自动帮我跑起来
              </label>
            </div>
          </div>

          <div className="field">
            <label className="field-label">备注（可留空）</label>
            <input
              className="input"
              placeholder="例如 平时最常用的控制台"
              value={note}
              onChange={(e) => setNote(e.target.value)}
            />
          </div>
        </div>

        <div className="modal-foot">
          <button className="btn btn-ghost" onClick={onClose}>
            取消
          </button>
          <span className="grow" />
          <button
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void save()}
          >
            {busy ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
