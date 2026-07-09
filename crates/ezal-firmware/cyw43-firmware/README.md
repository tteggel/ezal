# CYW43439 radio firmware blobs

The Pico 2 W's wireless is an Infineon **CYW43439**. It has no on-chip
non-volatile storage, so the host must upload three binary images to it at
every boot. `src/wifi.rs` `include_bytes!`s them straight into the ezal
firmware image (via `cyw43::aligned_bytes!`), so a single `.uf2`/elf is
wholly self-contained — no separate flashing step, and the UF2 drag-and-drop
path keeps working.

| file              | size   | what it is                                            |
|-------------------|-------:|-------------------------------------------------------|
| `43439A0.bin`     | 231 KB | WiFi MAC firmware, run from the CYW43439's RAM         |
| `43439A0_clm.bin` | 984 B  | Country Locale Matrix — regulatory channel/power table |
| `nvram_rp2040.bin`| 742 B  | board NVRAM: the CYW43439 module's calibration/config  |

`nvram_rp2040.bin` is named for the RP2040 Pico W but is a property of the
**CYW43439 module**, which is identical on the Pico W and Pico 2 W — the
embassy `blinky_wifi` example for the Pico 2 W uses this same file.

## Provenance

Vendored verbatim from the [embassy](https://github.com/embassy-rs/embassy)
project's `cyw43-firmware/` directory, at tag **`cyw43-v0.7.0`** (the tag
matching the `cyw43` crate version this firmware pins). embassy in turn takes
them from the upstream
[`georgerobotics/cyw43-driver`](https://github.com/georgerobotics/cyw43-driver/tree/main/firmware).

To refresh them, re-pull the same three files from the embassy tag that
matches the `cyw43` version in the workspace `Cargo.toml`, and check the
sizes above still line up (the driver hard-codes the RAM layout against them).

## Licence

These blobs are **not** covered by ezal's MIT/Apache-2.0 dual licence. They
are redistributed under the Infineon **Permissive Binary License**, the full
text of which sits alongside them in
[`LICENSE-permissive-binary-license-1.0.txt`](./LICENSE-permissive-binary-license-1.0.txt).
It permits redistribution in binary form; keep that licence file next to the
blobs if you copy them elsewhere.
