// 顶层兜底：界面里任何一处渲染报错，都显示人话提示 + 一键重载，
// 而不是整窗空白（黑屏那种）。React 渲染期异常如果不拦，会把整个界面卸掉。
import { Component, type ReactNode } from "react";

interface Props {
  children: ReactNode;
}
interface State {
  failed: boolean;
}

export class ErrorBoundary extends Component<Props, State> {
  state: State = { failed: false };

  static getDerivedStateFromError(): State {
    return { failed: true };
  }

  componentDidCatch(error: unknown): void {
    // 技术细节只进控制台，窗口里只显示人话
    console.error("[界面错误]", error);
  }

  render(): ReactNode {
    if (this.state.failed) {
      return (
        <div className="page">
          <h1 className="page-title">界面出现异常</h1>
          <p className="page-sub">
            设置、服务和隧道数据均未受影响。点击下方按钮重新加载界面即可继续使用；如仍无效，请退出应用后重新启动。
          </p>
          <button
            className="btn btn-primary"
            onClick={() => window.location.reload()}
          >
            重新加载界面
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}
