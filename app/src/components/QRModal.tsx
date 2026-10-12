// 二维码弹层（D4 §3.8 / ⑰）：离线生成、白底黑码、自绘 SVG。
//
// 不引 CDN、不联网：下面自带一个字节模式的二维码编码器（ECC L/M，版本 1..40）。
// 纠错块表与对齐图案位置表是 ISO/IEC 18004 的公开标准数据；算法按标准实现，
// 已用独立解码器对参考实现做过逐格比对（含高版本与大内容）。

import { useMemo, useRef, useState } from "react";

import { copyText } from "../copy/clipboard";
import { T } from "../copy/toasts";
import { toast } from "../store/toastStore";
import { useModalA11y } from "./useModalA11y";
import "../styles/modals.css";

// ---------- 二维码编码器（字节模式） ----------

/** 每个版本、每个纠错级别的分块参数：[块数, 总码字数, 数据码字数]（可有多组） */
const QR_RS_L: number[][][] = [
  [[1, 26, 19]], [[1, 44, 34]], [[1, 70, 55]], [[1, 100, 80]], [[1, 134, 108]],
  [[2, 86, 68]], [[2, 98, 78]], [[2, 121, 97]], [[2, 146, 116]], [[2, 86, 68], [2, 87, 69]],
  [[4, 101, 81]], [[2, 116, 92], [2, 117, 93]], [[4, 133, 107]], [[3, 145, 115], [1, 146, 116]],
  [[5, 109, 87], [1, 110, 88]], [[5, 122, 98], [1, 123, 99]], [[1, 135, 107], [5, 136, 108]],
  [[5, 150, 120], [1, 151, 121]], [[3, 141, 113], [4, 142, 114]], [[3, 135, 107], [5, 136, 108]],
  [[4, 144, 116], [4, 145, 117]], [[2, 139, 111], [7, 140, 112]], [[4, 151, 121], [5, 152, 122]],
  [[6, 147, 117], [4, 148, 118]], [[8, 132, 106], [4, 133, 107]], [[10, 142, 114], [2, 143, 115]],
  [[8, 152, 122], [4, 153, 123]], [[3, 147, 117], [10, 148, 118]], [[7, 146, 116], [7, 147, 117]],
  [[5, 145, 115], [10, 146, 116]], [[13, 145, 115], [3, 146, 116]], [[17, 145, 115]],
  [[17, 145, 115], [1, 146, 116]], [[13, 145, 115], [6, 146, 116]], [[12, 151, 121], [7, 152, 122]],
  [[6, 151, 121], [14, 152, 122]], [[17, 152, 122], [4, 153, 123]], [[4, 152, 122], [18, 153, 123]],
  [[20, 147, 117], [4, 148, 118]], [[19, 148, 118], [6, 149, 119]],
];
const QR_RS_M: number[][][] = [
  [[1, 26, 16]], [[1, 44, 28]], [[1, 70, 44]], [[2, 50, 32]], [[2, 67, 43]],
  [[4, 43, 27]], [[4, 49, 31]], [[2, 60, 38], [2, 61, 39]], [[3, 58, 36], [2, 59, 37]],
  [[4, 69, 43], [1, 70, 44]], [[1, 80, 50], [4, 81, 51]], [[6, 58, 36], [2, 59, 37]],
  [[8, 59, 37], [1, 60, 38]], [[4, 64, 40], [5, 65, 41]], [[5, 65, 41], [5, 66, 42]],
  [[7, 73, 45], [3, 74, 46]], [[10, 74, 46], [1, 75, 47]], [[9, 69, 43], [4, 70, 44]],
  [[3, 70, 44], [11, 71, 45]], [[3, 67, 41], [13, 68, 42]], [[17, 68, 42]], [[17, 74, 46]],
  [[4, 75, 47], [14, 76, 48]], [[6, 73, 45], [14, 74, 46]], [[8, 75, 47], [13, 76, 48]],
  [[19, 74, 46], [4, 75, 47]], [[22, 73, 45], [3, 74, 46]], [[3, 73, 45], [23, 74, 46]],
  [[21, 73, 45], [7, 74, 46]], [[19, 75, 47], [10, 76, 48]], [[2, 74, 46], [29, 75, 47]],
  [[10, 74, 46], [23, 75, 47]], [[14, 74, 46], [21, 75, 47]], [[14, 74, 46], [23, 75, 47]],
  [[12, 75, 47], [26, 76, 48]], [[6, 75, 47], [34, 76, 48]], [[29, 74, 46], [14, 75, 47]],
  [[13, 74, 46], [32, 75, 47]], [[40, 75, 47], [7, 76, 48]], [[18, 75, 47], [31, 76, 48]],
];
/** 每个版本的对齐图案中心坐标 */
const QR_ALIGN_POS: number[][] = [
  [], [6, 18], [6, 22], [6, 26], [6, 30], [6, 34], [6, 22, 38], [6, 24, 42], [6, 26, 46],
  [6, 28, 50], [6, 30, 54], [6, 32, 58], [6, 34, 62], [6, 26, 46, 66], [6, 26, 48, 70],
  [6, 26, 50, 74], [6, 30, 54, 78], [6, 30, 56, 82], [6, 30, 58, 86], [6, 34, 62, 90],
  [6, 28, 50, 72, 94], [6, 26, 50, 74, 98], [6, 30, 54, 78, 102], [6, 28, 54, 80, 106],
  [6, 32, 58, 84, 110], [6, 30, 58, 86, 114], [6, 34, 62, 90, 118], [6, 26, 50, 74, 98, 122],
  [6, 30, 54, 78, 102, 126], [6, 26, 52, 78, 104, 130], [6, 30, 56, 82, 108, 134],
  [6, 34, 60, 86, 112, 138], [6, 30, 58, 86, 114, 142], [6, 34, 62, 90, 118, 146],
  [6, 30, 54, 78, 102, 126, 150], [6, 24, 50, 76, 102, 128, 154], [6, 28, 54, 80, 106, 132, 158],
  [6, 32, 58, 84, 110, 136, 162], [6, 26, 54, 82, 110, 138, 166], [6, 30, 58, 86, 114, 142, 170],
];

