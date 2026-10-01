// The square thumbnail form: while it's open, the chosen square is
// outlined on the picture, and clicking the picture centres the square
// on that spot.

/** The square of `side` centred on (`x`, `y`), kept inside the picture. */
export function centred(
  x: number,
  y: number,
  side: number,
  width: number,
  height: number,
): { left: number; top: number; side: number } {
  const s = Math.max(1, Math.min(side, width, height));
  const clamp = (n: number, max: number) => Math.max(0, Math.min(Math.round(n), max));
  return { left: clamp(x - s / 2, width - s), top: clamp(y - s / 2, height - s), side: s };
}

export function enableCropPicker(root: Document = document): void {
  const form = root.querySelector<HTMLFormElement>("[data-crop-form]");
  const details = form?.closest("details");
  const layer = root.querySelector<HTMLElement>("[data-notes]");
  const image = layer?.querySelector("img");
  if (!form || !details || !layer || !image) return;
  const width = Number(form.dataset["width"]);
  const height = Number(form.dataset["height"]);
  const field = (name: string) => form.querySelector<HTMLInputElement>(`[data-crop="${name}"]`);
  const [left, top, side] = [field("left"), field("top"), field("side")];
  if (!left || !top || !side || !width || !height) return;

  const outline = root.createElement("div");
  outline.className = "crop-outline";
  outline.hidden = true;
  layer.append(outline);
  const draw = () => {
    outline.hidden = !details.open;
    const [l, t, s] = [Number(left.value), Number(top.value), Number(side.value)];
    outline.style.left = `${(l / width) * 100}%`;
    outline.style.top = `${(t / height) * 100}%`;
    outline.style.width = `${(s / width) * 100}%`;
    outline.style.height = `${(s / height) * 100}%`;
  };
  details.addEventListener("toggle", draw);
  form.addEventListener("input", draw);
  image.addEventListener("click", (event) => {
    if (!details.open) return;
    event.preventDefault();
    const box = image.getBoundingClientRect();
    const x = ((event.clientX - box.left) / box.width) * width;
    const y = ((event.clientY - box.top) / box.height) * height;
    const square = centred(x, y, Number(side.value), width, height);
    left.value = String(square.left);
    top.value = String(square.top);
    side.value = String(square.side);
    draw();
  });
  draw();
}
