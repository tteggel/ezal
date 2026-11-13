#!/usr/bin/env python3
"""Assemble the standalone walkthrough. Python standard library only.

    build.py                  write index.html
    build.py orbit -o FILE    preview only the named section pairs in FILE

Sections quote the repository's source rather than retyping it. A fragment says
which lines it wants by the text they contain:

    <figure class="code" data-src="crates/ezal-core/src/orbit.rs" data-comments="omit">
      <figcaption>Rotate into Earth-fixed axes, then subtract the station.</figcaption>
      <pre data-from="let x = c * teme_km[0]" data-to="let z = teme_km[2]"></pre>
      <ol><li data-at="let z =">Earth turns about its z axis, so z is unchanged.</li></ol>
    </figure>

Each empty <pre> becomes the lines from the first containing `data-from` to the
next containing `data-to` (one line without it). The first `data-from` in a
figure must match exactly one line of the file; later ones search onward from
the previous excerpt, so several <pre>s show one function with gaps; two with
no code between them join into one block. Each <li data-at> numbers the first excerpted line containing its text. An anchor
that begins with ^ must start its line, indentation included, so
data-to="^    }" ends on a function's own closing brace rather than an inner
one. The caption gains a link to those lines on GitHub. `data-comments="omit"`
drops lines that are only a comment. An anchor that no longer matches fails the
build, so an excerpt cannot silently drift from the code.
"""
import argparse
import html
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
BLOB = 'https://github.com/tteggel/ezal/blob/main/'
PAIRS = ['board', 'control', 'orbit', 'tracking']
CHAPTERS = [('boot', 'Board bring-up'), ('feedback', 'Position feedback'), ('permission', 'Permission to ask'), ('actuation', 'Output activation'), ('orbit', 'Find the satellite'), ('tracking', 'Follow a pass'), ('bench', 'Trace & verify')]

FIGURE = re.compile(r'<figure class="code"([^>]*)>(.*?)</figure>', re.S)
ATTR = re.compile(r'data-([\w-]+)="([^"]*)"')
PRE = re.compile(r'<pre data-from="([^"]*)"(?: data-to="([^"]*)")?></pre>')
NOTE = re.compile(r'<li data-at="([^"]*)">')
RUST = re.compile(
    r'(?P<c>//.*)|(?P<s>b?"(?:\\.|[^"\\])*")'
    r'|(?P<k>\b(?:as|async|await|const|else|enum|fn|for|if|impl|in|let|loop|match|mod|move|mut|pub|return|self|Self|static|struct|trait|type|use|where|while)\b)'
    r'|(?P<n>\b(?:0x[\da-fA-F_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:e[+-]?\d+)?)(?:_?[fiu](?:8|16|32|64|128|size))?\b)')


def highlight(code, rust):
    """Escape one source line; mark Rust comments, strings, keywords, numbers."""
    if not rust:
        return html.escape(code, quote=False)
    out, pos = [], 0
    for token in RUST.finditer(code):
        out.append(html.escape(code[pos:token.start()], quote=False))
        out.append(f'<span class="t{token.lastgroup}">{html.escape(token[0], quote=False)}</span>')
        pos = token.end()
    return ''.join(out) + html.escape(code[pos:], quote=False)


