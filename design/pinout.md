# G-5500 external-control connector pinout

The Yaesu **G-5500** (and the -DC variant) exposes its external-control
interface as an 8-pin DIN connector on the back of the controller box.
Manufacturer reference: Yaesu G-5500 instruction manual, "External
remote control terminal".

| Pin | Direction | Function                                                     |
|-----|-----------|--------------------------------------------------------------|
|  1  | output    | Elevation feedback: 2.0–4.5 VDC corresponds to 0°–180°       |
|  2  | input     | Short to pin 8 → rotate **right** (clockwise azimuth)        |
|  3  | input     | Short to pin 8 → rotate **up**                               |
|  4  | input     | Short to pin 8 → rotate **left** (counter-clockwise azimuth) |
|  5  | input     | Short to pin 8 → rotate **down**                             |
|  6  | output    | Azimuth feedback: 2.0–4.5 VDC corresponds to 0°–450°         |
|  7  | output    | Auxiliary supply: 8–13 VDC at up to 100 mA                   |
|  8  | —         | Common ground                                                |

## Notes

* "Direction" is from the G-5500's perspective: *input* means the
  controller box drives that line internally and expects us to short it
  to ground; *output* means the box drives the line and we read it.
* Pins 2/3/4/5 are switch inputs. When shorted to pin 8 they behave
  exactly like pressing the corresponding direction button on the front
  panel — full motor speed in that direction. There is **no** analog
  speed control on this interface; everything is bang-bang.
* The 2.0–4.5 V feedback range is approximate. Each unit drifts slightly
  with temperature and age, so the firmware calibrates per-unit.
* Pin 7 (the auxiliary supply) is intended for small external loads such
  as a relay coil; it can power a Pico 2 module if regulated down, but
  the prototype currently powers the Pico from its own USB-C port.

See [circuit.svg](circuit.svg) for how the Pico 2 wires up to these pins.