const GF_EXP = new Uint8Array(512);
const GF_LOG = new Uint8Array(256);
{
  let x = 1;
  for (let i = 0; i < 255; i += 1) {
    GF_EXP[i] = x;
    GF_LOG[x] = i;
    x <<= 1;
    if (x & 0x100) x ^= 0x11d;
  }
  for (let i = 255; i < 512; i += 1) GF_EXP[i] = GF_EXP[i - 255];
}
const gfMul = (a: number, b: number): number =>
  a === 0 || b === 0 ? 0 : GF_EXP[GF_LOG[a] + GF_LOG[b]];

function rsGenerator(ecCount: number): number[] {
  let poly = [1];
  for (let i = 0; i < ecCount; i += 1) {
    const next = new Array<number>(poly.length + 1).fill(0);
    for (let j = 0; j < poly.length; j += 1) {
      next[j] ^= poly[j];
      next[j + 1] ^= gfMul(poly[j], GF_EXP[i]);
    }
    poly = next;
  }
  return poly;
}

function rsRemainder(data: number[], ecCount: number, gen: number[]): number[] {
  const res = new Array<number>(ecCount).fill(0);
  for (const b of data) {
    const factor = b ^ res[0];
    for (let i = 0; i < ecCount - 1; i += 1) res[i] = res[i + 1];
    res[ecCount - 1] = 0;
    if (factor !== 0) {
      for (let i = 0; i < ecCount; i += 1) res[i] ^= gfMul(gen[i + 1], factor);
    }
  }
  return res;
}

function chooseVersion(byteLen: number, ecc: "L" | "M"): number {
  const table = ecc === "L" ? QR_RS_L : QR_RS_M;
  for (let v = 1; v <= 40; v += 1) {
    const data = table[v - 1].reduce((n, [c, , d]) => n + c * d, 0);
    const lenBits = v <= 9 ? 8 : 16;
    if (4 + lenBits + byteLen * 8 <= data * 8) return v;
  }
  throw new Error("内容太长，二维码放不下");
}

