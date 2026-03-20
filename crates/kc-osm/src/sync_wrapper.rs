/// Wrapper to satisfy `Send + Sync` bounds for `OnceLock` on types containing COM pointers.
///
/// # Safety
/// This is **only** safe when all access occurs on a single thread. Dark Engine OSMs are
/// single-threaded — these globals are only accessed from the engine's script thread.
/// Do not use this in multi-threaded contexts.
pub struct UnsafeSendSync<T>(pub T);
unsafe impl<T> Send for UnsafeSendSync<T> {}
unsafe impl<T> Sync for UnsafeSendSync<T> {}
