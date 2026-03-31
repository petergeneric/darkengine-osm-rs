//! Safe Rust API for DarkHooky bundle authors.
//!
//! Provides [`Api`] (safe wrapper around the raw C ABI), the [`Bundle`] trait,
//! and supporting types. Use [`export_bundle!`](crate::export_bundle) to generate
//! the required DLL exports from a `Bundle` implementation.

use std::ffi::{CStr, CString, c_void};

use crate::ffi::{self, DarkHookyApi, HookResult, HookyBundleInfo};

// ============================================================================
// Api — safe wrapper around the raw DarkHookyApi
// ============================================================================

/// Safe handle to the DarkHooky API.
///
/// Wraps the raw [`DarkHookyApi`](ffi::DarkHookyApi) pointer. Each method performs
/// null/availability checks internally, returning a sensible default if the function
/// isn't present (e.g. when running on an older host).
///
/// `Api` is `Copy` — pass it by value freely.
#[derive(Clone, Copy)]
pub struct Api {
    raw: *const DarkHookyApi,
}

// Safety: the DarkHookyApi is a process-lifetime static and its methods are thread-safe.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

impl Api {
    /// Create from a raw API pointer.
    ///
    /// Returns `None` if the pointer is null.
    ///
    /// # Safety
    /// `raw` must point to a valid `DarkHookyApi` that remains valid for the process lifetime.
    pub unsafe fn from_raw(raw: *const DarkHookyApi) -> Option<Self> {
        if raw.is_null() { None } else { Some(Self { raw }) }
    }

    /// Returns the underlying raw API pointer.
    pub fn as_raw(&self) -> *const DarkHookyApi {
        self.raw
    }

    fn api(&self) -> &DarkHookyApi {
        // Safety: from_raw guarantees non-null, and the contract guarantees process lifetime.
        unsafe { &*self.raw }
    }

    /// Returns the API version reported by the host.
    pub fn api_version(&self) -> u32 {
        self.api().api_version
    }

    /// Write a message to the host's log.
    pub fn log(&self, msg: &str) {
        if let Some(f) = self.api().log
            && let Ok(c) = CString::new(msg)
        {
            unsafe { f(c.as_ptr()) };
        }
    }

    /// Inject a keyboard scancode event.
    pub fn inject_key(&self, scancode: u16, key_up: bool) {
        if let Some(f) = self.api().inject_key {
            unsafe { f(scancode, key_up as i32, 0) };
        }
    }

    /// Inject a keyboard scancode event with the extended key flag.
    ///
    /// Extended keys include arrow keys, Insert, Delete, Home, End, Page Up/Down,
    /// and other keys that share scancodes with the numpad.
    pub fn inject_key_extended(&self, scancode: u16, key_up: bool) {
        if let Some(f) = self.api().inject_key {
            unsafe { f(scancode, key_up as i32, 1) };
        }
    }

    /// Inject relative mouse movement (dx/dy in pixels).
    pub fn inject_mouse_move(&self, dx: i32, dy: i32) {
        if let Some(f) = self.api().inject_mouse_move {
            unsafe { f(dx, dy) };
        }
    }

    /// Get the game directory path.
    pub fn game_directory(&self) -> Option<&str> {
        let f = self.api().get_game_directory?;
        let ptr = unsafe { f() };
        if ptr.is_null() {
            return None;
        }
        unsafe { CStr::from_ptr(ptr) }.to_str().ok()
    }

    /// Register a D3D9 EndScene callback. Lower `priority` values run first.
    ///
    /// # Safety
    /// `cb` and `user_data` must remain valid for the process lifetime.
    pub unsafe fn request_d3d9_endscene(&self, cb: ffi::EndSceneCallbackFn, user_data: *mut c_void, priority: i32) {
        if let Some(f) = self.api().request_d3d9_endscene {
            unsafe { f(cb, user_data, priority) };
        }
    }

    /// Get the current D3D9 device pointer, or null if not yet available.
    pub fn d3d9_device(&self) -> *mut c_void {
        match self.api().request_d3d9_device {
            Some(f) => unsafe { f() },
            None => std::ptr::null_mut(),
        }
    }

