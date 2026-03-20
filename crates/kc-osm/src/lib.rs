#![allow(non_snake_case)]

mod malloc;
mod services;
mod sync_wrapper;

/// Provides a stub for `_Unwind_Resume` needed when targeting `i686-pc-windows-gnu`.
///
/// Rust's pre-built std for the `-gnu` target emits `_Unwind_Resume` references even
/// with `panic = "abort"`. Call this macro once in your cdylib crate's `lib.rs`.
/// On `-msvc` targets this is a no-op.
#[macro_export]
macro_rules! unwind_resume_stub {
    () => {
        #[cfg(target_env = "gnu")]
        #[unsafe(no_mangle)]
        pub extern "C" fn _Unwind_Resume() -> ! {
            unsafe { core::hint::unreachable_unchecked() }
        }
    };
}

pub use sync_wrapper::UnsafeSendSync;

use std::{
    ffi::{CStr, CString, c_char, c_float, c_int, c_uchar, c_uint, c_ulong},
    os::raw::c_void,
    ptr::null,
};

/// Type discriminant for [`sMultiParm`].
///
/// The engine uses `t = 0` (undef) and `t = 1` (int) interchangeably for integer
/// values (e.g. HitPoints returns t=1), and `t = 2` for strings. Both 0 and 1 are
/// treated as Int.
// TODO: add Float(2), String(3), Vector(4) variants and handle the t=0 vs t=1
// ambiguity properly.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultiParmType {
    /// Untyped / integer — the engine uses 0 for integers.
    Int = 0,
    /// String — the engine uses 2 for string values from property Get.
    String = 2,
    /// Object reference (same representation as integer).
    Object = 5,
}

impl MultiParmType {
    /// Convert from raw `c_int`. Returns `None` for unknown discriminants.
    #[must_use]
    pub fn from_raw(t: c_int) -> Option<Self> {
        match t {
            0 => Some(Self::Int),
            2 => Some(Self::String),
            5 => Some(Self::Object),
            _ => None,
        }
    }
}

/// Type-safe wrapper for the Dark Engine's `cMultiParm` discriminated union.
///
/// Use [`MultiParm::from_raw`] to convert from an [`sMultiParm`] returned by the engine,
/// and [`MultiParm::into_raw`] to produce an [`sMultiParm`] for passing to the engine.
#[derive(Debug)]
pub enum MultiParm {
    /// Untyped / integer value (`t = 0`).
    Int(i32),
    /// Owned string value. Copied from the engine's allocated buffer.
    String(CString),
    /// Object reference (same as integer, but semantically an object ID).
    Object(ObjectId),
    /// Unknown or undefined type — preserves both the type discriminant and raw value
    /// for debugging and round-tripping.
    Undefined { t: i32, val: i32 },
}

impl MultiParm {
    /// Convert a raw [`sMultiParm`] (as returned by the engine) into a typed [`MultiParm`].
    ///
    /// For string values (`t == 2`), the string data is **copied** into an owned `CString`.
    /// The caller is responsible for freeing the original engine-allocated pointer if needed
    /// (e.g. via `malloc::free`).
    ///
    /// # Safety
    /// - For string types, `parm.val` must be a valid pointer to a null-terminated C string
    ///   (or null, which produces an empty string).
    #[must_use]
    pub unsafe fn from_raw(parm: &sMultiParm) -> Self {
        const _: () = assert!(size_of::<*const c_char>() == size_of::<c_int>(), "sMultiParm.val is a c_int/pointer union; requires 32-bit target");
        match parm.t {
            // 0 = undef, 1 = int — both treated as integer
            0 | 1 => MultiParm::Int(parm.val),
            2 => {
                // String: val is a pointer to a C string.
                // Validate pointer range before dereferencing — the SDK header declares
                // The engine also uses t=2 for floats, so a float value could arrive with t=2 and val containing
                // float bits rather than a valid pointer.
                let ptr = parm.val as u32;
                if parm.val == 0 {
                    MultiParm::String(CString::default())
                } else if ptr >= 0x10000 && ptr < 0x80000000 {
                    let cstr = unsafe { CStr::from_ptr(parm.val as *const c_char) };
                    MultiParm::String(CString::from(cstr))
                } else {
                    MultiParm::Undefined { t: parm.t, val: parm.val }
                }
            }
            5 => MultiParm::Object(ObjectId(parm.val)),
            _ => MultiParm::Undefined { t: parm.t, val: parm.val },
        }
    }

