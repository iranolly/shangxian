// 添加 / 编辑本机服务弹层（原来的 NewServiceWizard + EditServiceModal 合并成一个）。
// 只关心这台电脑上的东西：端口、启动程序/参数/工作目录、看护、开机自启、备注。
// 右侧给一份「本机正在监听的端口」，点一下就自动读取启动方式，填完还能手动改。
//
// 自动读取纪律：
// - 点端口行先填端口/名字（新建还会先把列表里的程序路径填上），再异步读启动方式（exe/args/workdir）；
// - 编辑模式不提前写 exe/args/workdir，三个字段统一在确认后由 applyLaunchInfo 写入，
//   取消 = 三个字段都保持原样；
// - 读到的信息不可靠（pid 对不上 / 多个占用者 / 进程没了 / 端口换人了）就一个字都不覆盖，
//   只在警告区说明原因；
// - 编辑模式字段已有内容时先确认再覆盖，可取消；
// - 参数里疑似有密钥（code=sensitiveArgs）时红字提醒，不勾选不让保存。

import { useEffect, useRef, useState } from "react";

import {
  inspectProcessLaunchInfo,
  listLocalPorts,
  previewLaunchCommand,
  type LaunchWarning,
  type LocalPort,
  type ServiceView,
} from "../api";
import { actions } from "../store/actions";
import { useStore } from "../store/useStore";
import { ConfirmModal } from "./ConfirmModal";
import { useModalA11y } from "./useModalA11y";

interface Props {
  /** 传进来 = 编辑这一台；不传 = 新建 */
  service?: ServiceView | null;
  /** 现有服务清单：用来判断端口是不是已经登记在别的服务名下 */
  services?: ServiceView[];
  /** Services 页「跳到已登记服务的编辑」时置 true：打开后自动读取补全一次 */
  autoRead?: boolean;
  onClose: () => void;
  /** 保存成功后回调；launchChanged = 编辑时改动了启动程序/参数/工作目录；serviceId = 保存后的服务 id */
  onSaved: (launchChanged: boolean, serviceId: string) => void;
  /** 点已登记端口行：切到那台服务的编辑界面（由 Services 页管状态） */
  onSwitchToEdit?: (serviceId: string) => void;
}

/** 这些警告说明「读到的信息不可靠」，不能拿它覆盖用户填的东西 */
const BLOCKING_WARNING_CODES = [
  "multipleOwners",
  "processGone",
  "portOwnerChanged",
];

type AutoField =
  | "name"
  | "localPort"
  | "launchExe"
  | "launchArgs"
  | "launchWorkdir";

type AutoFilled = Partial<Record<AutoField, boolean>>;

/** 进程名去掉 .exe，当作服务名字的建议 */
function nameFromProcess(process: string): string {
  return process.replace(/\.exe$/i, "");
}

/** 启动方式的三个字段统一按「去掉首尾空白、空串视作没填」比较 */
function normLaunch(value: string | null | undefined): string | null {
  const text = (value ?? "").trim();
  return text || null;
}

