// 状态 → 中文状态词 + 一句人话（对齐 DESIGN.md §4.5）
// 铁律：任何地方都不许只有颜色；出错必须带具体原因。

/** 本机服务的状态（这台电脑上的东西） */
export type ServiceStatus =
  | "stopped"
  | "starting"
  | "running"
  | "not_listening"
  | "failed";

/** 发布隧道的状态（本机服务 → 云端服务器） */
export type TunnelStatus =
  | "running"
  | "connecting"
  | "disconnected"
  | "error"
  | "paused";

export interface StatusMeta {
  /** 状态词（圆点旁的中文） */
  word: string;
  /** 一句人话（列表第二行 / 详情页） */
  sentence: string;
  /** 卡片上的主动作按钮文案；无动作则不显示 */
  action?: string;
  /** 主动作是否是"坏状态修复"（用主按钮样式） */
  actionPrimary?: boolean;
}

export const SERVICE_STATUS_META: Record<ServiceStatus, StatusMeta> = {
  stopped: {
    word: "未启动",
    sentence: "程序未在运行。填写启动程序后，点击「启动」即可。",
  },
  starting: {
    word: "启动中",
    sentence: "程序已启动，正在检查端口，请稍候。",
  },
  running: {
    word: "运行中",
    sentence: "程序正在运行，端口正常。",
  },
  not_listening: {
    word: "端口未监听",
    sentence: "程序进程仍在，但未监听该端口：请更换端口，或查看程序日志。",
  },
  failed: {
    word: "启动失败",
    sentence: "程序启动失败，已停止自动重试。请检查启动程序、参数、工作目录，或查看日志。",
  },
};

export const TUNNEL_STATUS_META: Record<TunnelStatus, StatusMeta> = {
  running: {
    word: "发布中",
    sentence: "隧道已连通；云端配置和域名解析正确后，即可对外访问。",
    action: "暂停",
  },
  connecting: {
    word: "连接中",
    sentence: "正在连接云端服务器；如长时间停留在此状态，通常是网络不通，请查看日志了解原因。",
  },
  disconnected: {
    word: "重连中",
    sentence: "无法连接云端服务器，可能网络波动，将自动重试。",
  },
  error: {
    word: "发布失败",
    sentence: "请查看日志了解原因。",
  },
  paused: {
    word: "已暂停",
    sentence: "发布已暂停，当前无法访问。",
    action: "开始发布",
  },
};

/** 体检（doctor）每一步的结果。D0 裁决在三值上追加 warn，状态词 + 图标都不许只靠颜色。 */
export type DoctorStatus = "pass" | "warn" | "fail" | "skip";

export interface DoctorStatusMeta {
  /** 状态词（行内和图标一起显示） */
  word: string;
  /** 图标（配合状态词，不只靠颜色） */
  icon: string;
  /** 颜色语义 */
  tone: "ok" | "warn" | "error" | "muted";
}

export const DOCTOR_STATUS_META: Record<DoctorStatus, DoctorStatusMeta> = {
  pass: { word: "通过", icon: "✓", tone: "ok" },
  warn: { word: "注意", icon: "!", tone: "warn" },
  fail: { word: "未通过", icon: "✕", tone: "error" },
  skip: { word: "跳过", icon: "–", tone: "muted" },
};

/** 云端环境识别（D3 §3）：面板 / 网站服务的固定中文说法。 */
export type ServerPanelKind = "bt" | "xp" | "none" | "unknown";
export type ServerWebKind = "nginx" | "apache" | "both" | "none" | "unknown";

/** 隧道排序：坏状态排前面（出错 > 掉线 > 连接中 > 正在发布 > 已暂停） */
export const TUNNEL_ORDER: Record<TunnelStatus, number> = {
  error: 0,
  disconnected: 1,
  connecting: 2,
  running: 3,
  paused: 4,
};