def excerpt(figure):
    """Fill one <figure class="code"> from the source file it names."""
    attrs = {key: html.unescape(value) for key, value in ATTR.findall(figure[1])}
    src, body = attrs['src'], figure[2]
    lines = (REPO / src).read_text().splitlines()
    omit = attrs.get('comments') == 'omit'

    def has(i, text):
        return lines[i].startswith(text[1:]) if text.startswith('^') else text in lines[i]

    def find(text, start, unique=False):
        hits = [i for i in range(start, len(lines)) if has(i, text)]
        if not hits or unique and len(hits) > 1:
            raise SystemExit(f'{src}: anchor {text!r} matches lines {[i + 1 for i in hits]} (searched from line {start + 1})')
        return hits[0]

    def hidden(i):
        return omit and lines[i].lstrip().startswith('//')

    blocks, cursor = [], 0  # the lines shown by each <pre>; none if it joined the block before
    for start, end in PRE.findall(body):
        first = find(html.unescape(start), cursor, unique=not blocks)
        last = find(html.unescape(end), first) if end else first
        kept = [i for i in range(first, last + 1) if not hidden(i)]
        # Omitted comments can leave blank lines stranded at an edge or doubled.
        kept = [i for n, i in enumerate(kept) if lines[i].strip() or 0 < n < len(kept) - 1 and lines[kept[n - 1]].strip()]
        gap = range(cursor, first)
        if blocks and all(hidden(i) or not lines[i].strip() for i in gap):
            # No code was skipped, so continue the previous block: the gap drawn
            # between two blocks always stands for code left out.
            previous = next(block for block in reversed(blocks) if block)
            previous += [i for i in gap if not lines[i].strip()][:1] + kept
            kept = []
        blocks.append(kept)
        cursor = last + 1
    if not blocks:
        raise SystemExit(f'{src}: a code figure needs at least one <pre data-from="…"></pre>')
    shown = [i for kept in blocks for i in kept]
    indent = min(len(lines[i]) - len(lines[i].lstrip()) for i in shown if lines[i].strip())

    marks = {}
    for number, anchor in enumerate(NOTE.findall(body), 1):
        text = html.unescape(anchor)
        hit = next((i for i in shown if has(i, text)), None)
        if hit is None:
            raise SystemExit(f'{src}: note anchor {text!r} is not in the excerpted lines')
        marks.setdefault(hit, []).append(number)

    def render(i):
        notes = ''.join(f'<b class="mark" aria-hidden="true">{n}</b>' for n in marks.get(i, []))
        return highlight(lines[i][indent:], src.endswith('.rs')) + notes

    filled = iter('<pre tabindex="0"><code>' + '\n'.join(map(render, kept)) + '</code></pre>' if kept else '' for kept in blocks)
    body = NOTE.sub('<li>', PRE.sub(lambda _: next(filled), body))

    label = re.sub(r'^crates/([^/]+)/src/', r'\1 · ', src)
    span = f'line {shown[0] + 1}' if len(shown) == 1 else f'lines {shown[0] + 1}–{shown[-1] + 1}'
    link = f'<a class="src" href="{BLOB}{src}#L{shown[0] + 1}-L{shown[-1] + 1}">{label}, {span}{", comments omitted" if omit else ""}</a>'
    if '</figcaption>' in body:
        body = body.replace('</figcaption>', f' {link}</figcaption>', 1)
    else:
        body = f'<figcaption>{link}</figcaption>{body}'
    return f'<figure class="code">{body}</figure>'


def check_links(page):
    """Every source link must name an existing file and lines inside it."""
    for path, first, last in re.findall(re.escape(BLOB) + r'([^"?#]+)(?:\?plain=1)?(?:#L(\d+)(?:-L(\d+))?)?"', page):
        source = REPO / path
        if not source.exists():
            raise SystemExit(f'source link to missing path: {path}')
        if first and int(last or first) > len(source.read_text().splitlines()):
            raise SystemExit(f'source link beyond the end of {path}: L{first}-L{last}')


def main():
    args = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    args.add_argument('pairs', nargs='*', help=f'only these section pairs, for a preview (default: {" ".join(PAIRS)})')
    args.add_argument('-o', '--out', type=Path, default=ROOT / 'index.html')
    args = args.parse_args()
    if unknown := set(args.pairs) - set(PAIRS):
        raise SystemExit(f'unknown section pair: {", ".join(sorted(unknown))}')

    read = lambda name: (ROOT / name).read_text()
    pairs = [pair for pair in PAIRS if pair in args.pairs or not args.pairs]
    parts = ''.join(read(f'sections/{pair}.html') for pair in pairs)
    scripts = read('intro.js') + ''.join(read(f'sections/{pair}.js') for pair in pairs)
    nav = ''.join(f'<li><a href="#{key}">{label}</a></li>' for key, label in CHAPTERS)
    nav_script = """(() => { const links=[...document.querySelectorAll('.rail ol a')]; const observer=new IntersectionObserver(entries=>{for(const e of entries){if(e.isIntersecting){for(const a of links)a.removeAttribute('aria-current');const a=links.find(a=>a.hash==='#'+e.target.id);if(a)a.setAttribute('aria-current','true');}}},{rootMargin:'-5% 0px -65% 0px',threshold:0});document.querySelectorAll('.chapter').forEach(s=>observer.observe(s));})();"""
    page = f'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="description" content="An illustrated, interactive walkthrough of ezal firmware, from Pico 2 W startup through position feedback and output activation to following a satellite across the sky."><title>From power-on to pointing · ezal</title><style>{read('style.css')}</style></head>
<body><a class="skip" href="#content">Skip to walkthrough</a><div class="layout"><nav class="rail" aria-label="Walkthrough chapters"><a class="brand" href="#start">ez/al</a><small>A board. Four outputs.<br>One path to movement.</small><ol>{nav}</ol><div class="extras"><a href="#questions">Questions & concerns</a><a href="#reading">Further reading</a><a href="https://github.com/tteggel/ezal">Repository on GitHub</a></div></nav><main id="content">{read('intro.html')}{parts}{read('outro.html')}</main></div><script>{scripts}\n{nav_script}</script></body></html>'''
    page = FIGURE.sub(excerpt, page)
    # GitHub's plain Markdown view preserves line anchors used by the source notes.
    page = re.sub(r'(https://github\.com/tteggel/ezal/blob/main/[^"?#]+\.md)#L', r'\1?plain=1#L', page)
    check_links(page)
    args.out.write_text(page)
    print(f'Built {args.out} ({len(page):,} characters; sections: {", ".join(pairs)})')


if __name__ == '__main__':
    main()
