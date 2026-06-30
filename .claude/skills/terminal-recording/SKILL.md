---
name: terminal-recording
description: Record a terminal session headlessly with VHS (charmbracelet/vhs) and turn it into a small gif/mp4/webm. Use when you need to attach a terminal recording to a PR — to demo a CLI or TUI change, or to reproduce an issue visually — without a human driving a live terminal.
---

# Recording the terminal with VHS

VHS renders a terminal session from a plain-text `.tape` script — no human, no
live terminal. It runs the commands in a headless pty (via `ttyd`) and encodes
the result with `ffmpeg`. Perfect for CI/agent contexts.

`vhs`, `ttyd`, and `ffmpeg` are provided by the dev shell — run everything inside
`nix develop` (or the direnv shell). Check it works: `vhs --version`.

## Write a `.tape`

A tape is a sequence of commands, one per line:

```tape
Output demo.gif          # also .mp4 / .webm — pick one or several Output lines

Set FontSize 14
Set Width 1000           # pixels; smaller = smaller file
Set Height 600
Set Padding 10

Type "cargo run -- --help"   # types the text into the shell
Enter                        # presses Enter (runs it)
Sleep 3s                     # hold so viewers can read; also lets the cmd finish

Hide                         # stop recording frames...
Type "setup-step" Enter      # ...do off-camera setup...
Show                         # ...resume recording
```

Key commands: `Output`, `Set` (FontSize/Width/Height/Padding/Theme/TypingSpeed),
`Type`, `Enter`, `Sleep <n>s`, `Ctrl+C`, `Hide`/`Show`. Full reference:
`vhs manual`.

## Render it (headless)

```bash
nix develop --command vhs demo.tape   # writes the Output file(s)
```

That's the whole loop — no display, no interaction.

## Recording a TUI

A TUI records fine: VHS runs it in a real pty, so the alternate screen is
captured. Launch it, `Sleep` long enough for it to render, then **quit with its
own quit key** so the recording terminates — e.g. for `yeschef tui`:

```tape
Type "cargo run --quiet -- tui"
Enter
Sleep 4s
Type "q"        # the TUI's quit key — otherwise the tape hangs until timeout
Sleep 1s
```

Pre-build first (`cargo build`) and use `--quiet` so the recording isn't a wall
of compiler output. Make sure there's something to show (an existing ticket, or
accept the empty-state render).

## Keep it small

- Prefer a gif for inline PR embeds; reach for mp4/webm only for long/high-motion clips.
- Keep `Width`/`Height`/`FontSize` modest and `Sleep`s tight — a static TUI gif is
  tens of KB.
- Verify before committing: `ls -la demo.gif`, and spot-check a frame with
  `ffmpeg -i demo.gif -vf "select=eq(n\,100)" -vframes 1 frame.png` then view it.

## Attach to a PR

**Preferred — commit, then delete (keeps the repo tree clean).** A pushed blob
stays reachable by commit SHA even after you remove it from `HEAD`, so you can
embed it without leaving the image in the final tree:

```bash
git add docs/demo.gif && git commit -m "tmp: demo gif" && git push
SHA=$(git rev-parse HEAD)                       # pin the URL to this commit
git rm docs/demo.gif && git commit -m "rm demo gif" && git push
# Embed in the PR body with the pinned-SHA raw URL (survives the deletion):
echo "![demo](https://github.com/<owner>/<repo>/raw/$SHA/docs/demo.gif)"
```

The gif renders in the PR but never lands in the merged tree — no repo
pollution. (A relative `![demo](docs/demo.gif)` would break here, since the file
is gone from `HEAD`; the absolute pinned-SHA URL is what makes it work.)

**Alternatives:**
- **Keep it in the repo:** commit the gif (e.g. `docs/demo.gif`) and embed with a
  relative `![demo](docs/demo.gif)`. Fine for a small, lasting demo.
- **Upload, never touch git:** drag-drop into the PR on GitHub, or
  `gh pr comment <n> --body '![demo](<uploaded-url>)'` after uploading.

Commit the `.tape` (it's tiny and reproducible) even when you keep the gif out of
the tree, so anyone can regenerate the recording.