    /// Convert this typed value into a raw [`sMultiParm`] for passing to the engine.
    ///
    /// For [`MultiParm::String`], the `CString` is consumed via `into_raw()` — the engine
    /// (or caller) is responsible for the pointer's lifetime. Only safe on 32-bit targets
    /// where `*const c_char` fits in `c_int`.
    #[must_use]
    pub fn into_raw(self) -> sMultiParm {
        match self {
            MultiParm::Int(v) => sMultiParm {
                val: v,
                t: MultiParmType::Int as c_int,
            },
            MultiParm::String(s) => sMultiParm {
                val: s.into_raw() as c_int,
                t: MultiParmType::String as c_int,
            },
            MultiParm::Object(id) => sMultiParm {
                val: id.0,
                t: MultiParmType::Object as c_int,
            },
            MultiParm::Undefined { t, val } => sMultiParm { val, t },
        }
    }

    /// Returns the integer value if this is `Int` or `Object`.
    #[must_use]
    pub fn as_int(&self) -> Option<i32> {
        match self {
            MultiParm::Int(v) => Some(*v),
            MultiParm::Object(id) => Some(id.0),
            _ => None,
        }
    }

    /// Returns a string reference if this is `String`.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            MultiParm::String(s) => s.to_str().ok(),
            _ => None,
        }
    }
}

impl From<i32> for MultiParm {
    fn from(v: i32) -> Self {
        MultiParm::Int(v)
    }
}

impl From<ObjectId> for MultiParm {
    fn from(id: ObjectId) -> Self {
        MultiParm::Object(id)
    }
}

impl From<&str> for MultiParm {
    fn from(s: &str) -> Self {
        MultiParm::String(CString::new(s).unwrap_or_default())
    }
}

pub use crate::services::*;
pub use kc_osm_proc_macros::{dark_engine_service, dark_script};
pub use windows::{Win32::System::Com::IMalloc, core::*};

/// Dark Engine object identifier. Wraps a raw `i32` for type safety.
///
/// - Negative values are archetypes (e.g. Weapon = -30)
/// - Positive values are concrete objects (e.g. a specific sword instance)
/// - Zero means "no object" or "any" (wildcard in link queries)
///
/// Use `Option<ObjectId>` for APIs where zero means "not found".
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObjectId(pub i32);

impl ObjectId {
    /// Returns `true` if this is an archetype (negative ID).
    #[must_use]
    pub fn is_archetype(self) -> bool {
        self.0 < 0
    }

    /// Returns `true` if this is a concrete object (positive ID).
    #[must_use]
    pub fn is_concrete(self) -> bool {
        self.0 > 0
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Dark Engine link instance identifier. Wraps a raw `i32` for type safety.
///
/// Distinct from [`LinkKind`] (the relation type) and [`ObjectId`] (the object).
/// Zero means "no link" — use `Option<LinkId>` where appropriate.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkId(pub i32);

impl std::fmt::Display for LinkId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Dark Engine link flavor/relation type (e.g. "Contains", "~ControlDevice").
///
/// Obtained from [`LinkToolsService::link_kind_named`]. Distinct from [`LinkId`]
/// (a specific link instance) and [`ObjectId`].
/// Zero means "any flavor" (wildcard in link queries).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LinkKind(pub i32);

impl std::fmt::Display for LinkKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Frob message — extends `sScrMsg` with frob-specific fields.
/// Received for FrobWorldBegin/End, FrobToolBegin/End, FrobInvBegin/End messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sFrobMsg {
    pub base: sScrMsg,
    pub srcobj: ObjectId,
    pub destobj: ObjectId,
    pub frobber: ObjectId,
    pub srcloc: c_int,  // eFrobLoc
    pub destloc: c_int, // eFrobLoc
    pub sec: c_float,
    pub abort: c_int, // BOOL
}

impl std::ops::Deref for sFrobMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sFrobMsg {
    /// Cast a base `sScrMsg` to `sFrobMsg`.
    ///
    /// Prefer using typed handler parameters via `#[dark_script]` instead of
    /// calling this directly — the macro dispatches typed messages automatically.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a frob message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sFrobMsg) }
    }
}

