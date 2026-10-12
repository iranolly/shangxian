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
    word: "没启动",
    sentence: "程序没在跑；填好启动程序后，点「启动」就能帮你拉起来。",
  },
  starting: {
    word: "正在启动",
    sentence: "程序已经拉起来，正在看端口通不通，等几秒。",
  },
  running: {
    word: "在跑",
    sentence: "程序在跑，端口也通。",
  },
  not_listening: {
    word: "进程在但端口不通",
    sentence: "程序还在，但它监听的不是这个端口：换一个端口，或者看程序日志。",
  },
  failed: {
    word: "起不来（已停止自动重试）",
    sentence: "程序启动失败，已经停止自动重试。检查启动程序、参数、工作目录，或看日志。",
  },
};

export const TUNNEL_STATUS_META: Record<TunnelStatus, StatusMeta> = {
  running: {
    word: "正在发布",
    sentence: "隧道通了；云端配置贴好、域名解析对了之后，别人才能打开。",
    action: "暂停",
  },
  connecting: {
    word: "连接中",
    sentence: "正在和云端服务器建立连接；如果一直停在这里，多半是连不上，点「看日志」看原因。",
  },
  disconnected: {
    word: "掉线重连中",
    sentence: "云端服务器联系不上了，可能网络波动。会自动重试。",
    action: "立刻重连",
  },
  error: {
    word: "出错",
    sentence: "发布失败。",
    action: "重试",
  },
  paused: {
    word: "已暂停",
    sentence: "你暂停了发布，现在没人能访问。",
    action: "开始发布",
  },
};

/** 本机服务排序：坏状态排前面（起不来 > 端口不通 > 正在启动 > 没启动 > 在跑） */
export const SERVICE_ORDER: Record<ServiceStatus, number> = {
  failed: 0,
  not_listening: 1,
  starting: 2,
  stopped: 3,
  running: 4,
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
  warn: { word: "要留意", icon: "!", tone: "warn" },
  fail: { word: "没通过", icon: "✕", tone: "error" },
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
