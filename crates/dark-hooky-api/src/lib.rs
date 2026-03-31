//! # DarkHooky API — Types and traits for the DarkHooky bundle system
//!
//! This crate provides the API types, [`Bundle`] trait, and [`export_bundle!`] macro
//! that bundle authors need to create DarkHooky plugins. It contains no host-side
//! logic — the host implementation lives in the `dark-hooky-dinput` crate.
//!
//! ## For bundle authors
//!
//! Implement the [`Bundle`] trait and call [`export_bundle!`] to generate the DLL exports:
//!
//! ```rust,ignore
//! use dark_hooky_api::{Api, Bundle, BundleContext, BundleInfo};
//!
//! struct MyBundle;
//!
//! impl Bundle for MyBundle {
//!     const INFO: BundleInfo = BundleInfo {
//!         id: c"com.example.mybundle",
//!         name: c"My Bundle",
//!         version: 1,
//!         api_version_min: 1,
//!     };
//!
//!     fn init(api: Api) -> Option<Self> {
//!         api.log("Hello from MyBundle!");
//!         Some(MyBundle)
//!     }
//!
//!     fn ready(&self, ctx: BundleContext) {
//!         ctx.log("MyBundle is ready!");
//!     }
//! }
//!
//! dark_hooky_api::export_bundle!(MyBundle);
//! ```
//!
//! Build your crate as a 32-bit Windows DLL (`cdylib`), name it `hooky<anything>.dll`
//! (e.g. `hooky_my_mod.dll`), and place it in `MODS/<your_folder>/` under the game directory.
//!
//! ## Bundle lifecycle
//!
//! 1. **Init** — DLL loaded, [`Bundle::init`] called with [`Api`]. Return `Some(Self)`
//!    to accept, `None` to decline (DLL unloaded).
//!
//! 2. **Start** — [`Bundle::start`] called with a [`BundleContext`] and list of all loaded
//!    bundles. Message handler is auto-registered by [`export_bundle!`].
//!
//! 3. **Ready** — [`Bundle::ready`] called. Inter-bundle messaging is safe from this point.
//!
//! 4. **Shutdown** — [`Bundle::shutdown`] called in reverse load order during DLL unload.
//!
//! ## API versioning
//!
//! The raw [`ffi::DarkHookyApi`] uses the Win32 `cbSize` pattern for forward compatibility.
//! The [`Api`] wrapper handles bounds checks internally — a bundle compiled against API v2
//! running on a v1 host will get `None`/`false`/no-op for v2-only features.
//!
//! ## Threading model
//!
//! - [`Api`] methods are safe to call from any thread.
//! - EndScene callbacks run on the D3D9 render thread.
//! - XInput and frame callbacks run on the input poll thread (must not block).
//! - Inter-bundle messages are delivered synchronously on the sender's thread.

mod api;
pub mod ffi;

pub use api::{Api, Bundle, BundleContext, BundleEntry, BundleInfo};
pub use ffi::{HookResult, StandardNamedPointers};

/// Generate the DLL exports required for a DarkHooky bundle.
///
/// Call this once at the top level of your crate, passing the type that implements [`Bundle`]:
///
/// ```rust,ignore
/// dark_hooky_api::export_bundle!(MyBundle);
/// ```
///
/// This generates four `extern "system"` exports:
/// - `DarkHookyBundleInit` — calls [`Bundle::init`]
/// - `DarkHookyBundleStart` — calls [`Bundle::start`], registers message handler
/// - `DarkHookyBundleReady` — calls [`Bundle::ready`]
/// - `DarkHookyBundleShutdown` — calls [`Bundle::shutdown`]
///
/// The macro also stores the bundle instance and API handle in static storage,
/// and generates a message handler trampoline that calls [`Bundle::on_message`].
#[macro_export]
macro_rules! export_bundle {
    ($ty:ty) => {
        #[doc(hidden)]
        mod __dark_hooky_bundle {
            use super::*;
            use std::ffi::c_void;
            use std::sync::OnceLock;

            static INSTANCE: OnceLock<$ty> = OnceLock::new();
            static CTX: OnceLock<$crate::BundleContext> = OnceLock::new();

            static BUNDLE_INFO: $crate::ffi::HookyBundleInfo = $crate::ffi::HookyBundleInfo {
                info_size: std::mem::size_of::<$crate::ffi::HookyBundleInfo>() as u32,
                bundle_id: <$ty as $crate::Bundle>::INFO.id.as_ptr(),
                bundle_version: <$ty as $crate::Bundle>::INFO.version,
                bundle_name: <$ty as $crate::Bundle>::INFO.name.as_ptr(),
                api_version_min: <$ty as $crate::Bundle>::INFO.api_version_min,
            };

            fn ctx() -> $crate::BundleContext {
                *CTX.get().expect("DarkHooky: lifecycle method called before init")
            }

            unsafe extern "system" fn message_trampoline(_user_data: *mut c_void, sender: *const $crate::ffi::HookyBundleInfo, msg_type: u32, data: *const c_void, data_len: u32) {
                let Some(instance) = INSTANCE.get() else {
                    return;
                };
                let sender_id = if !sender.is_null() {
                    let id_ptr = unsafe { (*sender).bundle_id };
                    if !id_ptr.is_null() {
                        unsafe { std::ffi::CStr::from_ptr(id_ptr) }.to_str().unwrap_or("(unknown)")
                    } else {
                        "(unknown)"
                    }
                } else {
                    "(unknown)"
                };
                let data_slice = if data.is_null() || data_len == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(data as *const u8, data_len as usize) }
                };
                instance.on_message(ctx(), sender_id, msg_type, data_slice);
            }

            #[unsafe(export_name = "DarkHookyBundleInit")]
            pub unsafe extern "system" fn bundle_init(raw_api: *const $crate::ffi::DarkHookyApi) -> *const $crate::ffi::HookyBundleInfo {
                let api = match unsafe { $crate::Api::from_raw(raw_api) } {
                    Some(a) => a,
                    None => return std::ptr::null(),
                };
                match <$ty as $crate::Bundle>::init(api) {
                    Some(instance) => {
                        let ctx = unsafe { $crate::BundleContext::new(api, &BUNDLE_INFO) };
                        CTX.set(ctx).ok();
                        INSTANCE.set(instance).ok();
                        &BUNDLE_INFO as *const $crate::ffi::HookyBundleInfo
                    }
                    None => std::ptr::null(),
                }
            }

            #[unsafe(export_name = "DarkHookyBundleStart")]
            pub unsafe extern "system" fn bundle_start(raw_bundles: *const $crate::ffi::HookyBundleEntry, count: u32) {
                let Some(instance) = INSTANCE.get() else {
                    return;
                };
                let ctx = ctx();

                unsafe {
                    ctx.register_message_handler_raw(message_trampoline, std::ptr::null_mut());
                }

                let entries = unsafe { $crate::BundleEntry::from_raw_slice(raw_bundles, count) };
                instance.start(ctx, &entries);
            }

            #[unsafe(export_name = "DarkHookyBundleReady")]
            pub unsafe extern "system" fn bundle_ready() {
                if let Some(instance) = INSTANCE.get() {
                    instance.ready(ctx());
                }
            }

            #[unsafe(export_name = "DarkHookyBundleShutdown")]
            pub unsafe extern "system" fn bundle_shutdown() {
                if let Some(instance) = INSTANCE.get() {
                    instance.shutdown(ctx());
                }
            }
        }
    };
}
