// 悬浮说明的小问号：界面上不铺文字，鼠标停留（hover）时才显示。
// 用法：<InfoTip text="大白话说明" />；样式自带，不要另外写 CSS。
export function InfoTip({ text }: { text: string }) {
  return (
    <span
      title={text}
      aria-label={text}
      role="img"
      style={{
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        width: 14,
        height: 14,
        marginLeft: 4,
        borderRadius: "50%",
        border: "1px solid var(--border-strong)",
        color: "var(--text-3)",
        fontSize: 11,
        lineHeight: 1,
        cursor: "help",
        verticalAlign: "text-bottom",
        userSelect: "none",
      }}
    >
      ?
    </span>
  );
}