    /// Register an XInput pre-processing callback.
    ///
    /// # Safety
    /// `cb` and `user_data` must remain valid for the process lifetime.
    pub unsafe fn request_xinput_hook(&self, cb: ffi::XInputCallbackFn, user_data: *mut c_void) {
        if let Some(f) = self.api().request_xinput_hook {
            unsafe { f(cb, user_data) };
        }
    }

    /// Register a per-poll frame callback.
    ///
    /// # Safety
    /// `cb` and `user_data` must remain valid for the process lifetime.
    pub unsafe fn request_frame_callback(&self, cb: ffi::FrameCallbackFn, user_data: *mut c_void) {
        if let Some(f) = self.api().request_frame_callback {
            unsafe { f(cb, user_data) };
        }
    }

    /// Check if a bundle is loaded with at least the given version.
    pub fn is_bundle_loaded(&self, bundle_id: &str, min_version: u32) -> bool {
        let Some(f) = self.api().is_bundle_loaded else {
            return false;
        };
        let Ok(c) = CString::new(bundle_id) else {
            return false;
        };
        unsafe { f(c.as_ptr(), min_version) != 0 }
    }

    /// Send an inter-bundle message (raw — requires the sender's `HookyBundleInfo` pointer).
    ///
    /// Prefer [`BundleContext::send_message`] which fills in the sender automatically.
    ///
    /// # Safety
    /// `sender` must point to a valid `HookyBundleInfo` for the process lifetime.
    pub unsafe fn send_message_raw(&self, sender: *const HookyBundleInfo, target: &str, msg_type: u32, data: &[u8]) -> Result<(), HookResult> {
        let Some(f) = self.api().send_message else {
            return Err(HookResult::Unsupported);
        };
        let Ok(c) = CString::new(target) else {
            return Err(HookResult::TargetBundleNotFound);
        };
        let result = unsafe { f(sender, c.as_ptr(), msg_type, data.as_ptr() as *const c_void, data.len() as u32) };
        match HookResult::from_i32(result) {
            HookResult::Ok => Ok(()),
            err => Err(err),
        }
    }

    /// Register a DirectInput CreateDevice hook callback.
    ///
    /// # Safety
    /// `cb` and `user_data` must remain valid for the process lifetime.
    pub unsafe fn request_dinput_create_device_hook(&self, cb: ffi::DinputCreateDeviceCallbackFn, user_data: *mut c_void) {
        if let Some(f) = self.api().request_dinput_create_device_hook {
            unsafe { f(cb, user_data) };
        }
    }

    /// Register a DirectInput EnumDevices hook callback.
    ///
    /// # Safety
    /// `cb` and `user_data` must remain valid for the process lifetime.
    pub unsafe fn request_dinput_enum_devices_hook(&self, cb: ffi::DinputEnumDevicesCallbackFn, user_data: *mut c_void) {
        if let Some(f) = self.api().request_dinput_enum_devices_hook {
            unsafe { f(cb, user_data) };
        }
    }

    /// Register a named pointer identified by a binary GUID.
    ///
    /// Use [`StandardNamedPointers`](ffi::StandardNamedPointers) for well-known GUIDs.
    ///
    /// # Safety
    /// `ptr` must point to valid data that remains valid for the process lifetime.
    pub unsafe fn register_named_pointer(&self, guid: &ffi::NamedPointerGuid, ptr: *mut c_void) {
        if let Some(f) = self.api().register_named_pointer {
            unsafe { f(guid as *const ffi::NamedPointerGuid, ptr) };
        }
    }

    /// Inject a left mouse button press (down=true) or release (down=false).
    pub fn inject_mouse_click(&self, down: bool) {
        if let Some(f) = self.api().inject_mouse_click {
            unsafe { f(down as i32) };
        }
    }