function makeCodewords(bytes: number[], version: number, ecc: "L" | "M"): number[] {
  const table = ecc === "L" ? QR_RS_L : QR_RS_M;
  const blocks = table[version - 1];
  const dataCount = blocks.reduce((n, [c, , d]) => n + c * d, 0);
  const bits: number[] = [];
  const put = (value: number, len: number) => {
    for (let i = len - 1; i >= 0; i -= 1) bits.push((value >>> i) & 1);
  };
  put(0b0100, 4);
  put(bytes.length, version <= 9 ? 8 : 16);
  for (const b of bytes) put(b, 8);
  const capacity = dataCount * 8;
  put(0, Math.min(4, capacity - bits.length));
  while (bits.length % 8 !== 0) bits.push(0);
  const codewords: number[] = [];
  for (let i = 0; i < bits.length; i += 8) {
    let v = 0;
    for (let j = 0; j < 8; j += 1) v = (v << 1) | bits[i + j];
    codewords.push(v);
  }
  let pad = 0xec;
  while (codewords.length < dataCount) {
    codewords.push(pad);
    pad = pad === 0xec ? 0x11 : 0xec;
  }
  const dataBlocks: number[][] = [];
  const ecBlocks: number[][] = [];
  let offset = 0;
  for (const [count, total, dataLen] of blocks) {
    const ecLen = total - dataLen;
    const gen = rsGenerator(ecLen);
    for (let i = 0; i < count; i += 1) {
      const block = codewords.slice(offset, offset + dataLen);
      offset += dataLen;
      dataBlocks.push(block);
      ecBlocks.push(rsRemainder(block, ecLen, gen));
    }
  }
  const maxData = Math.max(...dataBlocks.map((b) => b.length));
  const maxEc = Math.max(...ecBlocks.map((b) => b.length));
  const out: number[] = [];
  for (let i = 0; i < maxData; i += 1) {
    for (const b of dataBlocks) if (i < b.length) out.push(b[i]);
  }
  for (let i = 0; i < maxEc; i += 1) {
    for (const b of ecBlocks) if (i < b.length) out.push(b[i]);
  }
  return out;
}

function bitLen(n: number): number {
  let len = 0;
  let value = n;
  while (value !== 0) {
    len += 1;
    value >>>= 1;
  }
  return len;
}

function bchTypeInfo(data: number): number {
  const G15 = 0x537;
  let d = data << 10;
  while (bitLen(d) - bitLen(G15) >= 0) d ^= G15 << (bitLen(d) - bitLen(G15));
  return ((data << 10) | d) ^ 0x5412;
}

function bchTypeNumber(data: number): number {
  const G18 = 0x1f25;
  let d = data << 12;
  while (bitLen(d) - bitLen(G18) >= 0) d ^= G18 << (bitLen(d) - bitLen(G18));
  return (data << 12) | d;
}

function maskFn(mask: number, r: number, c: number): boolean {
  switch (mask) {
    case 0:
      return (r + c) % 2 === 0;
    case 1:
      return r % 2 === 0;
    case 2:
      return c % 3 === 0;
    case 3:
      return (r + c) % 3 === 0;
    case 4:
      return (Math.floor(r / 2) + Math.floor(c / 3)) % 2 === 0;
    case 5:
      return ((r * c) % 2) + ((r * c) % 3) === 0;
    case 6:
      return (((r * c) % 2) + ((r * c) % 3)) % 2 === 0;
    default:
      return (((r * c) % 3) + ((r + c) % 2)) % 2 === 0;
  }
}

function buildBase(version: number): (boolean | null)[][] {
  const size = version * 4 + 17;
  const m: (boolean | null)[][] = [];
  for (let r = 0; r < size; r += 1) m.push(new Array<boolean | null>(size).fill(null));
  const finder = (row: number, col: number) => {
    for (let r = -1; r <= 7; r += 1) {
      for (let c = -1; c <= 7; c += 1) {
        if (row + r < 0 || row + r >= size || col + c < 0 || col + c >= size) continue;
        const dark =
          (r >= 0 && r <= 6 && (c === 0 || c === 6)) ||
          (c >= 0 && c <= 6 && (r === 0 || r === 6)) ||
          (r >= 2 && r <= 4 && c >= 2 && c <= 4);
        m[row + r][col + c] = dark;
      }
    }
  };
  finder(0, 0);
  finder(size - 7, 0);
  finder(0, size - 7);
  const pos = QR_ALIGN_POS[version - 1];
  for (const r of pos) {
    for (const c of pos) {
      if (m[r][c] !== null) continue;
      for (let dr = -2; dr <= 2; dr += 1) {
        for (let dc = -2; dc <= 2; dc += 1) {
          m[r + dr][c + dc] = Math.max(Math.abs(dr), Math.abs(dc)) !== 1;
        }
      }
    }
  }
  for (let i = 8; i < size - 8; i += 1) {
    if (m[6][i] === null) m[6][i] = i % 2 === 0;
    if (m[i][6] === null) m[i][6] = i % 2 === 0;
  }
  // 类型信息 / 版本信息格先占住（先填 false，数据映射会跳过非 null 格）
  for (let i = 0; i < 15; i += 1) {
    if (i < 6) m[i][8] = false;
    else if (i < 8) m[i + 1][8] = false;
    else m[size - 15 + i][8] = false;
    if (i < 8) m[8][size - i - 1] = false;
    else if (i < 9) m[8][15 - i] = false;
    else m[8][15 - i - 1] = false;
  }
  m[size - 8][8] = false;
  if (version >= 7) {
    for (let i = 0; i < 18; i += 1) {
      m[Math.floor(i / 3)][(i % 3) + size - 11] = false;
      m[(i % 3) + size - 11][Math.floor(i / 3)] = false;
    }
  }
  return m;
}

