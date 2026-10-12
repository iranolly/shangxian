// 批量操作栏（D4 §3.3 BulkBar）：固定底部居中，并发 3 逐条处理；失败进结果面板。
// 位次口径：所有写操作仍走 actions.*（runCommand 的 pending / 乐观 / 就地错误都在里面）。

import { useRef, useState } from "react";

import { T } from "../../copy/toasts";
import { actions, type BulkAction, type BulkOutcome, type BulkOutcomeFailure } from "../../store/actions";
import { getState } from "../../store/snapshotStore";
import { toast } from "../../store/toastStore";
import { BulkConfirmModal } from "./BulkConfirmModal";
import { BulkResultPanel } from "./BulkResultPanel";
import {
  clearSelection,
  removeFromSelection,
  selectAll,
  useBulkSelection,
  type ListPage,
} from "./listPrefs";

interface Props {
  page: ListPage;
  /** 当前筛选后的全部 id（全选目标） */
  visibleIds: string[];
  onExit(): void;
}

interface ResultState {
  ok: number;
  failures: BulkOutcomeFailure[];
  tokens: { token: string; name: string }[];
}

interface ProgressState {
  done: number;
  total: number;
  /** 被当前筛选藏起来、这次不处理的已选项数 */
  hidden: number;
}

export function BulkBar({ page, visibleIds, onExit }: Props) {
  const selected = useBulkSelection(page);
  const [progress, setProgress] = useState<ProgressState | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [result, setResult] = useState<ResultState | null>(null);
  const [lastAction, setLastAction] = useState<BulkAction>("start");
  const stopRef = useRef(false);

  const nameOf = (id: string) =>
    page === "tunnels"
      ? getState().byTunnelId[id]?.name ?? id
      : getState().byServiceId[id]?.name ?? id;

  const run = async (action: BulkAction, ids: string[], hidden = 0) => {
    if (ids.length === 0) return;
    setConfirmOpen(false);
    setResult(null);
    stopRef.current = false;
    setLastAction(action);
    setProgress({ done: 0, total: ids.length, hidden });
    const hiddenNote = hidden > 0 ? `另有 ${hidden} 条被筛选隐藏，这次没动` : "";

    const opts = {
      onProgress: (done: number, total: number) => setProgress({ done, total, hidden }),
      isStopped: () => stopRef.current,
    };
    const outcome: BulkOutcome =
      page === "tunnels"
        ? await actions.bulkTunnel(action, ids, opts)
        : await actions.bulkService(action, ids, opts);

    setProgress(null);
    removeFromSelection(page, outcome.succeeded);

    const total = ids.length;
    const fail = outcome.failures.length;
    const remaining = total - outcome.succeeded.length - fail;
    if (outcome.stopped && remaining > 0) {
      toast.info(`已停止，剩下 ${remaining} 条没处理${hiddenNote ? `；${hiddenNote}` : ""}`);
    }

    if (fail === 0) {
      if (action === "remove") {
        const tokens = outcome.tokens;
        const removed = outcome.succeeded.length;
        const detail = [
          hiddenNote,
          removed > 20 ? "超过 20 条的部分不能撤销" : "",
        ]
          .filter(Boolean)
          .join("；");
        toast.success(T.bulkRemoved(removed), {
          durationMs: 5000,
          detail: detail || undefined,
          key: `bulk-remove:${page}:${Date.now()}`,
          action:
            tokens.length > 0
              ? { label: "全部撤销", run: () => void actions.undoMany(tokens) }
              : undefined,
        });
      } else if (outcome.succeeded.length > 0) {
        toast.success(T.bulkDone(outcome.succeeded.length), {
          detail: hiddenNote || undefined,
        });
      }
      if (!outcome.stopped) onExit();
      return;
    }

    const next: ResultState = {
      ok: outcome.succeeded.length,
      failures: outcome.failures,
      tokens: outcome.tokens,
    };
    setResult(next);
    toast.warn(T.bulkPartial(total, fail), {
      key: `bulk-partial:${page}`,
      detail: hiddenNote || undefined,
      action: { label: "看详情", run: () => setResult(next) },
    });
  };

  const allSelected = visibleIds.length > 0 && visibleIds.every((id) => selected.has(id));
  // 只认当前可见的勾选项：筛选把已选项藏起来时，任何批量动作都不动它们（R-B 高-1）
  const ids = visibleIds.filter((id) => selected.has(id));
  const hiddenSelected = selected.size - ids.length;
  const pct = progress ? Math.round((progress.done / Math.max(1, progress.total)) * 100) : 0;

  return (
    <>
      <div className="bulk-bar" role="region" aria-label="批量操作">
        {progress ? (
          <>
            <span
              className="bulk-count"
              title={
                progress.hidden > 0
                  ? `另有 ${progress.hidden} 条被筛选隐藏，这次不动`
                  : undefined
              }
            >
              正在处理 {progress.done}/{progress.total}…
              {progress.hidden > 0 ? `（隐藏 ${progress.hidden} 条）` : ""}
            </span>
            <span className="bulk-progress" aria-hidden="true">
              <span className="bulk-progress-fill" style={{ width: `${pct}%` }} />
            </span>
            <button
              className="btn btn-ghost btn-sm"
              disabled={stopRef.current}
              onClick={() => {
                stopRef.current = true;
              }}
            >
              停止
            </button>
          </>
        ) : (
          <>
            <span
              className="bulk-count"
              title={
                hiddenSelected > 0
                  ? `另有 ${hiddenSelected} 条被筛选隐藏，不参与操作`
                  : undefined
              }
            >
              已选 {ids.length} 条
              {hiddenSelected > 0 ? `（隐藏 ${hiddenSelected} 条）` : ""}
            </span>
            <button
              className="btn btn-ghost btn-sm"
              aria-label="全选当前筛选结果"
              disabled={visibleIds.length === 0}
              onClick={() => {
                if (allSelected) clearSelection(page);
                else selectAll(page, visibleIds);
              }}
            >
              {allSelected ? "取消全选" : `全选这 ${visibleIds.length} 条`}
            </button>
            <button
              className="btn btn-primary btn-sm"
              disabled={ids.length === 0}
              onClick={() => void run("start", ids, hiddenSelected)}
            >
              {page === "tunnels" ? "开始发布" : "启动"}
            </button>
            <button
              className="btn btn-ghost btn-sm"
              disabled={ids.length === 0}
              onClick={() => void run("pause", ids, hiddenSelected)}
            >
              {page === "tunnels" ? "暂停发布" : "停止"}
            </button>
            <button
              className="btn btn-ghost btn-sm btn-danger-text"
              disabled={ids.length === 0}
              onClick={() => setConfirmOpen(true)}
            >
              删除
            </button>
            <button className="link-btn" onClick={onExit}>
              取消
            </button>
          </>
        )}
      </div>

      {confirmOpen ? (
        <BulkConfirmModal
          action="remove"
          unit={page === "tunnels" ? "隧道" : "本机服务"}
          names={ids.map(nameOf)}
          onConfirm={() => void run("remove", ids, hiddenSelected)}
          onClose={() => setConfirmOpen(false)}
        />
      ) : null}

      {result ? (
        <BulkResultPanel
          result={result}
          onClose={() => setResult(null)}
          onRetry={(retryIds) => void run(lastAction, retryIds)}
          onUndoDeleted={
            result.tokens.length > 0
              ? () => {
                  void actions.undoMany(result.tokens);
                  setResult(null);
                }
              : undefined
          }
        />
      ) : null}
    </>
  );
}