/// Timer message — extends `sScrMsg` with the timer name.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sScrTimerMsg {
    pub base: sScrMsg,
    pub name: *const c_char,
}

impl std::ops::Deref for sScrTimerMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sScrTimerMsg {
    /// Cast a base `sScrMsg` to `sScrTimerMsg`.
    ///
    /// Prefer using typed handler parameters via `#[dark_script]` instead of
    /// calling this directly — the macro dispatches typed messages automatically.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a Timer message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sScrTimerMsg) }
    }

    #[must_use]
    pub fn timer_name(&self) -> &str {
        if self.name.is_null() {
            return "";
        }
        unsafe { CStr::from_ptr(self.name).to_str().unwrap_or("") }
    }
}

/// Door message — extends `sScrMsg` with door action fields.
/// Received for DoorOpen, DoorClose, DoorOpening, DoorClosing, DoorHalt messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sDoorMsg {
    pub base: sScrMsg,
    pub action: c_int,     // eDoorAction
    pub prevaction: c_int, // eDoorAction
    pub proxy: c_int,      // BOOL (NewDark only, but present in struct layout)
}

impl std::ops::Deref for sDoorMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sDoorMsg {
    /// Cast a base `sScrMsg` to `sDoorMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a door message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sDoorMsg) }
    }

    /// Returns the door action as a typed enum, if valid.
    pub fn door_action(&self) -> std::result::Result<DoorAction, i32> {
        DoorAction::try_from(self.action)
    }

    /// Returns the previous door action as a typed enum, if valid.
    pub fn prev_action(&self) -> std::result::Result<DoorAction, i32> {
        DoorAction::try_from(self.prevaction)
    }
}

/// Damage message — extends `sScrMsg` with damage info.
/// Received for "Damage" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sDamageScrMsg {
    pub base: sScrMsg,
    pub kind: c_int,
    pub damage: c_int,
    pub culprit: ObjectId,
}

impl std::ops::Deref for sDamageScrMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sDamageScrMsg {
    /// Cast a base `sScrMsg` to `sDamageScrMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a Damage message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sDamageScrMsg) }
    }
}

/// Slay message — extends `sScrMsg` with slay info.
/// Received for "Slain" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sSlayMsg {
    pub base: sScrMsg,
    pub culprit: ObjectId,
    pub kind: c_int,
}

impl std::ops::Deref for sSlayMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sSlayMsg {
    /// Cast a base `sScrMsg` to `sSlayMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a Slain message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sSlayMsg) }
    }
}

/// Contained message — extends `sScrMsg` with container info.
/// Received for "Contained" messages (sent to the contained object).
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sContainedScrMsg {
    pub base: sScrMsg,
    pub container: ObjectId,
}

impl std::ops::Deref for sContainedScrMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sContainedScrMsg {
    /// Cast a base `sScrMsg` to `sContainedScrMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a Contained message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sContainedScrMsg) }
    }
}

/// Container message — extends `sScrMsg` with containee info.
/// Received for "Container" messages (sent to the container object).
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sContainerScrMsg {
    pub base: sScrMsg,
    pub containee: ObjectId,
}

impl std::ops::Deref for sContainerScrMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sContainerScrMsg {
    /// Cast a base `sScrMsg` to `sContainerScrMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a Container message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sContainerScrMsg) }
    }
}

/// Stimulus message — extends `sScrMsg` with stimulus data.
/// Received for "Stimulus" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sStimMsg {
    pub base: sScrMsg,
    pub stimulus: ObjectId,
    pub intensity: c_float,
    pub sensor: ObjectId,
    pub source: ObjectId,
}

impl std::ops::Deref for sStimMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sStimMsg {
    /// Cast a base `sScrMsg` to `sStimMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a Stimulus message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sStimMsg) }
    }
}

