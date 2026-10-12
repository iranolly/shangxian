// 发布预设徽标（D4 §3.3 / D2 §6.2）：默认 web 不显示，保持安静。

import type { TunnelView } from "../../api";

interface Props {
  preset?: TunnelView["preset"];
}

export function PresetBadge({ preset }: Props) {
  if (preset === "ws") {
    return (
      <span
        className="chip preset-badge"
        title="这条隧道用了云端配置预设，改动后要重新贴一次云端配置才生效"
      >
        预设：WebSocket
      </span>
    );
  }
  if (preset === "upload") {
    return (
      <span
        className="chip preset-badge"
        title="这条隧道用了云端配置预设，改动后要重新贴一次云端配置才生效"
      >
        预设：大文件上传
      </span>
    );
  }
  // web 与未知值一样：不显示（D2 §1.4 兼容铁律）
  return null;
}