    /// Set the mouse cursor to a 640x480 virtual coordinate (converted to screen coords).
    pub fn set_cursor_640(&self, x: i32, y: i32) {
        if let Some(f) = self.api().set_cursor_640 {
            unsafe { f(x, y) };
        }
    }

    /// Get elapsed milliseconds since the host DLL was loaded.
    pub fn elapsed_ms(&self) -> u64 {
        match self.api().get_elapsed_ms {
            Some(f) => unsafe { f() },
            None => 0,
        }
    }

    /// Find COM_QueryInterface by scanning the game exe's .text section.
    ///
    /// Returns `Some((fn_addr, aggregate_addr))` on success, or `None` if the
    /// signature was not found or the API is too old.
    pub fn find_com_query_interface(&self) -> Option<(u32, u32)> {
        let f = self.api().find_com_query_interface?;
        let mut fn_addr: u32 = 0;
        let mut aggregate_addr: u32 = 0;
        let ok = unsafe { f(&mut fn_addr, &mut aggregate_addr) };
        if ok != 0 { Some((fn_addr, aggregate_addr)) } else { None }
    }

    /// Register a message handler (raw FFI). Must be called during the Start phase only.
    ///
    /// The [`export_bundle!`](crate::export_bundle) macro handles this automatically.
    ///
    /// # Safety
    /// `cb` and `user_data` must remain valid for the process lifetime.
    pub unsafe fn register_message_handler_raw(&self, cb: ffi::MessageCallbackFn, user_data: *mut c_void) {
        if let Some(f) = self.api().register_message_handler {
            unsafe { f(cb, user_data) };
        }
    }
}

// ============================================================================
// BundleContext — Api + bundle identity
// ============================================================================

/// Bundle context passed to lifecycle methods after init.
///
/// Wraps [`Api`] together with the bundle's own identity, enabling
/// [`send_message`](BundleContext::send_message) without requiring the caller
/// to pass sender info manually.
///
/// Derefs to `Api`, so all `Api` methods are available directly.
#[derive(Clone, Copy)]
pub struct BundleContext {
    api: Api,
    info: *const HookyBundleInfo,
}

// Safety: both pointers are to process-lifetime statics.
unsafe impl Send for BundleContext {}
unsafe impl Sync for BundleContext {}

impl std::ops::Deref for BundleContext {
    type Target = Api;
    fn deref(&self) -> &Api {
        &self.api
    }
}

impl BundleContext {
    /// Create a new bundle context.
    ///
    /// # Safety
    /// Both pointers must point to valid data that remains valid for the process lifetime.
    pub unsafe fn new(api: Api, info: *const HookyBundleInfo) -> Self {
        Self { api, info }
    }

    /// Send a message to another bundle. The sender identity is filled in automatically.
    pub fn send_message(&self, target: &str, msg_type: u32, data: &[u8]) -> Result<(), HookResult> {
        // Safety: self.info is guaranteed valid by the BundleContext::new contract.
        unsafe { self.api.send_message_raw(self.info, target, msg_type, data) }
    }
}

// ============================================================================
// BundleInfo — Rust-friendly bundle metadata
// ============================================================================

/// Bundle metadata used in the [`Bundle`] trait.
///
/// Use `c"..."` string literals for the `id` and `name` fields:
///
/// ```rust,ignore
/// const INFO: BundleInfo = BundleInfo {
///     id: c"com.example.mybundle",
///     name: c"My Bundle",
///     version: 1,
///     api_version_min: 1,
/// };
/// ```
pub struct BundleInfo {
    /// Reverse-DNS identifier, e.g. `c"com.author.mybundle"`.
    pub id: &'static CStr,
    /// Human-readable name, e.g. `c"My Cool Bundle"`.
    pub name: &'static CStr,
    /// Bundle version number (your own versioning scheme).
    pub version: u32,
    /// Minimum DarkHooky API version this bundle requires.
    pub api_version_min: u32,
}

// ============================================================================
// BundleEntry — safe view of loaded bundles
// ============================================================================

