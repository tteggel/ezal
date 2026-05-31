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
 * the *first 4 KiB* of flash for an "image definition" block, which
 * `embassy-rp` provides for us (the `imagedef-secure-exe` feature emits a
 * `static IMAGE_DEF` into the `.start_block` section — see `src/main.rs`).
 *
 * The catch: cortex-m-rt's `link.x` knows nothing about `.start_block`.
 * On its own the linker treats `.start_block` as an orphan and dumps it
 * *after* `.text`/`.rodata`, ~11 KiB in — past the boot ROM's search
 * window — so the chip never finds a valid image and drops back into
 * BOOTSEL. The `SECTIONS … INSERT AFTER` block below is what pins
 * `.start_block` into the first 4 KiB (right after the vector table) and
 * shifts `.text` to begin after it. This mirrors embassy-rp's own
 * `examples/rp235x/memory.x`; without it, a flashed image will not boot.
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

/* ── RP2350 boot-image placement ───────────────────────────────────────
 *
 * Everything below pins the boot ROM's "block loop" sections into the
 * right places. It has to live here (rather than in a separate script)
 * because cortex-m-rt's `link.x` does an `INCLUDE memory.x`, which gives
 * these `INSERT AFTER` directives the `.vector_table` / `.text` / `.uninit`
 * sections to anchor against. Lifted verbatim (bar sizing) from embassy-rp.
 */

SECTIONS {
    /* ### Boot ROM info
     *
     * Goes right after .vector_table to keep it in the first 4K of flash,
     * where the boot ROM (and picotool) scan for the image-def block.
     */
    .start_block : ALIGN(4)
    {
        __start_block_addr = .;
        KEEP(*(.start_block));
        KEEP(*(.boot_info));
    } > FLASH
} INSERT AFTER .vector_table;

/* Move .text to start *after* the boot info, so they don't overlap. */
_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS {
    /* ### Picotool 'Binary Info' entries
     *
     * picotool reads this block (the header points at it) for metadata.
     * Empty today, but kept so `picotool info` works once we add entries.
     */
    .bi_entries : ALIGN(4)
    {
        __bi_entries_start = .;
        KEEP(*(.bi_entries));
        . = ALIGN(4);
        __bi_entries_end = .;
    } > FLASH
} INSERT AFTER .text;

SECTIONS {
    /* ### Boot ROM extra info
     *
     * Goes after everything else so it can hold a signature/hash for a
     * signed image. Empty for our unsigned secure-exe build.
     */
    .end_block : ALIGN(4)
    {
        __end_block_addr = .;
        KEEP(*(.end_block));
    } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);
