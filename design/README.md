# design/

Hardware design assets for the ezal Pico 2 ↔ Yaesu G-5500 interface
board.

## Contents

| file | purpose |
|------|---------|
| [`pinout.md`](pinout.md)     | Pin assignments for the G-5500's 8-pin external-control DIN |
| [`circuit.py`](circuit.py)   | [Schemdraw](https://schemdraw.readthedocs.io/) source for the schematic — **edit this** when the circuit changes |
| [`circuit.svg`](circuit.svg) | Rendered schematic (vector) — viewable directly in GitHub |
| `circuit.png`                | Rendered schematic (raster) — same image, for tools that don't speak SVG |

## What the circuit does

* **Direction** — four Pico GPIOs drive four 2N3904 NPN switches. Each
  switch shorts one of the G-5500's direction inputs (pins 2/3/4/5) to
  ground, mimicking a press on the front-panel direction button.
* **Feedback** — an Adafruit ADS1015 I²C ADC reads the G-5500's
  azimuth and elevation feedback (pins 1, 6) through two resistor
  dividers that step the 2.0–4.5 V range down into the ADS1015's input
  window. The Pico talks to the ADS1015 over I²C0 (GP4/GP5).

## Re-rendering after changes

```bash
pip install --user schemdraw matplotlib
python3 circuit.py
```

This writes both `circuit.svg` (always) and `circuit.png` (when
matplotlib is present). Both are checked in so GitHub renders them
without anyone needing a Python environment to view.

## Conventions for adding more

* Anything `.py` is *source*; the rendered images are derived artefacts.
  Always commit them together so a clone shows the schematic without
  needing to run a renderer.
* If a future change adds a second board (e.g. a USB-serial interface
  for the host link), drop it in here as `circuit_<name>.py` /
  `circuit_<name>.svg` and update this README.
