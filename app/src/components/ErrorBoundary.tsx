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
          <h1 className="page-title">界面出了点问题</h1>
          <p className="page-sub">
            别担心：设置、服务、隧道都没事。点下面按钮重新加载界面就能接着用；还不行就退出应用重开一次。
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
