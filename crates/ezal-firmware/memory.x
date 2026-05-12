/* ────────────────────────────────────────────────────────────────────────
 * Memory layout for the Raspberry Pi Pico 2 (RP2350 + 4 MiB QSPI flash)
 * ────────────────────────────────────────────────────────────────────────
 *
 * The linker reads this file (via `INCLUDE memory.x` in cortex-m-rt's
 * `link.x`) to lay out our binary's sections in the physical address map
 * of the chip.
 *
 * Address map highlights for the Pico 2:
 *   0x10000000 — start of the XIP-mapped QSPI flash (4 MiB on the Pico 2)
 *   0x20000000 — start of on-chip SRAM (520 KiB total)
 *
 * Note: unlike the RP2040, the RP2350 does **not** require a hand-written
 * 2nd-stage bootloader at the start of flash. Instead the boot ROM scans
 * flash for an "image definition" block, which we provide from Rust in
 * `src/main.rs` via `embassy_rp::block::ImageDef`. That block ends up in
 * the `.start_block` section, which cortex-m-rt's link script places at
 * the very start of FLASH.
 */

MEMORY {
    /* External QSPI flash. The Pico 2 module ships with a Winbond 4 MiB
     * part. If you target an MCU with a different flash size, shrink this
     * region to match — overrunning would mean writing into nothing.
     */
    FLASH : ORIGIN = 0x10000000, LENGTH = 4096K

    /* On-chip SRAM. The RP2350 has 520 KiB total split across multiple
     * banks; for our purposes we present it to the linker as a single
     * flat region. cortex-m-rt will place the stack at the top
     * (_stack_start = ORIGIN(RAM) + LENGTH(RAM)).
     */
    RAM   : ORIGIN = 0x20000000, LENGTH = 520K
}
