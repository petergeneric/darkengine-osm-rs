//! Raw C ABI types for the DarkHooky DLL boundary.
//!
//! These `#[repr(C)]` types cross the DLL boundary via `LoadLibraryA`/`GetProcAddress`.
//! Most bundle authors should use the safe wrappers ([`super::Api`], [`super::Bundle`],
//! etc.) instead of working with these directly.

use std::ffi::{c_char, c_void};

/// Major API version; governs host↔bundle compatibility. Bumped when fields are
/// added to [`DarkHookyApi`]. Parsed from the `major.minor.revision` in
/// `api_version.txt` (via `include_str!`, so edits rebuild the crate).
pub const CURRENT_API_VERSION: u32 = parse_major(include_str!("../api_version.txt"));

/// Full `major.minor.revision` version string from `api_version.txt` (e.g.
/// `"6.0.0"`), whitespace-trimmed. Lets consumers check an exact release rather
/// than just the major. Generally speaking only major versions should be compared.
pub const CURRENT_API_VERSION_STR: &str = trim_ascii(include_str!("../api_version.txt"));

/// Const-context `str::trim` (not const in std); strips the source file's
/// trailing newline from the compiled-in version string.
const fn trim_ascii(s: &str) -> &str {
    let mut bytes = s.as_bytes();
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = bytes {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    unsafe { std::str::from_utf8_unchecked(bytes) }
}

/// Parses the leading major component (digits before the first non-digit).
const fn parse_major(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut value = 0u32;
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value * 10 + (bytes[i] - b'0') as u32;
        i += 1;
    }
    value
}

// ============================================================================
// Result codes
// ============================================================================

/// Result codes returned by DarkHooky API functions.
///
/// Values 0–99 are reserved for DarkHooky. Bundle authors may define
/// custom codes starting at 100.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookResult {
    Ok,
    VersionMismatch,
    MissingDependency,
    InitFailed,
    Unsupported,
    TargetBundleNotFound,
    NoMessageHandler,
    /// An unrecognized error code from the C ABI.
    Unknown(i32),
}

impl HookResult {
    /// Convert a raw `i32` (from the C ABI) back into a `HookResult`.
    pub fn from_i32(val: i32) -> Self {
        match val {
            0 => Self::Ok,
            1 => Self::VersionMismatch,
            2 => Self::MissingDependency,
            3 => Self::InitFailed,
            4 => Self::Unsupported,
            10 => Self::TargetBundleNotFound,
            11 => Self::NoMessageHandler,
            other => Self::Unknown(other),
        }
    }

    /// Convert to the raw `i32` representation for the C ABI.
    pub fn as_i32(&self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::VersionMismatch => 1,
            Self::MissingDependency => 2,
            Self::InitFailed => 3,
            Self::Unsupported => 4,
            Self::TargetBundleNotFound => 10,
            Self::NoMessageHandler => 11,
            Self::Unknown(val) => *val,
        }
    }
}

impl std::fmt::Display for HookResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "ok"),
            Self::VersionMismatch => write!(f, "version mismatch"),
            Self::MissingDependency => write!(f, "missing dependency"),
            Self::InitFailed => write!(f, "init failed"),
            Self::Unsupported => write!(f, "unsupported"),
            Self::TargetBundleNotFound => write!(f, "target bundle not found"),
            Self::NoMessageHandler => write!(f, "no message handler"),
            Self::Unknown(val) => write!(f, "unknown error ({})", val),
        }
    }
}

// ============================================================================
// Bundle info (C ABI)
// ============================================================================

/// Bundle self-description (C ABI), returned by `DarkHookyBundleInit`.
///
/// The bundle owns this struct — it must remain valid for the process lifetime.
/// The proxy reads from it but never modifies it.
#[repr(C)]
pub struct HookyBundleInfo {
    pub info_size: u32,
    pub bundle_id: *const c_char,
    pub bundle_version: u32,
    pub bundle_name: *const c_char,
    pub api_version_min: u32,
}

// Safety: all pointers in HookyBundleInfo point to static data (process lifetime).
unsafe impl Send for HookyBundleInfo {}
unsafe impl Sync for HookyBundleInfo {}

/// Entry in the bundle array passed to `DarkHookyBundleStart` (C ABI).
#[repr(C)]
pub struct HookyBundleEntry {
    pub bundle_id: *const c_char,
    pub bundle_version: u32,
    pub bundle_name: *const c_char,
}

