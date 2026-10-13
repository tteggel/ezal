# design/

Hardware design assets for the ezal Pico 2 W ↔ Yaesu G-5500 interface board.
[HARDWARE.md](../docs/HARDWARE.md) describes the circuit and the G-5500
connector.

| file | purpose |
|------|---------|
| [`circuit.py`](circuit.py)   | [Schemdraw](https://schemdraw.readthedocs.io/) source for the schematic; edit this when the circuit changes |
| [`circuit.svg`](circuit.svg) | Rendered schematic, viewable directly on GitHub |

Re-render from the dev shell, which provides Python with schemdraw:

```bash
python3 design/circuit.py
```

This writes `circuit.svg` beside the source. Commit the source and the
rendered schematic together, so a clone shows the current circuit without
running a renderer.
