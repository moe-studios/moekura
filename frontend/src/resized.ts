// "Resized to N% of the original": the link switches between the sample
// and the original in place. Without scripts, it reloads the page.

import { t } from "./i18n.ts";

export function enableResized(): void {
  const notice = document.querySelector<HTMLElement>("[data-resized]");
  const image = document.querySelector<HTMLImageElement>("[data-notes] img");
  const link = notice?.querySelector("a");
  const text = notice?.querySelector("span");
  const { sample, original, percent } = notice?.dataset ?? {};
  if (!notice || !image || !link || !text || !sample || !original) return;
  link.addEventListener("click", (event) => {
    event.preventDefault();
    const showOriginal = image.getAttribute("src") !== original;
    image.src = showOriginal ? original : sample;
    text.textContent = showOriginal
      ? t("resized-original", "Showing the original.")
      : t("resized-to", "Resized to {$percent}% of the original.", { percent: percent ?? "" });
    link.textContent = showOriginal ? t("resized-show-resized", "Show the resized image") : t("resized-show-original", "View the original");
  });
}
