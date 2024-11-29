use libc::{PROT_EXEC, PROT_READ, PROT_WRITE};
use std::ptr;

#[cfg(target_arch = "aarch64")]
// Magic value: code len (8) + pointer length (8)
pub const BACKUP_LEN: usize = 16;
#[cfg(target_arch = "aarch64")]
pub unsafe fn hook(target: *mut u8, hook_fn: *const u8) {
    const CODE: [u8; 8] = [
        0x43, 0x00, 0x00, 0x58, // ldr x3, +0x8
        0x60, 0x00, 0x1f, 0xd6, // br x3
    ];
    const CODE_USIZE: usize = usize::from_ne_bytes(CODE);
    ptr::write(target as *mut [usize; 2], [CODE_USIZE, hook_fn as usize]);
}
#[cfg(target_arch = "arm")]
fn is_thumb(addr: u32) -> bool {
    addr & 1 != 0
}
#[cfg(target_arch = "arm")]
fn clear_thumb_bit(addr: u32) -> u32 {
    addr & 0xfffffffe
}
#[cfg(target_arch = "arm")]
fn is_aligned(addr: u32) -> bool {
    addr % 4 == 0
}
#[cfg(target_arch = "arm")]
// Magic value: code len (4) + pointer length(4) + align(1)
pub const BACKUP_LEN: usize = 9;
#[cfg(target_arch = "arm")]
pub unsafe fn hook(target: *mut u8, hook_fn: *const u8) {
    let target_addr = target as u32;
    let hook_fn = hook_fn as u32;
    if is_thumb(target_addr) {
        // asm: nop
        const THUMB_NOOP: u16 = 0xbf00;
        // asm: ldr.w pc, [pc]
        const LDR_PC_PC: [u16; 2] = [0xf8df, 0xf000];
        let target_addr = clear_thumb_bit(target_addr);
        let mut target = target_addr as *mut u16;
        if !is_aligned(target_addr) {
            *target = THUMB_NOOP;
            target = target.offset(1);
        }
        *(target as *mut [u16; 2]) = LDR_PC_PC;
        *(target.offset(2) as *mut u32) = hook_fn;
    } else {
        // asm: ldr pc, [pc, -4]
        const CODE: u32 = 0xe51ff004;
        let arm_insns = target_addr as *mut u32;
        *arm_insns = CODE;
        *arm_insns.offset(1) = hook_fn;
    }
}

#[cfg(target_arch = "x86_64")]
pub const BACKUP_LEN: usize = 12;
#[cfg(target_arch = "x86_64")]
pub unsafe fn hook(target: *mut u8, hook_fn: *const u8) {
    let ptr = hook_fn as u64;
    let mut code: [u8; 12] = [
        0x48, 0xb8, // movabs rax, <ptr>
        0, 0, 0, 0, 0, 0, 0, 0, // <ptr>, we copy the real ptr later
        0xff, 0xe0, // jmp rax
    ];
    code[2..10].copy_from_slice(&ptr.to_ne_bytes());
    (target as *mut [u8; 12]).write(code);
}

#[cfg(target_arch = "x86")]
// Magic value: mov length (1) + pointer length (4) + jmp len (2)
pub const BACKUP_LEN: usize = 7;
#[cfg(target_arch = "x86")]
pub unsafe fn hook(target: *mut u8, hook_fn: *const u8) {
    let ptr = hook_fn as u32;
    let mut code: [u8; 7] = [
        0xB8, // mov eax, <ptr>
        0, 0, 0, 0, // <ptr>, we copy the real ptr later
        0xFF, 0xE0, // jmp eax
    ];
    code[1..5].copy_from_slice(&ptr.to_ne_bytes());
    (target as *mut [u8; 7]).write(code);
}

pub unsafe fn setup_hook(orig_fn: *mut u8, hook_fn: *const u8) -> [u8; BACKUP_LEN] {
    let pa_addr = page_align_addr(orig_fn) as *mut _;
    libc::mprotect(
        pa_addr,
        page_size::get(),
        PROT_READ | PROT_WRITE | PROT_EXEC,
    );
    #[cfg(not(target_arch = "arm"))]
    let offset_fn = orig_fn;
    #[cfg(target_arch = "arm")]
    let offset_fn = orig_fn.offset(-1);
    let result = ptr::read_unaligned(offset_fn as *mut [u8; BACKUP_LEN]);
    hook(orig_fn, hook_fn);
    // We do not clean instruction cache because mprotect does that for us i guess
    libc::mprotect(pa_addr, page_size::get(), PROT_READ | PROT_EXEC);
    result
}

pub unsafe fn unsetup_hook(orig_fn: *mut u8, orig_code: [u8; BACKUP_LEN]) {
    let pa_addr = page_align_addr(orig_fn) as *mut _;
    libc::mprotect(
        pa_addr,
        page_size::get(),
        PROT_READ | PROT_WRITE | PROT_EXEC,
    );
    #[cfg(target_arch = "arm")]
    let orig_fn = orig_fn.offset(-1);
    ptr::write_unaligned(orig_fn as *mut [u8; BACKUP_LEN], orig_code);
    // We do not clean instruction cache because mprotect does that for us i guess
    libc::mprotect(pa_addr, page_size::get(), PROT_READ | PROT_EXEC);
}
fn page_align_addr(addr: *mut u8) -> *mut u8 {
    (addr as usize & !(page_size::get() - 1)) as *mut u8
}