/// Body action types for `sBodyMsg`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyAction {
    MotionStart = 0,
    MotionEnd = 1,
    MotionFlagReached = 2,
}

impl TryFrom<i32> for BodyAction {
    type Error = i32;
    fn try_from(v: i32) -> std::result::Result<Self, i32> {
        match v {
            0 => Ok(Self::MotionStart),
            1 => Ok(Self::MotionEnd),
            2 => Ok(Self::MotionFlagReached),
            _ => Err(v),
        }
    }
}

/// Body message — extends `sScrMsg` with motion/body action info.
/// Received for "MotionStart", "MotionEnd", "MotionFlagReached" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sBodyMsg {
    pub base: sScrMsg,
    pub actiontype: c_int, // eBodyAction
    pub motionname: *const c_char,
    pub flagvalue: c_int,
}

impl std::ops::Deref for sBodyMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sBodyMsg {
    /// Cast a base `sScrMsg` to `sBodyMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a body/motion message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sBodyMsg) }
    }

    /// Returns the body action as a typed enum, if valid.
    pub fn body_action(&self) -> std::result::Result<BodyAction, i32> {
        BodyAction::try_from(self.actiontype)
    }

    /// Returns the motion name as a string slice.
    pub fn motion_name(&self) -> &str {
        if self.motionname.is_null() {
            return "";
        }
        unsafe { CStr::from_ptr(self.motionname).to_str().unwrap_or("") }
    }
}

/// Physics message — extends `sScrMsg` with collision/contact data.
/// Received for PhysMadePhysical, PhysMadeNonPhysical, PhysCollision,
/// PhysFellAsleep, PhysWokeUp, PhysContactCreate, PhysContactDestroy messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sPhysMsg {
    pub base: sScrMsg,
    pub submod: c_int,
    pub colltype: c_int, // ePhysCollisionType
    pub collobj: ObjectId,
    pub collsubmod: c_int,
    pub collmomentum: c_float,
    pub collnormal: sVector,
    pub collpoint: sVector,
    pub contacttype: c_int, // ePhysContactType
    pub contactobj: ObjectId,
    pub contactsubmod: c_int,
    pub transobj: ObjectId,
    pub transsubmod: c_int,
}

impl std::ops::Deref for sPhysMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sPhysMsg {
    /// Cast a base `sScrMsg` to `sPhysMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a physics message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sPhysMsg) }
    }
}

/// Room transition message — extends `sScrMsg` with room transit data.
/// Received for "ObjRoomTransit", "CreatureRoomEnter", "CreatureRoomExit" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sRoomMsg {
    pub base: sScrMsg,
    pub fromobj: ObjectId,
    pub toobj: ObjectId,
    pub moveobj: ObjectId,
    pub objtype: c_int,   // eObjType
    pub transtype: c_int, // eTransType
}

impl std::ops::Deref for sRoomMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sRoomMsg {
    /// Cast a base `sScrMsg` to `sRoomMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a room message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sRoomMsg) }
    }
}

/// Tweq message — extends `sScrMsg` with tweq animation data.
/// Received for "TweqComplete" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sTweqMsg {
    pub base: sScrMsg,
    pub tweq_type: c_int, // eTweqType
    pub op: c_int,        // eTweqOperation
    pub dir: c_int,       // eTweqDirection
}

impl std::ops::Deref for sTweqMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sTweqMsg {
    /// Cast a base `sScrMsg` to `sTweqMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a TweqComplete message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sTweqMsg) }
    }
}

/// Combine message — extends `sScrMsg` with combiner object.
/// Received for "CombineAdd" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sCombineScrMsg {
    pub base: sScrMsg,
    pub combiner: ObjectId,
}

impl std::ops::Deref for sCombineScrMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sCombineScrMsg {
    /// Cast a base `sScrMsg` to `sCombineScrMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a CombineAdd message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sCombineScrMsg) }
    }
}

/// Attack message — extends `sScrMsg` with weapon object.
/// Received for "StartAttack", "StartWindup", "EndAttack" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sAttackMsg {
    pub base: sScrMsg,
    pub weapon: ObjectId,
}