// ============================================================================
// Callback types
// ============================================================================

/// D3D9 EndScene callback — called on the render thread, ordered by priority.
pub type EndSceneCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, device: *mut c_void);

/// D3D9 Present callback — called once per presented frame, ordered by priority.
pub type PresentCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, device: *mut c_void);

/// XInput pre-processing callback.
/// Return 0 to continue normal processing, non-zero to suppress default handling.
pub type XInputCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, buttons: *mut u16, lt: *mut u8, rt: *mut u8, lx: *mut i16, ly: *mut i16, rx: *mut i16, ry: *mut i16) -> i32;

/// Per-poll frame tick callback — called on the input poll thread.
pub type FrameCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, api: *const DarkHookyApi);

/// GUID structure matching Windows layout (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookyGuid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

/// A 16-byte binary GUID used as a key for named pointers.
pub type NamedPointerGuid = [u8; 16];

// ============================================================================
// DLL export signatures
// ============================================================================
//
// Signatures of the functions the host exports from `dinput.dll` and consumers
// (e.g. the OSM) resolve via `GetProcAddress` + `transmute`. Defined once so the
// host can statically assert its exports match (the `const _: ... = ...` checks
// beside each export) and consumers transmute to the shared type — a signature
// change then fails to compile on both sides instead of silently miscompiling.

/// Signature of the `DarkHookyGetApi` export.
///
/// Returns a pointer to the [`DarkHookyApi`] struct (never null in practice;
/// treat null defensively).
pub type DarkHookyGetApiFn = unsafe extern "system" fn() -> *const DarkHookyApi;

/// Signature of the `DarkHookyGetVersion` export.
///
/// Returns a process-lifetime, null-terminated UTF-8 string with the host's full
/// `major.minor.revision` version ([`CURRENT_API_VERSION_STR`]).
/// Consumers can compare it against their own compiled-in [`CURRENT_API_VERSION_STR`]
/// to detect upgradable installs.
/// Added in 6.0.0
pub type DarkHookyGetVersionFn = unsafe extern "system" fn() -> *const c_char;

/// Signature of the `DarkHookyGetNamedPointer` export.
///
/// Looks up a named pointer by its 16-byte binary GUID; returns null if none is
/// registered. The caller casts the result to the concrete pointee type agreed
/// for that GUID (e.g. `*mut OverlayState` for [`StandardNamedPointers::OverlayState`]).
pub type DarkHookyGetNamedPointerFn = unsafe extern "system" fn(guid: *const NamedPointerGuid) -> *mut c_void;

/// Standard named pointer GUIDs defined by DarkHooky.
///
/// If you would like your mod listed here (primarily to document services exposed to
/// other hooky mods), please reach out via github issue.
///
/// Each variant provides a binary GUID for use with `register_named_pointer` /
/// `get_named_pointer`. The string form is shown in comments for readability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardNamedPointers {
    /// (Private, Highly unstable) - Gamepad Mod Overlay state
    OverlayState,
}

impl StandardNamedPointers {
    /// Returns the 16-byte binary GUID for this named pointer.
    pub const fn guid(&self) -> &'static NamedPointerGuid {
        match self {
            Self::OverlayState => uuid::uuid!("E995C060-80C0-49F2-A702-9D49CB01FF16").as_bytes(),
        }
    }
}

/// DirectInput CreateDevice hook callback.
///
/// Called when the engine calls `IDirectInput::CreateDevice`. The bundle receives the
/// device GUID and the real IDirectInput pointer. Return a non-null pointer to provide
/// a replacement device (e.g. an XInput-backed fake joystick); return null to let the
/// host forward the call to the real DirectInput.
pub type DinputCreateDeviceCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, guid: *const HookyGuid, real_di: *mut c_void) -> *mut c_void;

/// DirectInput EnumDevices hook callback.
///
/// Called when the engine calls `IDirectInput::EnumDevices`. The bundle receives the
/// same parameters as the real EnumDevices. Return 0 to indicate the bundle handled
/// the enumeration (the host will still forward to the real DirectInput afterward);
/// return non-zero to skip this bundle's handling.
pub type DinputEnumDevicesCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, dev_type: u32, enum_cb: *const c_void, enum_ctx: *mut c_void, flags: u32) -> i32;

