# Launch animation

The canonical Presmium document is `lecture.json`.
The seekable implementation is `scenes/network.ts`.
`storyboard.md` records the visual argument and timing.

Use Node.js 22.19 or newer and Presmium 1.0.0-rc.117.

```sh
pnpm install
pnpm exec presmium check . --json
pnpm exec presmium build . --to slides
pnpm exec presmium-lecture export . --out dist/agentmesh.mp4 --size 1200x750 --fps 24 --contact-sheet
```

Install FFmpeg 5 or newer for MP4 export, or use a `.webm` output without FFmpeg.
All four steps explicitly use silence; exporting does not generate paid voiceover.
The published video and poster are copied to `../site/assets/` after reviewing the rendered result.
Generated files, operator credentials, and installed skill bundles are excluded from Git.
Run `presmium skills install .` to install the authoring guidance before editing.
