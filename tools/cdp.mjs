// 用 Chrome DevTools 协议驱动运行中的应用（不注入鼠标，不打扰前台窗口）。
//
//   node tools/cdp.mjs eval "js 表达式"
//   node tools/cdp.mjs shot 输出.png
//   node tools/cdp.mjs targets

const PORT = process.env.CDP_PORT || "9222";

async function pageTarget() {
  const res = await fetch(`http://127.0.0.1:${PORT}/json/list`);
  const list = await res.json();
  const page = list.find((t) => t.type === "page" && t.url.includes("1420"));
  if (!page) throw new Error("没找到应用页面，检查 remote-debugging-port");
  return page;
}

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let id = 0;
    const pending = new Map();
    ws.onopen = () =>
      resolve({
        send(method, params = {}) {
          return new Promise((res, rej) => {
            const msgId = ++id;
            pending.set(msgId, { res, rej });
            ws.send(JSON.stringify({ id: msgId, method, params }));
          });
        },
        close: () => ws.close(),
      });
    ws.onerror = (e) => reject(e);
    ws.onmessage = (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && pending.has(msg.id)) {
        const { res, rej } = pending.get(msg.id);
        pending.delete(msg.id);
        if (msg.error) rej(new Error(JSON.stringify(msg.error)));
        else res(msg.result);
      }
    };
  });
}

const [cmd, arg] = process.argv.slice(2);

if (cmd === "targets") {
  const res = await fetch(`http://127.0.0.1:${PORT}/json/list`);
  console.log(await res.text());
  process.exit(0);
}

const page = await pageTarget();
const client = await connect(page.webSocketDebuggerUrl);

if (cmd === "call") {
  // 通用调用：cdp.mjs call Page.handleJavaScriptDialog '{"accept":true}'
  const method = arg;
  const params = process.argv[4] ? JSON.parse(process.argv[4]) : {};
  const result = await client.send(method, params);
  console.log(JSON.stringify(result, null, 2));
} else if (cmd === "eval") {
  const result = await client.send("Runtime.evaluate", {
    expression: arg,
    returnByValue: true,
    awaitPromise: true,
  });
  if (result.exceptionDetails) {
    console.error("页面里报错：", JSON.stringify(result.exceptionDetails, null, 2));
    process.exit(1);
  }
  const value = result.result?.value;
  console.log(typeof value === "string" ? value : JSON.stringify(value, null, 2));
} else if (cmd === "shot") {
  const shot = await client.send("Page.captureScreenshot", { format: "png" });
  const { writeFileSync } = await import("node:fs");
  writeFileSync(arg || "shot.png", Buffer.from(shot.data, "base64"));
  console.log("已保存", arg);
} else {
  console.error("用法：cdp.mjs eval|shot|targets");
  process.exit(2);
}

client.close();
process.exit(0);