/// Inter-bundle message handler.
pub type MessageCallbackFn = unsafe extern "system" fn(user_data: *mut c_void, sender: *const HookyBundleInfo, msg_type: u32, data: *const c_void, data_len: u32);

// ============================================================================
// API struct (C ABI)
// ============================================================================

/// The raw C ABI struct passed to bundle DLLs at initialization.
///
/// Uses the Win32 `cbSize` pattern for forward compatibility: `api_size` is always
/// the first field. Before accessing any field, verify `api_size` is large enough.
/// All function pointer fields are `Option` (null = not available).
///
/// Most bundle authors should use [`super::Api`] instead of this struct directly.
#[repr(C)]
pub struct DarkHookyApi {
    pub api_size: u32,
    pub api_version: u32,
    pub log: Option<unsafe extern "system" fn(msg: *const c_char)>,
    pub inject_key: Option<unsafe extern "system" fn(scancode: u16, key_up: i32, extended: i32)>,
    pub inject_mouse_move: Option<unsafe extern "system" fn(dx: i32, dy: i32)>,
    pub get_game_directory: Option<unsafe extern "system" fn() -> *const c_char>,
    pub request_d3d9_endscene: Option<unsafe extern "system" fn(cb: EndSceneCallbackFn, user_data: *mut c_void, priority: i32)>,
    pub request_d3d9_device: Option<unsafe extern "system" fn() -> *mut c_void>,
    pub request_xinput_hook: Option<unsafe extern "system" fn(cb: XInputCallbackFn, user_data: *mut c_void)>,
    pub request_frame_callback: Option<unsafe extern "system" fn(cb: FrameCallbackFn, user_data: *mut c_void)>,
    pub is_bundle_loaded: Option<unsafe extern "system" fn(bundle_id: *const c_char, min_version: u32) -> i32>,
    pub send_message: Option<unsafe extern "system" fn(sender: *const HookyBundleInfo, target_id: *const c_char, msg_type: u32, data: *const c_void, data_len: u32) -> i32>,
    pub register_message_handler: Option<unsafe extern "system" fn(cb: MessageCallbackFn, user_data: *mut c_void)>,

    // --- API v2 fields ---
    pub request_dinput_create_device_hook: Option<unsafe extern "system" fn(cb: DinputCreateDeviceCallbackFn, user_data: *mut c_void)>,
    pub request_dinput_enum_devices_hook: Option<unsafe extern "system" fn(cb: DinputEnumDevicesCallbackFn, user_data: *mut c_void)>,
    pub register_named_pointer: Option<unsafe extern "system" fn(guid: *const NamedPointerGuid, ptr: *mut c_void)>,
    pub inject_mouse_click: Option<unsafe extern "system" fn(down: i32)>,
    pub set_cursor_640: Option<unsafe extern "system" fn(x: i32, y: i32)>,
    pub get_elapsed_ms: Option<unsafe extern "system" fn() -> u64>,

    // --- API v3 fields ---
    /// Find COM_QueryInterface by scanning the game exe's .text section.
    /// Returns 1 on success (writing fn_addr and aggregate_addr to the out pointers),
    /// or 0 if the signature was not found.
    pub find_com_query_interface: Option<unsafe extern "system" fn(fn_addr: *mut u32, aggregate_addr: *mut u32) -> i32>,

    // --- API v4 fields ---
    /// Register a D3D9 Present callback. Fires once per presented frame (after all
    /// EndScene calls). Lower `priority` values run first.
    pub request_d3d9_present: Option<unsafe extern "system" fn(cb: PresentCallbackFn, user_data: *mut c_void, priority: i32)>,

    // --- API v5 fields ---
    /// Report whether `d3d9!Present`'s prologue was already a relative `JMP`
    /// when the host installed its inline detour — i.e. whether another D3D9
    /// wrapper or overlay (dgVoodoo2, DXVK, ReShade, etc.) had already
    /// hooked it. Returns `1` if pre-hooked, `0` if not, `-1` if the host
    /// has not yet installed the Present detour (called too early, or the
    /// install failed). Bundles use this to decide whether to drive their
    /// per-frame work from EndScene (pre-hook conflict) or Present (clean).
    pub d3d9_present_prologue_is_jmp: Option<unsafe extern "system" fn() -> i32>,
}
