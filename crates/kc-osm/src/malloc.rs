use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::System::Com::IMalloc;

use crate::UnsafeSendSync;

static MALLOC: OnceLock<UnsafeSendSync<IMalloc>> = OnceLock::new();

fn malloc() -> &'static IMalloc {
    &MALLOC.get().expect("Malloc hasn't been initialised.").0
}

pub(crate) fn init(malloc: IMalloc) {
    MALLOC.set(UnsafeSendSync(malloc)).ok();
}

#[allow(dead_code)]
pub(crate) unsafe fn alloc(size: usize) -> *mut c_void {
    unsafe { malloc().Alloc(size) }
}

/// # Safety
///
/// `ptr` must point to memory allocated by `malloc::alloc` or Dark Engine
pub(crate) unsafe fn free(ptr: *const c_void) {
    unsafe { malloc().Free(Some(ptr)) };
}
