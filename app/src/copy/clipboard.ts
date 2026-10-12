// 复制一段文字到系统剪贴板。
// 优先用浏览器自带的新接口；旧环境、或没给权限时，退回"选中 + 复制"的老办法。
// 返回是否成功：界面要按这个结果给用户提示，不能默默失败。

import { toast } from "../store/toastStore";

export async function copyText(text: string): Promise<boolean> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // 被拒绝或功能不可用：继续走下面那个老办法
    }
  }

  // 老办法：临时放一个看不见的输入框，选中它，再让浏览器执行复制
  try {
    const area = document.createElement("textarea");
    area.value = text;
    area.setAttribute("readonly", "");
    area.style.position = "fixed";
    area.style.top = "0";
    area.style.left = "0";
    area.style.opacity = "0";
    document.body.appendChild(area);
    area.focus();
    area.select();
    const ok = document.execCommand("copy");
    area.remove();
    return ok;
  } catch {
    return false;
  }
}

/**
 * 复制 + 就地反馈：成功给 successText，失败给一条人话错误。
 * 返回是否复制成功（调用方要按结果决定后续动作时用）。
 */
export async function copyWithToast(
  text: string,
  successText: string,
  failText = "复制失败，请手动选中后复制",
): Promise<boolean> {
  const ok = await copyText(text);
  if (ok) toast.success(successText);
  else toast.error(failText);
  return ok;
}