/// A loaded bundle as seen during the Start phase.
///
/// Passed to [`Bundle::start`] so bundles can discover their peers.
#[derive(Debug, Clone)]
pub struct BundleEntry {
    /// Bundle identifier (reverse-DNS).
    pub id: String,
    /// Bundle version number.
    pub version: u32,
    /// Human-readable bundle name.
    pub name: String,
}

impl BundleEntry {
    /// Convert a raw C array of bundle entries into a `Vec<BundleEntry>`.
    ///
    /// # Safety
    /// `raw` must point to `count` valid [`HookyBundleEntry`](ffi::HookyBundleEntry) structs
    /// with valid null-terminated string pointers.
    pub unsafe fn from_raw_slice(raw: *const ffi::HookyBundleEntry, count: u32) -> Vec<Self> {
        if raw.is_null() || count == 0 {
            return Vec::new();
        }
        let slice = unsafe { std::slice::from_raw_parts(raw, count as usize) };
        slice
            .iter()
            .map(|e| {
                let id = if e.bundle_id.is_null() {
                    "(unknown)".into()
                } else {
                    unsafe { CStr::from_ptr(e.bundle_id) }.to_str().unwrap_or("(unknown)").into()
                };
                let name = if e.bundle_name.is_null() {
                    "(unnamed)".into()
                } else {
                    unsafe { CStr::from_ptr(e.bundle_name) }.to_str().unwrap_or("(unnamed)").into()
                };
                BundleEntry { id, version: e.bundle_version, name }
            })
            .collect()
    }
}

// ============================================================================
// Bundle trait
// ============================================================================

/// Trait implemented by DarkHooky bundles.
///
/// Implement this trait and call [`export_bundle!`](crate::export_bundle) to generate
/// the DLL exports that DarkHooky needs to load your bundle.
///
/// # Example
///
/// ```rust,ignore
/// use dark_hooky::{Api, Bundle, BundleContext, BundleEntry, BundleInfo};
///
/// struct MyBundle {
///     // your state here
/// }
///
/// impl Bundle for MyBundle {
///     const INFO: BundleInfo = BundleInfo {
///         id: c"com.example.mybundle",
///         name: c"My Bundle",
///         version: 1,
///         api_version_min: 1,
///     };
///
///     fn init(api: Api) -> Option<Self> {
///         api.log("MyBundle initializing!");
///         Some(MyBundle { })
///     }
///
///     fn ready(&self, ctx: BundleContext) {
///         ctx.log("MyBundle is ready!");
///     }
/// }
///
/// dark_hooky::export_bundle!(MyBundle);
/// ```
///
/// # Lifecycle
///
/// 1. **`init`** — Called when the proxy loads your DLL. Return `Some(Self)` to accept,
///    `None` to decline (DLL will be unloaded). Use `api` for logging and early setup.
///
/// 2. **`start`** — Called after all bundles init. Discover peers via `bundles`.
///    Message handler is automatically registered by `export_bundle!`.
///
/// 3. **`ready`** — Called after all bundles have started. Inter-bundle messaging is now safe.
///
/// 4. **`shutdown`** — Called during proxy DLL unload (reverse load order). Clean up resources.
pub trait Bundle: Send + Sync + 'static {
    /// Bundle metadata. Must be a `const` — used at compile time by `export_bundle!`.
    const INFO: BundleInfo;

    /// Called when the proxy loads your DLL.
    ///
    /// Return `Some(Self)` to accept, `None` to decline (DLL will be unloaded).
    fn init(api: Api) -> Option<Self>
    where
        Self: Sized;

    /// Called after all bundles have been initialized. Discover peers here.
    fn start(&self, _ctx: BundleContext, _bundles: &[BundleEntry]) {}

    /// Called after all bundles have been started. Messaging is now safe.
    fn ready(&self, _ctx: BundleContext) {}

    /// Called during proxy DLL unload (reverse load order).
    fn shutdown(&self, _ctx: BundleContext) {}

    /// Called when another bundle sends a message to this bundle.
    fn on_message(&self, _ctx: BundleContext, _sender_id: &str, _msg_type: u32, _data: &[u8]) {}
}
