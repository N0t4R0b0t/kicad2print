#!/usr/bin/env python3
"""Record the README demo GIF for the unified build guide.

Drives the generated guide HTML through Playwright, captures a WebM,
and converts it to an optimized animated GIF for embedding in README.md.

Prerequisites:
    pip install playwright
    playwright install chromium
    # plus ffmpeg on PATH

Usage:
    python scripts/record-guide-demo.py [GUIDE_HTML] [OUTPUT_GIF]

Defaults assume the ps2-serial-mouse-adapter example:
    GUIDE_HTML  = ./output/ps2-serial-mouse-adapter_guide.html
    OUTPUT_GIF  = ./examples/ps2-serial-mouse-adapter/guide-demo.gif

Generate the guide first:
    kicad2print path/to/board.kicad_pcb --mode electrolysis
"""
import asyncio
import subprocess
import sys
import tempfile
from pathlib import Path

from playwright.async_api import async_playwright

DEFAULT_GUIDE = Path("output/ps2-serial-mouse-adapter_guide.html")
DEFAULT_GIF = Path("examples/ps2-serial-mouse-adapter/guide-demo.gif")
VIEWPORT = {"width": 1280, "height": 720}


async def record(guide_html: Path, work_dir: Path) -> Path:
    async with async_playwright() as p:
        browser = await p.chromium.launch(headless=True)
        context = await browser.new_context(
            viewport=VIEWPORT,
            record_video_dir=str(work_dir),
            record_video_size=VIEWPORT,
        )
        page = await context.new_page()
        await page.goto(f"file://{guide_html.absolute()}")
        await page.wait_for_selector(".tab-btn.active", timeout=5000)
        await page.wait_for_timeout(1500)

        # Walk a couple of assembly steps
        await page.evaluate("typeof aGoStep === 'function' && aGoStep(1)")
        await page.wait_for_timeout(1200)
        await page.evaluate("typeof aGoStep === 'function' && aGoStep(2)")
        await page.wait_for_timeout(1200)

        # Switch to continuity tab and walk through several nets
        await page.evaluate("switchTab('continuity')")
        await page.wait_for_timeout(1200)
        for i in range(6):
            stepped = await page.evaluate(
                f"(typeof cGoStep === 'function' && typeof C_STEPS !== 'undefined' && {i} < C_STEPS.length) "
                f"? (cGoStep({i}), true) : false"
            )
            if not stepped:
                break
            await page.wait_for_timeout(1400)

        # 3D preview as the closing beauty shot
        await page.evaluate("switchTab('3d')")
        await page.wait_for_timeout(3000)

        await context.close()
        await browser.close()

    videos = sorted(work_dir.glob("*.webm"))
    if not videos:
        raise RuntimeError("Playwright did not produce a video.")
    return videos[-1]


def convert_to_gif(webm: Path, out_gif: Path, *, width: int = 800, fps: int = 10, max_colors: int = 128) -> None:
    out_gif.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        palette = Path(tmp) / "palette.png"
        subprocess.run(
            [
                "ffmpeg", "-hide_banner", "-loglevel", "error",
                "-i", str(webm),
                "-vf", f"fps={fps},scale={width}:-1:flags=lanczos,palettegen=max_colors={max_colors}:stats_mode=diff",
                "-y", str(palette),
            ],
            check=True,
        )
        subprocess.run(
            [
                "ffmpeg", "-hide_banner", "-loglevel", "error",
                "-i", str(webm),
                "-i", str(palette),
                "-filter_complex",
                f"[0:v]fps={fps},scale={width}:-1:flags=lanczos[v];"
                "[v][1:v]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
                "-loop", "0",
                "-y", str(out_gif),
            ],
            check=True,
        )


def main() -> int:
    guide = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_GUIDE
    out_gif = Path(sys.argv[2]) if len(sys.argv) > 2 else DEFAULT_GIF

    if not guide.exists():
        print(f"Guide HTML not found: {guide}", file=sys.stderr)
        print("Generate it first with: kicad2print path/to/board.kicad_pcb --mode electrolysis", file=sys.stderr)
        return 1

    with tempfile.TemporaryDirectory() as tmp:
        work = Path(tmp)
        webm = asyncio.run(record(guide, work))
        convert_to_gif(webm, out_gif)

    size_kb = out_gif.stat().st_size // 1024
    print(f"Wrote {out_gif} ({size_kb} KB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
