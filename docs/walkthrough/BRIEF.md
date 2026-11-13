# Walkthrough writer's guide

The walkthrough explains ezal to someone who has never programmed a microcontroller. It describes `main` at `79046c1` (surveyed 5 November 2026, rewritten 13 November 2026). Read this before changing a chapter.

## The story

ezal runs on a Pico 2 W and points a Yaesu G-5500 rotator: four transistor switches press the rotator's direction inputs, and an ADS1015 reads two position voltages back. The page asks one question, *what has to be true before pin GP10 goes high?*, and answers it in the order the firmware does. Then it asks what changes when a satellite, not a finger, makes the request.

| Chapter | Section id | Files in `sections/` | Its question |
| --- | --- | --- | --- |
| 1 | `boot` | `board` | Why does a perfect start leave the motors still? |
| 2 | `feedback` | `board` | What turns two voltages into a position worth trusting? |
| 3 | `permission` | `control` | What must the board know before it takes a button press seriously? |
| 4 | `actuation` | `control` | Will the pin now do what the request says? |
| 5 | `orbit` | `orbit` | How does a board that cannot see the sky know where a satellite will be? |
| 6 | `tracking` | `tracking` | What has to happen before the pins can follow that direction with nobody holding a button? |
| 7 | `bench` | `tracking` | What would convince you it works, and what is still unproven? |

Each chapter opens with its question and ends by handing over to the next. A chapter refers back by number to what an earlier one explained; it does not explain it again. Chapter 7 is the one home for what has not been measured and for the table of unrelated 500 ms rules.

## The bar

The standard is Bartosz Ciechanowski's explainers (ciechanow.ski: "GPS", "Mechanical Watch").

1. **Show the thing happening, then name it.** The reader operates a figure and sees the mechanism before it gets a name. Introduce a term only if the reader needs it again; otherwise say it in plain words and move on. A paragraph that is a chain of bolded definitions has failed.
2. **Figures show the thing itself**: the dish, the wire, the voltage, the timeline. A row of boxes and arrows that restates the prose is not a figure. One idea per figure, placed where the idea is introduced, each building on the last. Prefer something to drag over a menu that swaps paragraphs.
3. **Colour ties words to pictures.** The same thing has the same colour in every figure and in the prose that describes it.
4. **Plain, confident, economical prose.** Short sentences, "you" and "we", concrete numbers, worked examples. Everyday verbs, not contract words ("admit", "authorize", "inhibit", "service"), unless the code's own name is about to appear in an excerpt.
5. **Say each limitation once**, where it matters. What a figure simplifies takes a sentence or two in its caption. The rest belongs in the concerns log. Two must never be lost: a pin can hold its level through a watchdog reset, and the board and rotator have not been commissioned.
6. **The code appears in place.** Once the reader understands a mechanism, show the few lines of source that implement it, with short notes tying lines to what they just watched. Two to five excerpts a chapter, each about 25 lines at most.

## Delivery

One offline HTML file, `index.html`, with its CSS, JavaScript and SVG embedded. It is built, not committed: CI publishes it to GitHub Pages from `main`. No frameworks, no CDN, nothing fetched at run time, nothing that contacts a board. `build.py` assembles it and uses only Python's standard library. Do not modify the firmware for the page, and do not read `.env`.

The page is light only: white paper, navy #18354a, cobalt #245bce, teal #087e83, pale blue #edf4fa, amber #925100, text #20323e, in a humanist sans (Trebuchet MS, Segoe UI) at a reading width of about 70 characters, with a chapter rail beside it. Every control is a native, labelled element that works from the keyboard and shows its focus. Nothing moves until the reader acts (a Play button counts), and `prefers-reduced-motion` is honoured. At 320 px wide the page must not scroll sideways; a wide drawing may scroll inside its own labelled region.

## Conventions

- **Colour vocabulary** (classes in `style.css`; the same hex values inside canvases): azimuth `.c-az` #245bce, elevation `.c-el` #087e83, the satellite or any moving target `.c-sat` #9b3fb5, a limit, deadline or expiry `.c-warn` #925100, allowed `.c-ok` #11643e, refused or stopped `.c-stop` #a33317.
- **Operable figures** are `<figure class="live" id="PREFIX-…">`: the drawing (inline SVG or `<canvas>`), then `<div class="controls">`, then a `<figcaption>`. No heading, no box. Give a canvas `role="img"` and an `aria-label`, and put the numbers that matter in an `aria-live` paragraph. A slider's `<label>` needs `for=`; wrapping the input is not enough when the label also holds an `<output>`.
- **Drawing style**: flat schematic line art. Azimuth is a compass dial seen from above; elevation is a quarter arc seen from the side. Canvases scale with `devicePixelRatio` and redraw on input and on resize.
- **JavaScript**: one strict-mode IIFE per section pair, with its own id prefix (`board-`, `control-`, `orbit-`, `tb-`), no globals, no libraries. Styles that belong to one pair go in a `<style>` block at the top of its fragment.
- **Figures tell the truth about the code.** Take every constant and rule from the source and say in the caption which function the figure mirrors. Where it is feasible, check the figure's model against the Rust: a throwaway cargo project outside the repository that depends on `ezal-core` by path, fed the same inputs as the JavaScript under node.
- **Source excerpts** are filled in by the build from text anchors; the docstring at the top of `build.py` is the reference. `data-comments="omit"` suits an excerpt whose notes explain the lines better for a beginner than the source's own comments would. Hand-written `#Lx-Ly` links are checked only for an existing file and line, so re-read any you keep.
- **Preview one pair** with `python3 docs/walkthrough/build.py PAIR -o SOMEWHERE/preview.html`.
- **Concerns log**: keep `CONCERNS.md` current while writing, not afterwards. Add an entry whenever the source raises a question, shows a limit, or contradicts what the page said. Each entry has an id (`B`, `C`, `O` or `T` for its section pair, and a number), a source location, a status (question, confirmed limitation, potential defect, resolved), the consequence, and what would settle it. The page's last section (`outro.html`) is a digest of the log and needs updating with it.

## Before calling a change done

Look at the chapter rendered at 1440×1000 and 390×844, and check 320 px for sideways scroll. Operate every control with the pointer and with the keyboard. Confirm the browser console is clean. Check every number and behaviour stated against the source. Then read the chapter once as someone who has never touched a microcontroller.
