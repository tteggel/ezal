# ezal interactive walkthrough

Read it at <https://tteggel.github.io/ezal/>.

Seven chapters follow one request to move, from power reaching the Pico to a motor turning, and then let a satellite make the requests. The figures are teaching models in JavaScript: they take their constants and rules from the Rust source, but they do not run the firmware and cannot reach hardware. The code excerpts are the firmware itself, copied out of the repository each time the page is built.

The explanation was written against `main` at `79046c1` (5 November 2026). Its source links follow live `main`.

## Building

The page is one self-contained file, `index.html`: no CDN, package installation, server or board. It is built, not committed:

```sh
python3 docs/walkthrough/build.py
```

Open the resulting `docs/walkthrough/index.html` in a browser. The [Walkthrough workflow](../../.github/workflows/walkthrough.yml) runs the same build on every pull request and publishes it to GitHub Pages from `main`.

The build stops if the text that anchors a source excerpt no longer matches the code, or if a source link points at a missing file or past its end. That is the cue to bring the explanation up to date. The docstring at the top of `build.py` describes the excerpt syntax and how to preview one section pair alone.

## Editing

The page is assembled from `intro.html` and `intro.js`, four section pairs under `sections/` (`board`, `control`, `orbit`, `tracking`: a fragment and its script), `outro.html` and `style.css`.

`BRIEF.md` is the writer's guide: the narrative, the standard the page is held to, and its conventions. `CONCERNS.md` is the log of questions and known limits met while explaining the code; the page's last section is a digest of it.
