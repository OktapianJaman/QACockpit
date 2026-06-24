// ---------------------------------------------------------------------------
// Screenshot annotator — draw rectangles / arrows / redaction boxes over an
// image on a <canvas>, then flatten to a new PNG data URL. Self-contained and
// promise-based: openAnnotator(dataUrl) resolves to the edited image, or null
// if the user cancels.
// ---------------------------------------------------------------------------

import { $, show } from "./dom";

type Tool = "rect" | "arrow" | "redact";

interface Point {
  x: number;
  y: number;
}
interface Shape {
  tool: Tool;
  a: Point;
  b: Point;
}

const STROKE = "#e5484d"; // red, matches the app accent for warnings
const LINE_WIDTH = 3;

let canvas: HTMLCanvasElement;
let ctx: CanvasRenderingContext2D;
let baseImage: HTMLImageElement | null = null;
let shapes: Shape[] = [];
let tool: Tool = "rect";
let drawing = false;
let start: Point = { x: 0, y: 0 };
let cursor: Point = { x: 0, y: 0 };
let resolveFn: ((value: string | null) => void) | null = null;

/** Map a mouse event to canvas-pixel coordinates (canvas is CSS-scaled). */
function toCanvasPoint(e: MouseEvent): Point {
  const rect = canvas.getBoundingClientRect();
  return {
    x: ((e.clientX - rect.left) / rect.width) * canvas.width,
    y: ((e.clientY - rect.top) / rect.height) * canvas.height,
  };
}

function drawShape(s: Shape): void {
  const { a, b } = s;
  if (s.tool === "redact") {
    ctx.fillStyle = "#000";
    ctx.fillRect(Math.min(a.x, b.x), Math.min(a.y, b.y), Math.abs(b.x - a.x), Math.abs(b.y - a.y));
    return;
  }
  ctx.strokeStyle = STROKE;
  ctx.fillStyle = STROKE;
  ctx.lineWidth = LINE_WIDTH;
  if (s.tool === "rect") {
    ctx.strokeRect(
      Math.min(a.x, b.x),
      Math.min(a.y, b.y),
      Math.abs(b.x - a.x),
      Math.abs(b.y - a.y)
    );
  } else {
    // Arrow: shaft + filled head.
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.stroke();
    const angle = Math.atan2(b.y - a.y, b.x - a.x);
    const head = 14;
    ctx.beginPath();
    ctx.moveTo(b.x, b.y);
    ctx.lineTo(b.x - head * Math.cos(angle - Math.PI / 6), b.y - head * Math.sin(angle - Math.PI / 6));
    ctx.lineTo(b.x - head * Math.cos(angle + Math.PI / 6), b.y - head * Math.sin(angle + Math.PI / 6));
    ctx.closePath();
    ctx.fill();
  }
}

function redraw(): void {
  if (!baseImage) return;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(baseImage, 0, 0, canvas.width, canvas.height);
  for (const s of shapes) drawShape(s);
  if (drawing) drawShape({ tool, a: start, b: cursor });
}

function setTool(next: Tool): void {
  tool = next;
  for (const t of ["rect", "arrow", "redact"] as Tool[]) {
    $(`ann-tool-${t}`).classList.toggle("active", t === next);
  }
}

function finish(value: string | null): void {
  show($("annotate-overlay"), false);
  const r = resolveFn;
  resolveFn = null;
  baseImage = null;
  shapes = [];
  if (r) r(value);
}

/** Wire the annotator overlay once (call from app init). */
export function wireAnnotator(): void {
  canvas = $("ann-canvas") as HTMLCanvasElement;
  const c = canvas.getContext("2d");
  if (!c) throw new Error("canvas 2d context tidak tersedia");
  ctx = c;

  ($("ann-tool-rect") as HTMLElement).addEventListener("click", () => setTool("rect"));
  ($("ann-tool-arrow") as HTMLElement).addEventListener("click", () => setTool("arrow"));
  ($("ann-tool-redact") as HTMLElement).addEventListener("click", () => setTool("redact"));
  $("ann-undo").addEventListener("click", () => {
    shapes.pop();
    redraw();
  });
  $("ann-clear").addEventListener("click", () => {
    shapes = [];
    redraw();
  });
  $("ann-cancel").addEventListener("click", () => finish(null));
  $("ann-save").addEventListener("click", () => finish(canvas.toDataURL("image/png")));

  canvas.addEventListener("mousedown", (e) => {
    drawing = true;
    start = toCanvasPoint(e);
    cursor = start;
  });
  canvas.addEventListener("mousemove", (e) => {
    if (!drawing) return;
    cursor = toCanvasPoint(e);
    redraw();
  });
  const commit = (e: MouseEvent): void => {
    if (!drawing) return;
    drawing = false;
    const end = toCanvasPoint(e);
    // Ignore zero-size click (no drag).
    if (Math.abs(end.x - start.x) > 2 || Math.abs(end.y - start.y) > 2) {
      shapes.push({ tool, a: start, b: end });
    }
    redraw();
  };
  canvas.addEventListener("mouseup", commit);
  canvas.addEventListener("mouseleave", commit);
}

/** Cancel an open annotator session (for the global Escape handler). No-op if
 *  the annotator isn't open. */
export function cancelAnnotator(): void {
  if (resolveFn) finish(null);
}

/** Open the annotator on an image. Resolves to the edited PNG data URL, or null
 *  if cancelled. Only one annotator session is active at a time. */
export function openAnnotator(dataUrl: string): Promise<string | null> {
  return new Promise((resolve) => {
    resolveFn = resolve;
    shapes = [];
    drawing = false;
    setTool("rect");
    const img = new Image();
    img.onload = () => {
      baseImage = img;
      canvas.width = img.naturalWidth;
      canvas.height = img.naturalHeight;
      redraw();
      show($("annotate-overlay"), true);
    };
    img.onerror = () => finish(null);
    img.src = dataUrl;
  });
}