impl std::ops::Deref for sAttackMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sAttackMsg {
    /// Cast a base `sScrMsg` to `sAttackMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually an attack message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sAttackMsg) }
    }
}

/// Quest variable change message — extends `sScrMsg` with quest data.
/// Received for "QuestChange" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sQuestMsg {
    pub base: sScrMsg,
    pub name: *const c_char,
    pub oldvalue: c_int,
    pub newvalue: c_int,
}

impl std::ops::Deref for sQuestMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sQuestMsg {
    /// Cast a base `sScrMsg` to `sQuestMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a QuestChange message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sQuestMsg) }
    }

    /// Returns the quest variable name as a string slice.
    pub fn quest_name(&self) -> &str {
        if self.name.is_null() {
            return "";
        }
        unsafe { CStr::from_ptr(self.name).to_str().unwrap_or("") }
    }
}

/// Medium transition message — extends `sScrMsg` with medium type data.
/// Received for "MediumTransition" messages.
#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sMediumTransMsg {
    pub base: sScrMsg,
    pub fromtype: c_int,
    pub totype: c_int,
}

impl std::ops::Deref for sMediumTransMsg {
    type Target = sScrMsg;
    fn deref(&self) -> &sScrMsg {
        &self.base
    }
}

impl sMediumTransMsg {
    /// Cast a base `sScrMsg` to `sMediumTransMsg`.
    ///
    /// # Safety
    /// Caller must ensure the message is actually a MediumTransition message.
    pub unsafe fn from_msg(msg: &sScrMsg) -> &Self {
        unsafe { &*(msg as *const sScrMsg as *const sMediumTransMsg) }
    }
}

/// Door actions, received in `sDoorMsg` script messages.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorAction {
    Open = 0,
    Close = 1,
    Opening = 2,
    Closing = 3,
    Halt = 4,
}

impl TryFrom<i32> for DoorAction {
    type Error = i32;
    fn try_from(v: i32) -> std::result::Result<Self, i32> {
        match v {
            0 => Ok(Self::Open),
            1 => Ok(Self::Close),
            2 => Ok(Self::Opening),
            3 => Ok(Self::Closing),
            4 => Ok(Self::Halt),
            _ => Err(v),
        }
    }
}

/// Door states, returned by `DoorService::get_door_state`.
/// The engine returns 5 if the object is not a door.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoorState {
    Closed = 0,
    Open = 1,
    Closing = 2,
    Opening = 3,
    Halted = 4,
}

