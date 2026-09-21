/* Development layout: 4 MiB external flash, 512 KiB main SRAM. */
MEMORY {
    FLASH : ORIGIN = 0x10000000, LENGTH = 4M - 8K
    RAM   : ORIGIN = 0x20000000, LENGTH = 512K
}
SECTIONS {
    .start_block : ALIGN(4) {
        KEEP(*(.start_block));
    } > FLASH
} INSERT AFTER .vector_table;
_stext = ADDR(.start_block) + SIZEOF(.start_block);
/* RP2350 ROM scans the first 4 KiB for the image definition. */
ASSERT(SIZEOF(.vector_table) + SIZEOF(.start_block) <= 4096, "Boot header exceeds ROM search area");
