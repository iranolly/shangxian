// 隧道卡片的「⋯」菜单（D4 §3.3 CardMenu）。
// 只读入口（编辑 / 日志 / 云端配置 / 访问保护）在写操作进行中也可用；
// 重连 / 删除属于写操作，pending 时禁用。

import type { TunnelView } from "../../api";
import { T } from "../../copy/toasts";
import { actions } from "../../store/actions";
import { MenuButton } from "./Menu";

interface Props {
  tunnel: TunnelView;
  pending: boolean;
  onEdit(id: string): void;
  onNginx(id: string): void;
  onLogs(id: string): void;
  onClone(id: string): void;
  onOpenProtection(id: string): void;
  onRemove(id: string): void;
}

export function CardMenu({
  tunnel,
  pending,
  onEdit,
  onNginx,
  onLogs,
  onClone,
  onOpenProtection,
  onRemove,
}: Props) {
  const id = tunnel.id;
  const url = tunnel.url ?? "";
  return (
    <MenuButton
      items={[
        {
          label: "立刻重连",
          run: () => actions.tunnelRestart(id),
          disabled: pending,
        },
        { label: "编辑隧道", run: () => onEdit(id) },
        { label: "克隆这条隧道…", run: () => onClone(id) },
        {
          label: "复制公网网址",
          run: () => void actions.copyText(url, T.urlCopied(url)),
          disabled: !url,
          title: url ? undefined : "还没填公网网址",
        },
        { label: "云端配置", run: () => onNginx(id) },
        { label: "看发布日志", run: () => onLogs(id) },
        { label: "访问保护", run: () => onOpenProtection(id) },
        {
          label: "删除隧道",
          run: () => onRemove(id),
          danger: true,
          dividerBefore: true,
          disabled: pending,
        },
      ]}
    />
  );
}