function mapData(m: (boolean | null)[][], codewords: number[], mask: number): void {
  const size = m.length;
  let inc = -1;
  let row = size - 1;
  let bitIndex = 7;
  let byteIndex = 0;
  for (let col = size - 1; col > 0; col -= 2) {
    if (col === 6) col -= 1;
    for (;;) {
      for (let c = 0; c < 2; c += 1) {
        if (m[row][col - c] !== null) continue;
        let dark = false;
        if (byteIndex < codewords.length) dark = ((codewords[byteIndex] >>> bitIndex) & 1) === 1;
        if (maskFn(mask, row, col - c)) dark = !dark;
        m[row][col - c] = dark;
        bitIndex -= 1;
        if (bitIndex === -1) {
          byteIndex += 1;
          bitIndex = 7;
        }
      }
      row += inc;
      if (row < 0 || row >= size) {
        row -= inc;
        inc = -inc;
        break;
      }
    }
  }
}

function penalty(m: (boolean | null)[][]): number {
  const size = m.length;
  const get = (r: number, c: number) => m[r][c] === true;
  let score = 0;
  const runScore = (len: number) => (len >= 5 ? 3 + (len - 5) : 0);
  for (let r = 0; r < size; r += 1) {
    let run = 1;
    for (let c = 1; c < size; c += 1) {
      if (get(r, c) === get(r, c - 1)) run += 1;
      else {
        score += runScore(run);
        run = 1;
      }
    }
    score += runScore(run);
  }
  for (let c = 0; c < size; c += 1) {
    let run = 1;
    for (let r = 1; r < size; r += 1) {
      if (get(r, c) === get(r - 1, c)) run += 1;
      else {
        score += runScore(run);
        run = 1;
      }
    }
    score += runScore(run);
  }
  for (let r = 0; r < size - 1; r += 1) {
    for (let c = 0; c < size - 1; c += 1) {
      const v = get(r, c);
      if (v === get(r, c + 1) && v === get(r + 1, c) && v === get(r + 1, c + 1)) score += 3;
    }
  }
  const pat1 = [true, false, true, true, true, false, true, false, false, false, false];
  const pat2 = [false, false, false, false, true, false, true, true, true, false, true];
  const matches = (arr: boolean[], pat: boolean[], start: number) => {
    for (let i = 0; i < pat.length; i += 1) if (arr[start + i] !== pat[i]) return false;
    return true;
  };
  for (let r = 0; r < size; r += 1) {
    const rowArr = [];
    for (let c = 0; c < size; c += 1) rowArr.push(get(r, c));
    for (let c = 0; c + 11 <= size; c += 1) {
      if (matches(rowArr, pat1, c) || matches(rowArr, pat2, c)) score += 40;
    }
  }
  for (let c = 0; c < size; c += 1) {
    const colArr = [];
    for (let r = 0; r < size; r += 1) colArr.push(get(r, c));
    for (let r = 0; r + 11 <= size; r += 1) {
      if (matches(colArr, pat1, r) || matches(colArr, pat2, r)) score += 40;
    }
  }
  let dark = 0;
  for (let r = 0; r < size; r += 1) {
    for (let c = 0; c < size; c += 1) if (get(r, c)) dark += 1;
  }
  const percent = (dark * 100) / (size * size);
  score += Math.floor(Math.abs(percent - 50) / 5) * 10;
  return score;
}

function placeFormat(
  m: (boolean | null)[][],
  version: number,
  ecc: "L" | "M",
  mask: number,
): void {
  const size = m.length;
  const eccBits = ecc === "L" ? 1 : 0;
  const bits = bchTypeInfo((eccBits << 3) | mask);
  for (let i = 0; i < 15; i += 1) {
    const on = ((bits >>> i) & 1) === 1;
    if (i < 6) m[i][8] = on;
    else if (i < 8) m[i + 1][8] = on;
    else m[size - 15 + i][8] = on;
    if (i < 8) m[8][size - i - 1] = on;
    else if (i < 9) m[8][15 - i] = on;
    else m[8][15 - i - 1] = on;
  }
  m[size - 8][8] = true;
  if (version >= 7) {
    const vbits = bchTypeNumber(version);
    for (let i = 0; i < 18; i += 1) {
      const on = ((vbits >>> i) & 1) === 1;
      m[Math.floor(i / 3)][(i % 3) + size - 11] = on;
      m[(i % 3) + size - 11][Math.floor(i / 3)] = on;
    }
  }
}

