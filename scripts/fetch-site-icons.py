#!/usr/bin/env python3
"""Fetch each known site's favicon into crates/web/static/site-icons/.

Reads the sites from crates/core/src/sites/list.rs and, for each, saves
`<key>.png`: the best icon its home page links to (an apple-touch-icon,
then the largest raster `<link rel=icon>`, then /favicon.ico), scaled to
32px, on a white tile if it's a dark glyph on transparency that would
vanish on a dark theme. When the site turns the script away (most often Cloudflare's bot
check), its icon is taken from DuckDuckGo's or Google's favicon service instead
(both answer 404 for a host they have no icon for). SVG icons are skipped rather than rasterised, since they would need
a renderer run over files from the internet. A site whose icon can't be
fetched gets `<key>.svg`, its initials in an outlined square, instead.

    python3 -I scripts/fetch-site-icons.py [key ...]

With keys, only those sites are refetched. Needs Pillow.
"""

import html.parser
import http.cookiejar
import io
import re
import sys
import urllib.parse
import urllib.request
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
LIST = ROOT / "crates/core/src/sites/list.rs"
OUT = ROOT / "crates/web/static/site-icons"
SIZE = 32
MAX_BYTES = 8 * 1024 * 1024
USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"
)

# Initials for the fallback where a site's name's first letter won't do.
INITIALS = {
    "e621": "e6",
    "rule34_us": "R34",
    "rule34_xxx": "R34",
    "safebooru": "SB",
    "tbib": "TB",
    "adobe_portfolio": "Ap",
    "arca_live": "AL",
    "artistree": "At",
    "artstreet": "AS",
    "ci_en": "Ci",
    "dc_inside": "DC",
    "dotpict": "dp",
    "fantia": "Fa",
    "fc2": "FC2",
    "foriio": "Fo",
    "galleria": "Ga",
    "grafolio": "Gr",
    "hentai_foundry": "HF",
    "huajia": "Hj",
    "huashijie": "Hs",
    "inkbunny": "IB",
    "itaku": "It",
    "minitokyo": "MT",
    "postype": "Pt",
    "privatter": "Pv",
    "redgifs": "RG",
    "skland": "Sk",
    "toyhouse": "TH",
    "xfolio": "Xf",
}


def sites():
    """(key, name, url) for every site in list.rs."""
    text = LIST.read_text()
    pattern = re.compile(
        r'key: "([^"]+)",\s*name: "([^"]+)",\s*url: "([^"]+)"', re.S
    )
    return pattern.findall(text)


OPENER = urllib.request.build_opener(
    urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar())
)


def get(url):
    request = urllib.request.Request(
        url, headers={"User-Agent": USER_AGENT, "Accept": "*/*"}
    )
    with OPENER.open(request, timeout=20) as response:
        data = response.read(MAX_BYTES + 1)
        if len(data) > MAX_BYTES:
            raise ValueError("too large")
        return response.geturl(), data


class Links(html.parser.HTMLParser):
    """The icons a page's `<link>`s point at: (rel, sizes, type, href)."""

    def __init__(self):
        super().__init__()
        self.icons = []

    def handle_starttag(self, tag, attrs):
        if tag != "link":
            return
        attrs = dict(attrs)
        rel = (attrs.get("rel") or "").lower().split()
        href = attrs.get("href")
        if href and ("icon" in rel or "apple-touch-icon" in rel):
            self.icons.append(
                (rel, (attrs.get("sizes") or "").lower(), (attrs.get("type") or "").lower(), href)
            )


def candidates(page_url, page):
    """Icon URLs to try, best first."""
    links = Links()
    links.feed(page.decode("utf-8", "replace"))
    scored = []
    for rel, sizes, kind, href in links.icons:
        if "mask-icon" in rel or kind == "image/svg+xml" or href.split("?")[0].endswith(".svg"):
            continue
        size = max((int(n) for n in re.findall(r"(\d+)x\d+", sizes)), default=0)
        if "apple-touch-icon" in rel:
            size = max(size, 180)
        scored.append((size, urllib.parse.urljoin(page_url, href)))
    scored.sort(key=lambda pair: -pair[0])
    urls = [url for _, url in scored]
    urls += fallbacks(page_url)
    return list(dict.fromkeys(urls))


def fallbacks(url):
    """Icons to try when a page names none, or can't be read."""
    host = urllib.parse.urlsplit(url).hostname
    return [
        urllib.parse.urljoin(url, "/apple-touch-icon.png"),
        urllib.parse.urljoin(url, "/favicon.ico"),
        f"https://icons.duckduckgo.com/ip3/{host}.ico",
        f"https://www.google.com/s2/favicons?domain={host}&sz=64",
    ]


def icon(data):
    """The image in `data` scaled to SIZE, or None if it isn't one."""
    image = Image.open(io.BytesIO(data))
    if image.format == "ICO":
        image.size = max(image.ico.sizes())
    image.load()
    image = image.convert("RGBA")
    if image.width < 16 or image.height < 16:
        return None
    # Square it on a transparent canvas, then scale.
    side = max(image.size)
    square = Image.new("RGBA", (side, side))
    square.paste(image, ((side - image.width) // 2, (side - image.height) // 2))
    image = square.resize((SIZE, SIZE), Image.LANCZOS)
    if dark_on_clear(image):
        tile = Image.new("RGBA", image.size)
        ImageDraw.Draw(tile).rounded_rectangle(
            (0, 0, SIZE - 1, SIZE - 1), radius=SIZE // 5, fill="white"
        )
        image = Image.alpha_composite(tile, image)
    return image


def dark_on_clear(image):
    """Whether `image` is mostly near-black on a mostly clear ground."""
    pixels = list(image.get_flattened_data())
    opaque = [(r, g, b) for r, g, b, a in pixels if a >= 128]
    clear = 1 - len(opaque) / len(pixels)
    dark = sum(max(rgb) < 70 for rgb in opaque) / max(len(opaque), 1)
    return clear > 0.2 and dark > 0.4


def fetch(url):
    try:
        page_url, page = get(url)
        urls = candidates(page_url, page)
    except Exception:
        urls = fallbacks(url)
    for candidate in urls:
        try:
            _, data = get(candidate)
            image = icon(data)
        except Exception:
            continue
        if image is not None:
            return image, candidate
    raise ValueError("no usable icon")


def initials_svg(key, name):
    label = INITIALS.get(key, name[0].upper())
    font = {1: 13, 2: 10.5}.get(len(label), 8)
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24">'
        '<rect x="1.25" y="1.25" width="21.5" height="21.5" rx="5" fill="none" '
        'stroke="#888" stroke-width="2.5"/>'
        '<text x="12" y="12" dy="0.36em" text-anchor="middle" '
        'font-family="system-ui, sans-serif" font-weight="700" '
        f'font-size="{font}" fill="#888">{label}</text></svg>\n'
    )


def main():
    only = set(sys.argv[1:])
    OUT.mkdir(parents=True, exist_ok=True)
    failed = []
    for key, name, url in sites():
        if only and key not in only:
            continue
        png, svg = OUT / f"{key}.png", OUT / f"{key}.svg"
        try:
            image, source = fetch(url)
        except Exception as error:
            print(f"{key}: {error}; using initials", file=sys.stderr)
            png.unlink(missing_ok=True)
            svg.write_text(initials_svg(key, name))
            failed.append(key)
            continue
        image.save(png, optimize=True)
        svg.unlink(missing_ok=True)
        print(f"{key}: {source}")
    if failed:
        print(f"\ninitials for: {' '.join(failed)}", file=sys.stderr)


if __name__ == "__main__":
    main()