export function ServiceModal({
  service,
  services = [],
  autoRead = false,
  onClose,
  onSaved,
  onSwitchToEdit,
}: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  /** 疑似密钥警告区：点保存被拦时滚到它，用户能看见要勾的地方 */
  const warningRef = useRef<HTMLDivElement | null>(null);
  const configTrusted = useStore((s) => s.app.configTrusted);
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
  // 没填启动程序的只监控服务：看护 / 自启用不上，打开时就把开关归零
  const [watch, setWatch] = useState(
    service?.launchExe ? (service?.watch ?? false) : false,
  );
  const [autostart, setAutostart] = useState(
    service?.launchExe ? (service?.autostart ?? false) : false,
  );
  const [note, setNote] = useState(service?.note ?? "");
  const [ports, setPorts] = useState<LocalPort[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  /** 自动读取过程中后端给的提示（sensitiveArgs 要拦住保存） */
  const [warnings, setWarnings] = useState<LaunchWarning[]>([]);
  /** 用户确认「参数里有疑似密钥也照样保存」 */
  const [ackSensitive, setAckSensitive] = useState(false);
  /** 自动读取要覆盖已有启动字段时的确认框（替代 window.confirm） */
  const [overwriteAsk, setOverwriteAsk] = useState<{
    exe?: string | null;
    args?: string | null;
    workdir?: string | null;
  } | null>(null);
  /** 哪些字段是自动填的：显示「自动读取 · 未验证」，用户一编辑就消失 */
  const [autoFilled, setAutoFilled] = useState<AutoFilled>({});
  const [inspecting, setInspecting] = useState(false);
  /** 实际会执行的命令行预览（失败就空着不显示） */
  const [preview, setPreview] = useState("");
  /** 最近一次读到的原始命令行；参数没被采纳时只读展示，兑现警告里的「可以在下面查看」 */
  const [rawInfo, setRawInfo] = useState<{
    text: string;
    argsDropped: boolean;
  } | null>(null);
  const autoReadDone = useRef(false);
  /** 连点多个端口行时，只认最后一次读取的结果（防旧响应晚到乱覆盖） */
  const inspectSeq = useRef(0);
  /**
   * 三个启动字段的最新值。自动读取是异步的，确认基准必须用 ref 里的最新值，
   * 否则闭包里是点击那一刻的旧值：会误弹确认，取消后还会留下被提前替换的字段。
   */
  const launchExeRef = useRef(service?.launchExe ?? "");
  const launchArgsRef = useRef(service?.launchArgs ?? "");
  const launchWorkdirRef = useRef(service?.launchWorkdir ?? "");

  // ref 兜底跟随 state；onChange / applyLaunchInfo 里也会同步写 ref
  useEffect(() => {
    launchExeRef.current = launchExe;
    launchArgsRef.current = launchArgs;
    launchWorkdirRef.current = launchWorkdir;
  }, [launchExe, launchArgs, launchWorkdir]);

  useEffect(() => {
    listLocalPorts()
      .then(setPorts)
      .catch(() => setPorts([]));
  }, []);

  // 字段被自动填过 → 打上「自动读取 · 未验证」标记
  const mark = (...fields: AutoField[]) => {
    setAutoFilled((f) => {
      const next = { ...f };
      for (const field of fields) next[field] = true;
      return next;
    });
  };

  // 用户一编辑，这个字段的自动标记就消失
  const unmark = (field: AutoField) => {
    setAutoFilled((f) => (f[field] ? { ...f, [field]: false } : f));
  };

  /** 把读到的启动方式真正填进表单（确认覆盖后 / 没有旧内容时走这里）。 */
  const fillLaunchInfo = (
    exe?: string | null,
    args?: string | null,
    workdir?: string | null,
  ) => {
    const filled: AutoField[] = [];
    if (exe != null) {
      launchExeRef.current = exe;
      setLaunchExe(exe);
      filled.push("launchExe");
    }
    if (args != null) {
      launchArgsRef.current = args;
      setLaunchArgs(args);
      filled.push("launchArgs");
    }
    if (workdir != null) {
      launchWorkdirRef.current = workdir;
      setLaunchWorkdir(workdir);
      filled.push("launchWorkdir");
    }
    if (filled.length) mark(...filled);
  };

  /**
   * 把读到的启动方式填进表单。
   * - 新建：直接填；
   * - 编辑：三个字段里已有内容且新值不同才确认（用 ConfirmModal，保留原文案）；
   *   取消 = 三个字段都保持原样（pick/自动读取都不再提前写字段，确认通过后才统一写入）。
   */
  const applyLaunchInfo = (
    exe?: string | null,
    args?: string | null,
    workdir?: string | null,
  ) => {
    const changed =
      (exe != null && exe !== launchExeRef.current) ||
      (args != null && args !== launchArgsRef.current) ||
      (workdir != null && workdir !== launchWorkdirRef.current);
    const hasExisting = Boolean(
      launchExeRef.current.trim() ||
        launchArgsRef.current.trim() ||
        launchWorkdirRef.current.trim(),
    );
    if (service && hasExisting && changed) {
      setOverwriteAsk({ exe, args, workdir });
      return;
    }
    fillLaunchInfo(exe, args, workdir);
  };

  const confirmOverwrite = () => {
    const pending = overwriteAsk;
    setOverwriteAsk(null);
    if (!pending) return;
    fillLaunchInfo(pending.exe, pending.args, pending.workdir);
  };

  /** 读某个端口的启动方式；读不可靠就只报警告、不覆盖字段 */
  const inspect = async (port: number, expectedPid: number | null) => {
    const seq = ++inspectSeq.current;
    setInspecting(true);
    setWarnings([]);
    setAckSensitive(false);
    setRawInfo(null);
    // 新读一次就把上一轮还没点的「覆盖确认」撤掉，避免对着旧数据确认
    setOverwriteAsk(null);
    try {
      const info = await inspectProcessLaunchInfo(port, expectedPid);
      if (seq !== inspectSeq.current) return;
      const list = info.warnings ?? [];
      setWarnings(list);
      // 参数被丢弃（controlChars/tooLong/wrapper 等）时，把原文只读展示出来
      const raw = info.rawCommandLine?.trim();
      setRawInfo(raw ? { text: raw, argsDropped: info.args == null } : null);
      const unreliable =
        !info.pidMatched ||
        list.some((w) => BLOCKING_WARNING_CODES.includes(w.code));
      if (unreliable) return;
      applyLaunchInfo(info.exe, info.args, info.workdir);
    } catch (e) {
      if (seq !== inspectSeq.current) return;
      setWarnings([
        { code: "readFailed", text: `无法自动读取启动方式：${String(e)}` },
      ]);
    } finally {
      if (seq === inspectSeq.current) setInspecting(false);
    }
  };

  /** 点监听列表里的一行：先填端口/名字，再异步补启动方式（编辑模式不提前写 exe/args/workdir） */
  const pick = (p: LocalPort) => {
    // 这个端口已经登记在别的服务名下 → 直接切到那台服务的编辑界面
    const owner = services.find(
      (s) => s.localPort === p.port && s.id !== service?.id,
    );
    if (owner && onSwitchToEdit) {
      onSwitchToEdit(owner.id);
      return;
    }
    setLocalPort(String(p.port));
    mark("localPort");
    // 新建直接填；编辑不提前写启动程序，统一等 applyLaunchInfo 确认后再写，
    // 这样用户点取消时 exe/args/workdir 三个字段都保持原样。
    if (!service && p.exe) {
      launchExeRef.current = p.exe;
      setLaunchExe(p.exe);
      mark("launchExe");
    }
    if (!nameEdited && p.process) {
      setName(nameFromProcess(p.process));
      mark("name");
    }
    void inspect(p.port, p.pid ?? null);
  };

  /** 从「已登记端口」跳过来：打开后自动读取补全一次 */
  useEffect(() => {
    if (!autoRead || autoReadDone.current || !service) return;
    if (ports === null) return;
    autoReadDone.current = true;
    const row = ports.find((p) => p.port === service.localPort);
    if (row) {
      // 编辑模式同样不提前写启动程序：交给 inspect → applyLaunchInfo 确认后统一写入
      void inspect(row.port, row.pid ?? null);
    } else {
      // 端口现在没在监听：照样问一次，让警告区说清为什么读不到
      void inspect(service.localPort, null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoRead, ports, service]);

  // 启动程序 / 参数一变，就刷新「实际会执行的命令行」预览（失败静默）
  useEffect(() => {
    const exe = launchExe.trim();
    if (!exe) {
      setPreview("");
      return;
    }
    let alive = true;
    const timer = window.setTimeout(() => {
      previewLaunchCommand(exe, launchArgs)
        .then((text) => {
          if (alive) setPreview(text.trim());
        })
        .catch(() => {
          if (alive) setPreview("");
        });
    }, 300);
    return () => {
      alive = false;
      window.clearTimeout(timer);
    };
  }, [launchExe, launchArgs]);

  const sensitive = warnings.some((w) => w.code === "sensitiveArgs");
  const noLaunch = !launchExe.trim();

  const save = async () => {
    setError("");
    // 参数疑似含密钥且没勾确认：点保存给出提示并滚到勾选框，不做静默拦
    if (sensitive && !ackSensitive) {
      setError("启动参数疑似包含密钥：请先勾选上方的「确认仍然保存」，再点击保存。");
      warningRef.current?.scrollIntoView({ behavior: "smooth", block: "center" });
      return;
    }
    const port = Number(localPort);
    if (!name.trim()) {
      setError("名称不能为空，请填写易于识别的名称（例如 pi-web）");
      return;
    }
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      setError("本机端口需填写 1~65535 的数字");
      return;
    }
    // 编辑时改动启动方式：保存后旧进程照旧跑，界面提示「下次重启才生效」
    const launchChanged =
      !!service &&
      (normLaunch(launchExe) !== normLaunch(service.launchExe) ||
        normLaunch(launchArgs) !== normLaunch(service.launchArgs) ||
        normLaunch(launchWorkdir) !== normLaunch(service.launchWorkdir));
    setBusy(true);
    try {
      const savedId = await actions.serviceSave({
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
      if (!savedId) {
        setError("保存失败：请稍后重试");
        return;
      }
      onSaved(launchChanged, savedId);
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <div className="modal-backdrop">
      <div className="modal" role="dialog" aria-modal="true" ref={dialogRef} style={{ width: 620 }}>
        <div className="modal-head">
          <h2 className="modal-title">
            {service ? `编辑本机服务 · ${service.name}` : "添加本机服务"}
          </h2>
          <p
            className="modal-sub"
            title="本机服务即这台电脑上的程序：填写端口和启动方式后，才能启动和看护。"
          >
            这台电脑上的程序，填写端口和启动方式。
          </p>
        </div>

        <div className="modal-body">
          {error && <div className="error-line">{error}</div>}

          <div className="field-row">
            <div className="field">
              <label className="field-label">
                名称
                {autoFilled.name && (
                  <span className="auto-tag">自动读取 · 未验证</span>
                )}
              </label>
              <input
                className="input"
                placeholder="例如 pi-web"
                value={name}
                onChange={(e) => {
                  setName(e.target.value);
                  setNameEdited(true);
                  unmark("name");
                }}
              />
              <p className="field-hint hint">该名称仅本机可见。</p>
            </div>
            <div className="field">
              <label className="field-label">
                本机端口
                {autoFilled.localPort && (
                  <span className="auto-tag">自动读取 · 未验证</span>
                )}
              </label>
              <input
                className="input mono"
                inputMode="numeric"
                placeholder="例如 8080"
                value={localPort}
                onChange={(e) => {
                  setLocalPort(e.target.value);
                  unmark("localPort");
                }}
              />
              <p className="field-hint hint">即该程序监听的本机端口。</p>
            </div>
          </div>

          <div className="field">
            <label className="field-label">
              本机正在监听的端口（点击即可自动填写）
            </label>
            {!service && (
              <p
                className="field-hint hint"
                title="端口已登记在其他服务名下时会标出「已登记」，点击可直接跳转到该服务的编辑界面，不会重复登记。"
              >
                点击自动填写；已登记的会跳转到对应服务。
              </p>
            )}
            {ports === null ? (
              <p className="field-hint hint">读取中…</p>
            ) : ports.length === 0 ? (
              <p className="field-hint hint">
                未读取到端口；请先启动程序后再返回点击。
              </p>
            ) : (
              <div className="port-picker">
                {ports.map((p) => {
                  const owner = services.find(
                    (s) => s.localPort === p.port && s.id !== service?.id,
                  );
                  return (
                    <div
                      key={p.port}
                      className={`port-row${owner ? " registered" : ""}`}
                      onClick={() => pick(p)}
                      title={
                        owner
                          ? `该端口已登记在「${owner.name}」名下，点击可编辑`
                          : "点击自动填写端口和启动方式"
                      }
                    >
                      <span className="mono">{p.port}</span>
                      <span className="port-proc">
                        {p.process ? p.process : "无法读取程序名"}
                      </span>
                      {owner && (
                        <span className="port-owner">已登记：{owner.name}</span>
                      )}
                      {p.exe && (
                        <span className="port-proc port-exe">{p.exe}</span>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
            {inspecting && (
              <p className="field-hint hint">读取该程序的启动方式中…</p>
            )}
          </div>

          {warnings.length > 0 && (
            <div
              ref={warningRef}
              className={`launch-warnings${sensitive ? " sensitive" : ""}`}
            >
              {warnings.map((w, i) => (
                <p
                  key={`${w.code}-${i}`}
                  className={w.code === "sensitiveArgs" ? "warn-sensitive" : ""}
                >
                  {w.code === "sensitiveArgs" ? "⚠ " : ""}
                  {w.text}
                </p>
              ))}
              {sensitive && (
                <label
                  className="switch warn-check"
                  title="参数疑似包含密钥，确认仍然保存"
                >
                  <input
                    type="checkbox"
                    checked={ackSensitive}
                    onChange={(e) => setAckSensitive(e.target.checked)}
                  />
                  确认仍然保存
                </label>
              )}
              {rawInfo?.argsDropped && (
                <div className="raw-cmd">
                  <p className="raw-cmd-label">
                    原始命令行（只读，参数未填写）：
                  </p>
                  <p className="raw-cmd-text mono" title={rawInfo.text}>
                    {rawInfo.text}
                  </p>
                </div>
              )}
            </div>
          )}

          <div className="field">
            <label className="field-label">
              启动程序（可留空 = 仅监控，不启动）
              {autoFilled.launchExe && (
                <span className="auto-tag">自动读取 · 未验证</span>
              )}
            </label>
            <input
              className="input mono"
              placeholder="例如 C:\Python311\python.exe"
              value={launchExe}
              onChange={(e) => {
                launchExeRef.current = e.target.value;
                setLaunchExe(e.target.value);
                unmark("launchExe");
                // 启动程序被清空：看护 / 自启跟着关掉，别存出一个用不上的组合
                if (!e.target.value.trim()) {
                  setWatch(false);
                  setAutostart(false);
                }
              }}
            />
            {!launchExe.trim() && (
              <p className="field-hint hint">
                留空则只能查看状态，启动、看护、开机自启均无法使用。
              </p>
            )}
          </div>

          <div className="field-row">
            <div className="field">
              <label className="field-label">
                启动参数（可留空）
                {autoFilled.launchArgs && (
                  <span className="auto-tag">自动读取 · 未验证</span>
                )}
              </label>
              <textarea
                className="input mono"
                rows={2}
                placeholder="例如 -m pi_web"
                value={launchArgs}
                onChange={(e) => {
                  launchArgsRef.current = e.target.value;
                  setLaunchArgs(e.target.value);
                  unmark("launchArgs");
                }}
              />
              {preview && (
                <p className="field-hint mono preview-line hint">
                  实际会执行的命令行：{preview}
                </p>
              )}
            </div>
            <div className="field">
              <label className="field-label">
                工作目录（可留空）
                {autoFilled.launchWorkdir && (
                  <span className="auto-tag">自动读取 · 未验证</span>
                )}
              </label>
              <input
                className="input mono"
                placeholder="例如 D:\code\pi-web"
                value={launchWorkdir}
                onChange={(e) => {
                  launchWorkdirRef.current = e.target.value;
                  setLaunchWorkdir(e.target.value);
                  unmark("launchWorkdir");
                }}
              />
              <p className="field-hint hint">
                程序启动时使用的工作目录（读取到的是当前工作目录）。
              </p>
            </div>
          </div>

          <div className="field-row">
            <div className="field">
              <label className="field-label">看护</label>
              <label
                className="switch"
                title={noLaunch ? "未填写启动程序，无法使用" : undefined}
              >
                <input
                  type="checkbox"
                  checked={watch}
                  disabled={noLaunch}
                  onChange={(e) => setWatch(e.target.checked)}
                />
                程序停止后自动重新启动
              </label>
            </div>
            <div className="field">
              <label className="field-label">开机自启</label>
              <label
                className="switch"
                title={noLaunch ? "未填写启动程序，无法使用" : undefined}
              >
                <input
                  type="checkbox"
                  checked={autostart}
                  disabled={noLaunch}
                  onChange={(e) => setAutostart(e.target.checked)}
                />
                应用启动后自动启动该程序
              </label>
              <p
                className="field-hint hint"
                title="两项都开启后，开机时才会自动启动该服务。"
              >
                设置中也需开启「开机自动启动」。
              </p>
            </div>
          </div>
          {noLaunch && (
            <p className="hint-danger">
              未填写启动程序：看护和开机自启无法使用。
            </p>
          )}

          <div className="field">
            <label className="field-label">备注（可留空）</label>
            <input
              className="input"
              placeholder="例如 常用控制台"
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
          {error ? (
            <span className="error-line foot-error" title={error}>
              {error}
            </span>
          ) : !configTrusted ? (
            <span
              className="error-line foot-error"
              title="配置文件无法读取，请先修复后再修改设置"
            >
              配置文件无法读取，请先修复后再修改设置
            </span>
          ) : null}
          <button
            className="btn btn-primary"
            disabled={busy || !configTrusted}
            title={!configTrusted ? "配置文件无法读取，请先修复后再修改设置" : undefined}
            onClick={() => void save()}
          >
            {busy ? "保存中…" : "保存"}
          </button>
        </div>
      </div>
      </div>

      {overwriteAsk ? (
        <ConfirmModal
          title="启动程序 / 启动参数 / 工作目录已有内容。"
          body="是否用读取到的内容覆盖？取消将保留已填写的内容。"
          confirmLabel="覆盖"
          onConfirm={confirmOverwrite}
          onClose={() => setOverwriteAsk(null)}
        />
      ) : null}
    </>
  );
}
