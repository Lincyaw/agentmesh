# Agentmesh — One invite. Every agent.

This is a Presmium lecture: the document is `lecture.json`, which `presmium` exports to a web page, slides, and a Notebook. Change the documents and the code, never the output in `dist/`.

Run `presmium skills install .` to install the ignored `.agents/skills` bundle before editing.
The animation can also be maintained in explicit CLI compiler-only workflows without a document authoring host.

Skills (`.agents/skills`, self-contained):

- `presmium-author`: writing, presenting, and producing documents: how to write the reading and slide versions, pages, content blocks, steps and scripts, voiceover and lecture videos, the classroom, checks, exports, and publishing.
- `presmium-animate`: animation: the way to show each teaching moment, scenes written in code (with a gallery of effects for each animation library), and putting them on the voiceover.

When native MCP `presmium` is connected (`.mcp.json`, command `presmium mcp`), use its author actions directly for edits, checks and captures as “Work through the tools” in `presmium-author` says. The default command starts or reuses the workspace service before initialization. Use this project as the working directory or pass an absolute workspace or document path. Edit `lecture.json` directly only with explicit `--compiler-only` mode or without an MCP host. After an interruption, inspect saved history or task state before repeating a write with an unknown outcome. Automatic startup preserves approvals for paid operations, source-file writes and browser control.

After a change, run `presmium check` and fix what it reports; `presmium build` exports the web page, the slides, and the Notebook.