impl TryFrom<i32> for DoorState {
    type Error = i32;
    fn try_from(v: i32) -> std::result::Result<Self, i32> {
        match v {
            0 => Ok(Self::Closed),
            1 => Ok(Self::Open),
            2 => Ok(Self::Closing),
            3 => Ok(Self::Opening),
            4 => Ok(Self::Halted),
            _ => Err(v),
        }
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sVector {
    pub x: c_float,
    pub y: c_float,
    pub z: c_float,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sDatapath {
    pub num: c_uchar,
    pub last: c_uchar,
    pub nocurrent: BOOL,
    pub datapath: [*mut c_char; 16usize],
    pub findflags: c_int,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sLink {
    pub source: ObjectId,
    pub dest: ObjectId,
    pub flavor: LinkKind,
}

/// A link entry from a link query, combining the link instance ID with full link data.
#[derive(Debug, Clone, Copy)]
pub struct LinkEntry {
    pub id: LinkId,
    pub source: ObjectId,
    pub dest: ObjectId,
    pub flavor: LinkKind,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sMultiParm {
    pub val: c_int, // Union
    pub t: c_int,   // enum
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sScrClassDesc {
    pub mod_: *const c_char,
    pub name: *const c_char,
    pub base: *const c_char,
    pub factory: unsafe extern "C" fn(name: *const c_char, obj_id: c_int) -> *mut IScript,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sScrDatumTag {
    pub objId: c_int,
    pub _class: *const c_char,
    pub name: *const c_char,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sScrTraceHashKey {
    pub combo: [c_uchar; 40usize],
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sScrTrace {
    pub hostobj: c_int,
    pub action: c_uint,
    pub line: c_int,
    pub hashkey: sScrTraceHashKey,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct sPersistentVtbl {
    pub Destruct: Option<unsafe extern "C" fn(arg1: *mut sPersistentVtbl)>,
    pub Persistence: Option<unsafe extern "C" fn(arg1: *mut sPersistentVtbl) -> BOOL>,
    pub GetName: Option<unsafe extern "C" fn(arg1: *mut sPersistentVtbl) -> *const ::std::os::raw::c_char>,
}

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct sScrMsg {
    pub lpVtbl: *mut IUnknown_Vtbl,
    pub count: c_uint,
    pub lpPersistentVtbl: *mut sPersistentVtbl,
    pub from: ObjectId,
    pub to: ObjectId,
    pub message: *const c_char,
    pub time: c_ulong,
    pub flags: c_int,
    pub data: sMultiParm,
    pub data2: sMultiParm,
    pub data3: sMultiParm,
}

#[interface("D00000D0-7B50-129F-8348-00AA00A82B51")]
pub unsafe trait IScript: IUnknown {
    fn GetClassName(&self) -> *const c_char;
    fn ReceiveMessage(&self, msg: &mut sScrMsg, parms: &mut sMultiParm, action: i32) -> HRESULT;
}

#[interface("CF0000CF-7B4F-129E-8348-00AA00A82B51")]
pub unsafe trait IScriptMan: IUnknown {
    fn GameInit(&self) -> HRESULT;
    fn GameEnd(&self) -> HRESULT;
    fn BeginScripts(&self) -> HRESULT;
    fn EndScripts(&self) -> HRESULT;
    fn SetModuleDatapath(&self, datapath: *const sDatapath) -> HRESULT;
    fn AddModule(&self, name: *const c_char) -> HRESULT;
    fn RemoveModule(&self, name: *const c_char) -> HRESULT;
    fn ClearModules(&self) -> HRESULT;
    fn ExposeService(&self, service: IUnknown, guid: *const GUID) -> HRESULT;
    fn GetService(&self, guid: &GUID) -> IUnknown;
    fn GetFirstClass(&self, class: *mut c_uint) -> *const sScrClassDesc;
    fn GetNextClass(&self, class: *mut c_uint) -> *const sScrClassDesc;
    fn EndClassIter(&self, iter: *mut c_uint);
    fn GetClass(&self, name: *const c_char) -> *const sScrClassDesc;
    fn SetObjScripts(&self, obj_id: c_int, names: *mut *const c_char, len: c_uint) -> HRESULT;
    fn ForgetObj(&self, obj_id: c_int) -> HRESULT;
    fn ForgetAllObjs(&self) -> HRESULT;
    fn WantsMessage(&self, obj_id: c_int, msg_name: *const c_char) -> BOOL;
    fn SendMessage(&self, msg: *mut sScrMsg, parms: *mut sMultiParm) -> HRESULT;
    fn KilTimedMessage(&self, msg_id: c_uint);
    fn PumpMessages(&self) -> c_int;
    fn PostMessage(&self, msg: *mut sScrMsg);
    fn SetTimedMessage(&self, msg: *mut sScrMsg, time: c_ulong, kind: c_int) -> c_uint;
    fn SendMessage2(&self, from: c_int, to: c_int, msg_name: *const c_char, parms1: *const sMultiParm, parms2: *const sMultiParm, parms3: *const sMultiParm) -> sMultiParm;
    fn PostMessage2(&self, from: c_int, to: c_int, msg_name: *const c_char, parms1: *const sMultiParm, parms2: *const sMultiParm, parms3: *const sMultiParm, flags: c_ulong);
    fn SetTimedMessage2(&self, to: c_int, msg_name: *const c_char, time: c_ulong, kind: c_int, parms: *const sMultiParm) -> c_uint;
    fn IsScriptDataSet(&self, tag: *const sScrDatumTag) -> BOOL;
    fn GetScriptData(&self, tag: *const sScrDatumTag, parms: *mut sMultiParm) -> HRESULT;
    fn SetScriptData(&self, tag: *const sScrDatumTag, parms: *const sMultiParm) -> HRESULT;
    fn ClearScriptData(&self, tag: *const sScrDatumTag, parms: *mut sMultiParm) -> HRESULT;
    fn AddTrace(&self, obj_id: c_int, name: *const c_char, unk1: c_int, unk2: c_int) -> HRESULT;
    fn RemoveTrace(&self, obj_id: c_int, name: *const c_char) -> HRESULT;
    fn GetTraceLine(&self, line: c_int) -> BOOL;
    fn SetTraceLine(&self, line: c_int, on: BOOL);
    fn GetTraceLineMask(&self) -> c_int;
    fn SetTraceLineMask(&self, mask: c_int);
    fn GetFirstTrace(&self, iter: *mut c_uint) -> *const sScrTrace;
    fn GetNextTrace(&self, iter: *mut c_uint) -> *const sScrTrace;
    fn EndTraceIter(&self, iter: *mut c_uint);
    fn SaveLoad(&self, func: *mut c_int, ctx: *mut c_void, loading: BOOL) -> HRESULT;
    fn PostLoad(&self);
}

#[interface("D40000D4-7B54-12A3-8348-00AA00A82B51")]
unsafe trait IScriptModule: IUnknown {
    fn GetName(&self) -> *const c_char;
    fn GetFirstClass(&self, iter: &mut c_uint) -> *const sScrClassDesc;
    fn GetNextClass(&self, iter: &mut c_uint) -> *const sScrClassDesc;
    fn EndClassIter(&self, iter: &mut c_uint);
}

#[implement(IScriptModule)]
pub struct ScriptModule {
    name: CString,
    classes: Vec<sScrClassDesc>,
}

impl ScriptModule {
    fn new(name: &str) -> Self {
        Self {
            name: CString::new(name).unwrap(),
            classes: vec![],
        }
    }

    pub fn register_script<T>(&mut self)
    where
        T: DarkScript,
        IScript: From<T>,
    {
        self.classes.push(T::get_desc(self.name.to_str().unwrap()));
    }

    /// # Safety
    ///
    /// `out_mod` must be a non-null, valid pointer for writing an interface pointer.
    unsafe fn register(self, out_mod: *mut *mut c_void) -> bool {
        let script_module: IScriptModule = self.into();
        let guid = IScriptModule::IID;
        unsafe {
            if !HRESULT::is_ok(script_module.query(&raw const guid, out_mod)) {
                return false;
            }
        }

        true
    }
}

impl IScriptModule_Impl for ScriptModule_Impl {
    unsafe fn GetName(&self) -> *const c_char {
        self.name.as_ptr()
    }

    unsafe fn GetFirstClass(&self, iter: &mut c_uint) -> *const sScrClassDesc {
        *iter = 0;
        if *iter < self.classes.len() as u32 {
            return &self.classes[*iter as usize];
        }

        null()
    }

    unsafe fn GetNextClass(&self, iter: &mut c_uint) -> *const sScrClassDesc {
        *iter += 1;
        if *iter < self.classes.len() as u32 {
            return &self.classes[*iter as usize];
        }

        null()
    }

    unsafe fn EndClassIter(&self, _: &mut c_uint) {}
}

pub trait DarkScript: Sized
where
    IScript: From<Self>,
    Self: Default,
{
    fn get_desc(mod_name: &str) -> sScrClassDesc;
    extern "C" fn factory(_name: *const c_char, _id: c_int) -> *mut IScript;
}

#[unsafe(no_mangle)]
extern "system" fn ScriptModuleInit(raw_name: *const c_char, script_manager: IScriptMan, _: *mut i32, malloc: IMalloc, out_mod: *mut *mut c_void) -> i32 {
    malloc::init(malloc);
    services_init(script_manager);

    unsafe {
        let mut test_mod = ScriptModule::new(CStr::from_ptr(raw_name).to_str().unwrap());
        match module_init(&mut test_mod) {
            Ok(_) => test_mod.register(out_mod).into(),
            Err(e) => {
                services().debug.print(e);
                false.into()
            }
        }
    }
}

unsafe extern "Rust" {
    fn module_init(module: &mut ScriptModule) -> std::result::Result<(), &'static str>;
}
