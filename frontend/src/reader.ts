// Remembers the last page read of each pool, in this browser, and offers
// to continue from it on the pool's page. Nothing is sent to the server.

const PREFIX = "moekura:read:";

function load(pool: string): number | null {
  try {
    const value = Number(localStorage.getItem(PREFIX + pool));
    return Number.isInteger(value) && value > 0 ? value : null;
  } catch {
    // Storage can be off (private windows, strict settings).
    return null;
  }
}

function save(pool: string, page: number): void {
  try {
    localStorage.setItem(PREFIX + pool, String(page));
  } catch {
    // As above: nothing to remember with.
  }
}

export function enableReader(root: Document = document): void {
  const reader = root.querySelector<HTMLElement>("[data-reader]");
  const pool = reader?.dataset["reader"];
  if (reader && pool) {
    const pages = [...reader.querySelectorAll<HTMLElement>("[data-page]")];
    const [only] = pages;
    if (pages.length === 1 && only) {
      save(pool, Number(only.dataset["page"]));
    } else if (pages.length > 1 && "IntersectionObserver" in window) {
      // The page most in view is the one being read.
      const observer = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            if (entry.isIntersecting) save(pool, Number((entry.target as HTMLElement).dataset["page"]));
          }
        },
        { threshold: 0.5 },
      );
      for (const page of pages) observer.observe(page);
    }
  }

  const resume = root.querySelector<HTMLElement>("[data-reader-resume]");
  const resumePool = resume?.dataset["readerResume"];
  const link = resume?.querySelector<HTMLAnchorElement>("a");
  if (resume && resumePool && link) {
    const page = load(resumePool);
    if (page !== null && page > 1) {
      link.href = `/pools/${resumePool}/read/${page}`;
      link.textContent = `Continue reading from page ${page}`;
      resume.hidden = false;
    }
  }
}