/** 生成二维码模块矩阵：true = 黑块。 */
function encodeQr(text: string, ecc: "L" | "M"): boolean[][] {
  const bytes = Array.from(new TextEncoder().encode(text));
  const version = chooseVersion(bytes.length, ecc);
  const codewords = makeCodewords(bytes, version, ecc);
  const base = buildBase(version);
  let best = 0;
  let bestScore = Number.POSITIVE_INFINITY;
  for (let mask = 0; mask < 8; mask += 1) {
    const candidate = base.map((row) => row.slice());
    mapData(candidate, codewords, mask);
    const score = penalty(candidate);
    if (score < bestScore) {
      bestScore = score;
      best = mask;
    }
  }
  const out = base.map((row) => row.slice());
  mapData(out, codewords, best);
  placeFormat(out, version, ecc, best);
  return out.map((row) => row.map((v) => v === true));
}

const QUIET = 4;
/** 每个二维码模块画 6 像素（D4 §3.8）：画布长边 = (模块数 + 静区×2) × 6 */
const MODULE_PX = 6;
const LONG_URL = 300;

function qrPath(matrix: boolean[][]): string {
  const n = matrix.length;
  let d = "";
  for (let r = 0; r < n; r += 1) {
    for (let c = 0; c < n; c += 1) {
      if (matrix[r][c]) d += `M${c + QUIET} ${r + QUIET}h1v1h-1z`;
    }
  }
  return d;
}

interface Props {
  url: string;
  name: string;
  onClose(): void;
}

export function QrModal({ url, name, onClose }: Props) {
  const dialogRef = useRef<HTMLDivElement | null>(null);
  useModalA11y(dialogRef, onClose);
  const [copyFailed, setCopyFailed] = useState(false);

  const longUrl = url.length > LONG_URL;
  const ecc: "L" | "M" = longUrl ? "L" : "M";
  const matrix = useMemo(() => {
    try {
      return encodeQr(url, ecc);
    } catch {
      return null;
    }
  }, [url, ecc]);

  const copy = async () => {
    const ok = await copyText(url);
    setCopyFailed(!ok);
    if (ok) toast.success(T.urlCopied(url));
  };

  // 静区 4 模块 + 每模块 6px：长网址自动变大，别把模块缩糊
  const qrSize = matrix ? (matrix.length + QUIET * 2) * MODULE_PX : 0;

  return (
    <div className="modal-backdrop">
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        ref={dialogRef}
        style={{ width: 420 }}
      >
        <div className="modal-head">
          <h2 className="modal-title">手机扫码打开 · {name}</h2>
          <p className="modal-sub">扫不出来就复制网址发到手机。</p>
        </div>
        <div className="modal-body">
          {matrix ? (
            <div style={{ textAlign: "center" }}>
              <span className="qr-box">
                <svg
                  className="qr-svg"
                  width={qrSize}
                  height={qrSize}
                  viewBox={`0 0 ${matrix.length + QUIET * 2} ${matrix.length + QUIET * 2}`}
                  style={{ maxWidth: "100%", height: "auto" }}
                  role="img"
                  aria-label={`打开 ${name} 的二维码`}
                  shapeRendering="crispEdges"
                >
                  <rect
                    x={0}
                    y={0}
                    width={matrix.length + QUIET * 2}
                    height={matrix.length + QUIET * 2}
                    fill="#ffffff"
                  />
                  <path d={qrPath(matrix)} fill="#000000" />
                </svg>
              </span>
            </div>
          ) : (
            <div className="error-line">
              网址太长，二维码放不下；请用下面的「复制网址」发到手机。
            </div>
          )}

          <div className="qr-url">
            <span className="mono">{url}</span>
            <button className="btn btn-ghost btn-sm" onClick={() => void copy()}>
              复制网址
            </button>
          </div>
          {copyFailed && (
            <div className="error-line" style={{ marginTop: 8 }}>
              没复制成功：选中网址按 Ctrl+C。
            </div>
          )}
          <p className="field-hint hint" title="不联网也能生成，网址不会发到网上。">
            二维码是离线生成的。
          </p>
          {longUrl && matrix ? (
            <p className="field-hint hint">网址有点长，二维码比较密，尽量用复制。</p>
          ) : null}
        </div>
        <div className="modal-foot">
          <span className="grow" />
          <button className="btn btn-primary" onClick={onClose}>
            关闭
          </button>
        </div>
      </div>
    </div>
  );
}
