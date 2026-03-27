use std::{
    ffi::{CStr, CString, c_float, c_int, c_ulong},
    os::raw::{c_char, c_void},
    ptr::{null, null_mut},
    str::FromStr,
    sync::OnceLock,
};

use windows::{Win32::Foundation::S_FALSE, core::*};

use crate::{IScriptMan, LinkEntry, LinkId, LinkKind, MultiParm, MultiParmType, ObjectId, UnsafeSendSync, dark_engine_service, malloc, sLink, sMultiParm, sVector};

/// Dispatches a raw vtable call with version-dependent slot numbers.
///
/// Handles the common pattern of:
/// 1. Getting the raw COM `this` pointer
/// 2. Null-checking it (returning a fallback on null)
/// 3. Selecting the vtable slot based on [`game_version()`]
/// 4. Transmuting the vtable entry to the correct function signature
/// 5. Calling the function with the provided arguments
///
/// # Syntax
///
/// ```ignore
/// vtable_dispatch!(
///     self,                          // service wrapper with raw_this()
///     null_fallback: false,          // value to return if this is null
///     slots: { T1: 11, T2: 12 },    // vtable slot per game version
///     fn(c_int, *const c_char) -> c_int,  // function signature (excluding `this`)
///     obj.0, prop.as_ptr()           // arguments (excluding `this`)
/// )
/// ```
///
/// The `fn(...)` signature must NOT include the leading `*mut c_void` (`this`) parameter;
/// it is prepended automatically. The call is wrapped in `unsafe`.
///
/// Returns the raw return value from the vtable function.
macro_rules! vtable_dispatch {
    (
        $self:expr,
        null_fallback: $fallback:expr,
        slots: { T1: $t1:expr, T2: $t2:expr },
        fn( $($param_ty:ty),* $(,)? ) -> $ret_ty:ty,
        $($arg:expr),* $(,)?
    ) => {{
        let this = $self.raw_this();
        if this.is_null() {
            return $fallback;
        }
        let slot = match game_version() {
            GameVersion::T1 => $t1,
            GameVersion::T2 => $t2,
            GameVersion::SS2 => panic!(
                "{}::{} not supported on SS2",
                std::any::type_name::<Self>(),
                std::column!(), // breadcrumb — no macro way to capture method name
            ),
        };
        unsafe {
            let vtbl = *(this as *const *const *const c_void);
            let func: unsafe extern "system" fn(*mut c_void, $($param_ty),*) -> $ret_ty =
                std::mem::transmute(*vtbl.add(slot));
            func(this, $($arg),*)
        }
    }};
}

/// Which Dark Engine game is running, detected via gamesys fingerprinting.
/// See [`detect_game_version`] for details.
/// N.B. this library assumes the actual backing engine is always NewDark
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameVersion {
    /// Thief 1
    T1,
    /// Thief 2
    T2,
    /// System Shock 2
    SS2,
}

static GAME_VERSION: OnceLock<GameVersion> = OnceLock::new();

pub fn game_version() -> GameVersion {
    *GAME_VERSION.get().expect("Game version not set — call detect_game_version() from on_begin_script()")
}

/// Detect game by fingerprinting the gamesys via archetype names unique to each game.
/// Must be called after services are initialised (e.g. from `on_begin_script`).
/// Safe to call multiple times — only the first call takes effect.
///
/// Uses the object service to check for archetypes that exist only in specific games'
/// default gamesys data files:
/// - **Garrett** (-2099) exists in both Thief 1 and Thief 2 gamesys
/// - **MechCherub** (-5333) exists only in the Thief 2 gamesys
/// - **The Player** (-384) exists only in the System Shock 2 gamesys
///
/// This fingerprinting method is copied from saracoth,
/// See: <https://github.com/saracoth/newdark-mods>
pub fn detect_game_version() -> GameVersion {
    if let Some(&version) = GAME_VERSION.get() {
        return version;
    }

    let obj_svc = services().object.as_ref().expect("Object service unavailable — cannot detect game version");

    let version = if obj_svc.named("Garrett").is_some() {
        if obj_svc.named("MechCherub").is_some() { GameVersion::T2 } else { GameVersion::T1 }
    } else if obj_svc.named("The Player").is_some() {
        GameVersion::SS2
    } else {
        panic!("Unknown game — neither Garrett nor The Player found in gamesys");
    };

    let _ = GAME_VERSION.set(version);
    version
}

static SERVICES: OnceLock<UnsafeSendSync<Services>> = OnceLock::new();
static SCRIPT_MANAGER: OnceLock<UnsafeSendSync<IScriptMan>> = OnceLock::new();

fn script_manager() -> &'static IScriptMan {
    &SCRIPT_MANAGER.get().expect("ScriptManager hasn't been initialised.").0
}

pub fn set_timed_message(to: ObjectId, name: &str, time: u32, kind: i32, data: Option<MultiParm>) -> u32 {
    let name = CString::new(name).unwrap();
    let parm = data.map_or(
        sMultiParm {
            val: 0,
            t: MultiParmType::Int as c_int,
        },
        |mp| mp.into_raw(),
    );
    unsafe { script_manager().SetTimedMessage2(to.0, name.as_ptr(), time as c_ulong, kind, &parm) }
}

pub fn kill_timed_message(msg_id: u32) {
    unsafe { script_manager().KilTimedMessage(msg_id) }
}

pub struct Services {
    pub act_react: ActReactService,
    pub container: Option<ContainerService>,
    pub dark_overlay: Option<DarkOverlayService>,
    pub dark_ui: Option<DarkUIService>,
    pub data: Option<DataService>,
    pub debug: DebugService,
    pub engine: EngineService,
    pub key: Option<KeyService>,
    pub link: Option<LinkService>,
    pub link_tools: Option<LinkToolsService>,
    pub object: Option<ObjectService>,
    pub property: Option<PropertyService>,
    pub version: VersionService,
    pub weapon: Option<WeaponService>,
    pub damage: Option<DamageService>,
    pub dark_game: Option<DarkGameService>,
    pub inventory: Option<InventoryService>,
}

pub fn services() -> &'static Services {
    &SERVICES.get().expect("Services hasn't been initialised.").0
}

pub(crate) fn services_init(script_manager: IScriptMan) {
    let _ = SCRIPT_MANAGER.set(UnsafeSendSync(script_manager.clone()));
    let services = Services {
        act_react: ActReactService {
            service: get_service(&script_manager),
        },
        container: try_get_service(&script_manager).map(|s| ContainerService { service: s }),
        dark_overlay: try_get_service(&script_manager).map(|s| DarkOverlayService { service: s }),
        dark_ui: try_get_service(&script_manager).map(|s| DarkUIService { service: s }),
        data: try_get_service(&script_manager).map(|s| DataService { service: s }),
        debug: DebugService {
            service: get_service(&script_manager),
        },
        engine: EngineService {
            service: get_service(&script_manager),
        },
        key: try_get_service(&script_manager).map(|s| KeyService { service: s }),
        link: try_get_service(&script_manager).map(|s| LinkService { service: s }),
        link_tools: try_get_service(&script_manager).map(|s| LinkToolsService { service: s }),
        object: try_get_service(&script_manager).map(|s| ObjectService { service: s }),
        property: try_get_service(&script_manager).map(|s| PropertyService { service: s }),
        version: VersionService {
            service: get_service(&script_manager),
        },
        weapon: try_get_service(&script_manager).map(|s| WeaponService { service: s }),
        damage: try_get_service(&script_manager).map(|s| DamageService { service: s }),
        dark_game: try_get_service(&script_manager).map(|s| DarkGameService { service: s }),
        // IInventory is not in the script service table; acquired via QI on
        // IScriptMan, which delegates to the app aggregate through COM aggregation.
        inventory: try_qi_service(&script_manager).map(|s| InventoryService { service: s }),
    };
    let _ = SERVICES.set(UnsafeSendSync(services));
}

fn get_service<T: Interface>(script_manager: &IScriptMan) -> T {
    unsafe { script_manager.GetService(&T::IID).cast::<T>().unwrap() }
}

fn try_get_service<T: Interface>(script_manager: &IScriptMan) -> Option<T> {
    unsafe { script_manager.GetService(&T::IID).cast::<T>().ok() }
}

/// Acquire a COM interface via QueryInterface on the script manager directly.
/// Some interfaces are only reachable this way (not registered in the service table).
fn try_qi_service<T: Interface>(script_manager: &IScriptMan) -> Option<T> {
    script_manager.cast::<T>().ok()
}

// Vtable layout matches T1.
// T2 divergence: T2 has a single Stimulate with 4 params at slot [6] instead of the
// legacy 3-param StimulateLegacy at [6]. The 4-param Stimulate is at slot [14] on T1.
// Use raw vtable dispatch for Stimulate — see ActReactService::stimulate().
// StimulateLegacy is kept as a vtable placeholder; removing it would shift later slots.
#[dark_engine_service(ActReact)]
unsafe trait IActReactService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn React(
        &self,
        what: c_int,
        stim_intensity: c_int,
        target: c_int,
        agent: c_int,
        parm1: *const sMultiParm,
        parm2: *const sMultiParm,
        parm3: *const sMultiParm,
        parm4: *const sMultiParm,
        parm5: *const sMultiParm,
        parm6: *const sMultiParm,
        parm7: *const sMultiParm,
        parm8: *const sMultiParm,
    ) -> HRESULT;
    fn StimulateLegacy(&self, who: c_int, what: c_int, how_much: c_float) -> HRESULT;
    fn GetReactionNamed(&self, name: *const c_char) -> c_int;
    // GetReactionName — aggregate return (string), hidden retval_ptr
    fn GetReactionName(&self, retval: *mut *const c_char, id: c_int) -> *mut *const c_char;
    fn SubscribeToStimulus(&self, obj: c_int, what: c_int) -> HRESULT;
    fn UnsubscribeToStimulus(&self, obj: c_int, what: c_int) -> HRESULT;
    fn BeginContact(&self, source: c_int, sensor: c_int) -> HRESULT;
    fn EndContact(&self, source: c_int, sensor: c_int) -> HRESULT;
    fn SetSingleSensorContact(&self, source: c_int, sensor: c_int) -> HRESULT;
}

pub struct ActReactService {
    service: IActReactService,
}

impl ActReactService {
    fn raw_this(&self) -> *mut c_void {
        self.service.as_raw()
    }

    pub fn react(
        &self,
        what: ObjectId,
        stim_intensity: ObjectId,
        target: Option<ObjectId>,
        agent: Option<ObjectId>,
        parm1: Option<sMultiParm>,
        parm2: Option<sMultiParm>,
        parm3: Option<sMultiParm>,
        parm4: Option<sMultiParm>,
        parm5: Option<sMultiParm>,
        parm6: Option<sMultiParm>,
        parm7: Option<sMultiParm>,
        parm8: Option<sMultiParm>,
    ) -> Result<()> {
        unsafe {
            self.service.React(
                what.0,
                stim_intensity.0,
                target.map_or(0, |o| o.0),
                agent.map_or(0, |o| o.0),
                match parm1 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm2 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm3 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm4 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm5 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm6 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm7 {
                    Some(p) => &p,
                    None => null(),
                },
                match parm8 {
                    Some(p) => &p,
                    None => null(),
                },
            )
        }
        .ok()
    }

    /// Stimulate with 4 params: slot 14 on T1 (past the legacy 3-param version), slot 6 on T2.
    pub fn stimulate(&self, who: ObjectId, what: ObjectId, how_much: f32, source: Option<ObjectId>) -> Result<()> {
        vtable_dispatch!(
            self,
            null_fallback: HRESULT(1).ok(),
            slots: { T1: 14, T2: 6 },
            fn(c_int, c_int, c_float, c_int) -> HRESULT,
            who.0, what.0, how_much, source.map_or(0, |o| o.0)
        )
        .ok()
    }

    #[must_use]
    pub fn get_reaction_named(&self, name: &str) -> Option<i32> {
        let name = CString::new(name).unwrap();
        let id = unsafe { self.service.GetReactionNamed(name.as_ptr()) };
        if id != 0 { Some(id) } else { None }
    }

    #[must_use]
    pub fn get_reaction_name(&self, id: i32) -> Option<String> {
        let mut name: *const c_char = null();
        unsafe { self.service.GetReactionName(&mut name, id) };
        if name.is_null() {
            return None;
        }
        let value = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        unsafe { malloc::free(name as *const c_void) };
        if value.is_empty() { None } else { Some(value) }
    }

    pub fn subscribe_to_stimulus(&self, obj: ObjectId, what: ObjectId) -> Result<()> {
        unsafe { self.service.SubscribeToStimulus(obj.0, what.0) }.ok()
    }

    pub fn unsubscribe_to_stimulus(&self, obj: ObjectId, what: ObjectId) -> Result<()> {
        unsafe { self.service.UnsubscribeToStimulus(obj.0, what.0) }.ok()
    }

    pub fn begin_contact(&self, source: ObjectId, sensor: ObjectId) -> Result<()> {
        unsafe { self.service.BeginContact(source.0, sensor.0) }.ok()
    }

    pub fn end_contact(&self, source: ObjectId, sensor: ObjectId) -> Result<()> {
        unsafe { self.service.EndContact(source.0, sensor.0) }.ok()
    }

    pub fn set_single_sensor_contact(&self, source: ObjectId, sensor: ObjectId) -> Result<()> {
        unsafe { self.service.SetSingleSensorContact(source.0, sensor.0) }.ok()
    }
}

// Parameters below are `&mut *const c_char` due to windows-rs COM binding generation.
// The engine treats these as `const char*` by-value (8 variadic string slots) —
// the `&mut` is not semantically meaningful.
#[dark_engine_service(Debug)]
unsafe trait IDebugService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn MPrint(
        &self,
        s1: &mut *const c_char,
        s2: &mut *const c_char,
        s3: &mut *const c_char,
        s4: &mut *const c_char,
        s5: &mut *const c_char,
        s6: &mut *const c_char,
        s7: &mut *const c_char,
        s8: &mut *const c_char,
    ) -> HRESULT;
    fn Command(
        &self,
        s1: &mut *const c_char,
        s2: &mut *const c_char,
        s3: &mut *const c_char,
        s4: &mut *const c_char,
        s5: &mut *const c_char,
        s6: &mut *const c_char,
        s7: &mut *const c_char,
        s8: &mut *const c_char,
    ) -> HRESULT;
    fn Break(&self) -> HRESULT;
    fn Log(
        &self,
        s1: &mut *const c_char,
        s2: &mut *const c_char,
        s3: &mut *const c_char,
        s4: &mut *const c_char,
        s5: &mut *const c_char,
        s6: &mut *const c_char,
        s7: &mut *const c_char,
        s8: &mut *const c_char,
    ) -> HRESULT;
}

pub struct DebugService {
    service: IDebugService,
}

impl DebugService {
    pub fn print(&self, msg: &str) {
        let s1 = CString::new(msg).unwrap();
        let s = CString::from(c"");
        unsafe {
            let _ = self.service.MPrint(
                &mut s1.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
            );
        }
    }

    pub fn command(&self, cmd: &str) -> HRESULT {
        let s1 = CString::new(cmd).unwrap();
        let s = CString::from(c"");
        unsafe {
            self.service.Command(
                &mut s1.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
            )
        }
    }

    pub fn log(&self, msg: &str) {
        let s1 = CString::new(msg).unwrap();
        let s = CString::from(c"");
        unsafe {
            let _ = self.service.Log(
                &mut s1.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
                &mut s.as_ptr(),
            );
        }
    }

    pub fn breakpoint(&self) {
        let _ = unsafe { self.service.Break() };
    }
}

#[dark_engine_service(Engine)]
unsafe trait IEngineService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn ConfigIsDefined(&self, name: *const c_char) -> BOOL;
    fn ConfigGetInt(&self, name: *const c_char, value: *mut c_int) -> BOOL;
    fn ConfigGetFloat(&self, name: *const c_char, value: *mut c_float) -> BOOL;
    fn ConfigGetRaw(&self, name: *const c_char, value: *mut *mut c_char) -> BOOL;
    fn BindingGetFloat(&self, name: *const c_char) -> c_float;
    fn FindFileInPath(&self, path_config_var: *const c_char, filename: *const c_char, fullname: *mut *mut c_char) -> BOOL;
    fn IsRunningDX6(&self) -> BOOL;
    fn GetCanvasSize(&self, width: *mut c_int, height: *mut c_int);
    fn GetAspectRatio(&self) -> c_float;
    fn GetFog(&self, r: *mut c_int, g: *mut c_int, b: *mut c_int, dist: *mut c_float);
    fn SetFog(&self, r: c_int, g: c_int, b: c_int, dist: c_float);
    fn GetFogZone(&self, zone: c_int, r: *mut c_int, g: *mut c_int, b: *mut c_int, dist: *mut c_float);
    fn SetFogZone(&self, zone: c_int, r: c_int, g: c_int, b: c_int, dist: c_float);
    fn GetWeather(
        &self,
        precip_type: *mut c_int,
        precip_freq: *mut c_float,
        precip_speed: *mut c_float,
        vis_dist: *mut c_float,
        rend_radius: *mut c_float,
        alpha: *mut c_float,
        brightness: *mut c_float,
        snow_jitter: *mut c_float,
        rain_len: *mut c_float,
        splash_freq: *mut c_float,
        splash_radius: *mut c_float,
        splash_height: *mut c_float,
        splash_duration: *mut c_float,
        texture: *mut *mut c_char,
        wind: *mut sVector,
    );
    fn SetWeather(
        &self,
        precip_type: c_int,
        precip_freq: c_float,
        precip_speed: c_float,
        vis_dist: c_float,
        rend_radius: c_float,
        alpha: c_float,
        brightness: c_float,
        snow_jitter: c_float,
        rain_len: c_float,
        splash_freq: c_float,
        splash_radius: c_float,
        splash_height: c_float,
        splash_duration: c_float,
        texture: *const c_char,
        wind: *const sVector,
    );
}

pub struct FogSettings {
    pub r: i32,
    pub g: i32,
    pub b: i32,
    pub distance: f32,
}

pub struct WeatherSettings {
    pub precipitation_type: i32,
    pub precipitation_frequency: f32,
    pub precipitation_speed: f32,
    pub visibility_distance: f32,
    pub render_radius: f32,
    pub alpha: f32,
    pub brightness: f32,
    pub snow_jitter: f32,
    pub rain_length: f32,
    pub splash_frequency: f32,
    pub splash_radius: f32,
    pub splash_height: f32,
    pub splash_duration: f32,
    pub texture: String,
    pub wind: sVector,
}

pub struct EngineService {
    service: IEngineService,
}

impl EngineService {
    #[must_use]
    pub fn config_is_defined(&self, name: &str) -> bool {
        let name = CString::from_str(name).unwrap();
        unsafe { self.service.ConfigIsDefined(name.as_ptr()).into() }
    }

    #[must_use]
    pub fn config_get_int(&self, name: &str) -> Option<i32> {
        let name = CString::from_str(name).unwrap();
        let mut value = 0;
        match unsafe { self.service.ConfigGetInt(name.as_ptr(), &mut value).into() } {
            true => Some(value),
            false => None,
        }
    }

    #[must_use]
    pub fn config_get_float(&self, name: &str) -> Option<f32> {
        let name = CString::from_str(name).unwrap();
        let mut value = 0.0;
        match unsafe { self.service.ConfigGetFloat(name.as_ptr(), &mut value).into() } {
            true => Some(value),
            false => None,
        }
    }

    #[must_use]
    pub fn config_get_raw(&self, name: &str) -> Option<String> {
        let name = CString::from_str(name).unwrap();
        let mut ptr = null_mut();
        unsafe {
            let val = match self.service.ConfigGetRaw(name.as_ptr(), &mut ptr).into() {
                true => Some(CStr::from_ptr(ptr).to_string_lossy().into_owned()),
                false => None,
            };
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn binding_get_float(&self, name: &str) -> f32 {
        let name = CString::from_str(name).unwrap();
        unsafe { self.service.BindingGetFloat(name.as_ptr()) }
    }

    #[must_use]
    pub fn find_file_in_path(&self, path_config_var: &str, filename: &str) -> Option<String> {
        let path_config_var = CString::from_str(path_config_var).unwrap();
        let filename = CString::from_str(filename).unwrap();
        let mut ptr = null_mut();
        unsafe {
            let val = match self.service.FindFileInPath(path_config_var.as_ptr(), filename.as_ptr(), &mut ptr).into() {
                true => Some(CStr::from_ptr(ptr).to_string_lossy().into_owned()),
                false => None,
            };
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn is_running_dx6(&self) -> bool {
        unsafe { self.service.IsRunningDX6().into() }
    }

    #[must_use]
    pub fn get_canvas_size(&self) -> ScreenSize {
        let mut s = ScreenSize { width: 0, height: 0 };
        unsafe { self.service.GetCanvasSize(&mut s.width, &mut s.height) };
        s
    }

    #[must_use]
    pub fn get_aspect_ratio(&self) -> f32 {
        unsafe { self.service.GetAspectRatio() }
    }

    #[must_use]
    pub fn get_fog(&self) -> FogSettings {
        let mut r = 0;
        let mut g = 0;
        let mut b = 0;
        let mut distance = 0.0;
        unsafe { self.service.GetFog(&mut r, &mut g, &mut b, &mut distance) };
        FogSettings { r, g, b, distance }
    }

    pub fn set_fog(&self, fog: &FogSettings) {
        unsafe { self.service.SetFog(fog.r, fog.g, fog.b, fog.distance) };
    }

    #[must_use]
    pub fn get_fog_zone(&self, zone: i32) -> FogSettings {
        let mut r = 0;
        let mut g = 0;
        let mut b = 0;
        let mut distance = 0.0;
        unsafe { self.service.GetFogZone(zone, &mut r, &mut g, &mut b, &mut distance) };
        FogSettings { r, g, b, distance }
    }

    pub fn set_fog_zone(&self, zone: i32, fog: &FogSettings) {
        unsafe { self.service.SetFogZone(zone, fog.r, fog.g, fog.b, fog.distance) };
    }

    #[must_use]
    pub fn get_weather(&self) -> WeatherSettings {
        let mut precipitation_type = 0;
        let mut precipitation_frequency = 0.0;
        let mut precipitation_speed = 0.0;
        let mut visibility_distance = 0.0;
        let mut render_radius = 0.0;
        let mut alpha = 0.0;
        let mut brightness = 0.0;
        let mut snow_jitter = 0.0;
        let mut rain_length = 0.0;
        let mut splash_frequency = 0.0;
        let mut splash_radius = 0.0;
        let mut splash_height = 0.0;
        let mut splash_duration = 0.0;
        let mut texture_ptr = null_mut();
        let mut wind = sVector { x: 0.0, y: 0.0, z: 0.0 };
        unsafe {
            self.service.GetWeather(
                &mut precipitation_type,
                &mut precipitation_frequency,
                &mut precipitation_speed,
                &mut visibility_distance,
                &mut render_radius,
                &mut alpha,
                &mut brightness,
                &mut snow_jitter,
                &mut rain_length,
                &mut splash_frequency,
                &mut splash_radius,
                &mut splash_height,
                &mut splash_duration,
                &mut texture_ptr,
                &mut wind,
            );
            let val = WeatherSettings {
                precipitation_type,
                precipitation_frequency,
                precipitation_speed,
                visibility_distance,
                render_radius,
                alpha,
                brightness,
                snow_jitter,
                rain_length,
                splash_frequency,
                splash_radius,
                splash_height,
                splash_duration,
                texture: CStr::from_ptr(texture_ptr).to_string_lossy().into_owned(),
                wind,
            };
            malloc::free(texture_ptr as *const c_void);
            val
        }
    }

    pub fn set_weather(&self, weather: &WeatherSettings) {
        unsafe {
            let texture = CString::from_str(&weather.texture).unwrap();
            self.service.SetWeather(
                weather.precipitation_type,
                weather.precipitation_frequency,
                weather.precipitation_speed,
                weather.visibility_distance,
                weather.render_radius,
                weather.alpha,
                weather.brightness,
                weather.snow_jitter,
                weather.rain_length,
                weather.splash_frequency,
                weather.splash_radius,
                weather.splash_height,
                weather.splash_duration,
                texture.as_ptr(),
                &weather.wind,
            );
        }
    }
}

#[dark_engine_service(Version)]
unsafe trait IVersionService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn GetAppName(&self, title_only: BOOL, app_name: &mut *mut c_char);
    fn GetVersion(&self, major: &mut c_int, minor: &mut c_int);
    fn IsEditor(&self) -> c_int;
    fn GetGame(&self, game: &mut *mut c_char);
    fn GetGamsys(&self, gamsys: &mut *mut c_char);
    fn GetMap(&self, map: &mut *mut c_char);
    fn GetCurrentFM(&self, current_fm: &mut *mut c_char) -> HRESULT;
    fn GetCurrentFMPath(&self, current_fm_path: &mut *mut c_char) -> HRESULT;
    fn FMizeRelativePath(&self, in_path: *const c_char, out_path: &mut *mut c_char);
    fn FMizePath(&self, in_path: *const c_char, out_path: &mut *mut c_char);
}

pub struct VersionService {
    service: IVersionService,
}

impl VersionService {
    #[must_use]
    pub fn get_app_name(&self, title_only: bool) -> String {
        let mut ptr = null_mut();
        unsafe {
            self.service.GetAppName(title_only.into(), &mut ptr);
            let val = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn get_version(&self) -> (i32, i32) {
        let mut major = 0;
        let mut minor = 0;
        unsafe { self.service.GetVersion(&mut major, &mut minor) };
        (major, minor)
    }

    #[must_use]
    pub fn is_editor(&self) -> bool {
        unsafe { self.service.IsEditor() != 0 }
    }

    #[must_use]
    pub fn get_game(&self) -> String {
        let mut ptr = null_mut();
        unsafe {
            self.service.GetGame(&mut ptr);
            let val = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn get_gamsys(&self) -> String {
        let mut ptr = null_mut();
        unsafe {
            self.service.GetGamsys(&mut ptr);
            let val = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn get_map(&self) -> String {
        let mut ptr = null_mut();
        unsafe {
            self.service.GetMap(&mut ptr);
            let val = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn get_current_fm(&self) -> Option<String> {
        let mut ptr = null_mut();
        let result = unsafe { self.service.GetCurrentFM(&mut ptr) };
        let fm = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { malloc::free(ptr as *const c_void) };

        match HRESULT::is_ok(result) && result != S_FALSE {
            true => Some(fm),
            false => None,
        }
    }

    #[must_use]
    pub fn get_current_fm_path(&self) -> Option<String> {
        let mut ptr = null_mut();
        let result = unsafe { self.service.GetCurrentFMPath(&mut ptr) };
        let fm_path = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        unsafe { malloc::free(ptr as *const c_void) };

        match HRESULT::is_ok(result) && result != S_FALSE {
            true => Some(fm_path),
            false => None,
        }
    }

    #[must_use]
    pub fn fmize_relative_path(&self, path: &str) -> String {
        let path = CString::from_str(path).unwrap();
        let mut ptr = null_mut();
        unsafe {
            self.service.FMizeRelativePath(path.as_ptr(), &mut ptr);
            let val = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            malloc::free(ptr as *const c_void);
            val
        }
    }

    #[must_use]
    pub fn fmize_path(&self, path: &str) -> String {
        let path = CString::from_str(path).unwrap();
        let mut ptr = null_mut();
        unsafe {
            self.service.FMizePath(path.as_ptr(), &mut ptr);
            let val = CStr::from_ptr(ptr).to_string_lossy().into_owned();
            malloc::free(ptr as *const c_void);
            val
        }
    }
}

// ---- DarkOverlay Service (0x22b) ----

#[dark_engine_service(DarkOverlay)]
unsafe trait IDarkOverlayService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn SetHandler(&self, handler: *mut c_void);
    fn GetBitmap(&self, name: *const c_char, path: *const c_char) -> c_int;
    fn FlushBitmap(&self, handle: c_int);
    fn GetBitmapSize(&self, handle: c_int, width: *mut c_int, height: *mut c_int);
    fn WorldToScreen(&self, pos: *const sVector, x: *mut c_int, y: *mut c_int) -> BOOL;
    fn GetObjectScreenBounds(&self, obj: *const c_int, x1: *mut c_int, y1: *mut c_int, x2: *mut c_int, y2: *mut c_int) -> BOOL;
    fn CreateTOverlayItem(&self, x: c_int, y: c_int, width: c_int, height: c_int, alpha: c_int, trans_bg: BOOL) -> c_int;
    fn CreateTOverlayItemFromBitmap(&self, x: c_int, y: c_int, alpha: c_int, bm_handle: c_int, trans_bg: BOOL) -> c_int;
    fn DestroyTOverlayItem(&self, handle: c_int);
    fn UpdateTOverlayAlpha(&self, handle: c_int, alpha: c_int);
    fn UpdateTOverlayPosition(&self, handle: c_int, x: c_int, y: c_int);
    fn UpdateTOverlaySize(&self, handle: c_int, width: c_int, height: c_int);
    fn DrawBitmap(&self, handle: c_int, x: c_int, y: c_int);
    fn DrawSubBitmap(&self, handle: c_int, x: c_int, y: c_int, src_x: c_int, src_y: c_int, src_width: c_int, src_height: c_int);
    fn SetTextColorFromStyle(&self, style_color: c_int);
    fn SetTextColor(&self, r: c_int, g: c_int, b: c_int);
    fn GetStringSize(&self, text: *const c_char, width: *mut c_int, height: *mut c_int);
    fn DrawString(&self, text: *const c_char, x: c_int, y: c_int);
    fn DrawLine(&self, x1: c_int, y1: c_int, x2: c_int, y2: c_int);
    fn FillTOverlay(&self, color_idx: c_int, alpha: c_int);
    fn BeginTOverlayUpdate(&self, handle: c_int) -> BOOL;
    fn EndTOverlayUpdate(&self);
    fn DrawTOverlayItem(&self, handle: c_int);
}

/// Handle to a bitmap loaded via [`DarkOverlayService::get_bitmap`].
///
/// Max 128 bitmaps can be loaded at once; cleared on db reset.
/// See `darkoverlay.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BitmapHandle(pub i32);

/// Handle to a transparent overlay item created via
/// [`DarkOverlayService::create_t_overlay_item`] or
/// [`DarkOverlayService::create_t_overlay_item_from_bitmap`].
///
/// Max 64 overlays can be created at once; cleared on db reset.
/// See `darkoverlay.h`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OverlayHandle(pub i32);

/// Overlay alpha value (0 = fully transparent, 255 = fully opaque).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Alpha(pub u8);

impl Alpha {
    pub const TRANSPARENT: Self = Self(0);
    pub const OPAQUE: Self = Self(255);
}

/// An RGB color value
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const WHITE: Self = Self { r: 255, g: 255, b: 255 };
    pub const BLACK: Self = Self { r: 0, g: 0, b: 0 };
}

/// A 2D screen-space point in pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// A 2D point within a bitmap, relative to its upper-left origin.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BitmapPoint {
    pub x: i32,
    pub y: i32,
}

/// A 2D size in pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenSize {
    pub width: i32,
    pub height: i32,
}

/// A 2D screen-space bounding rectangle in pixels.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScreenRect {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl ScreenRect {
    #[must_use]
    pub fn width(&self) -> i32 {
        self.x2 - self.x1
    }

    #[must_use]
    pub fn height(&self) -> i32 {
        self.y2 - self.y1
    }

    #[must_use]
    pub fn size(&self) -> ScreenSize {
        ScreenSize {
            width: self.width(),
            height: self.height(),
        }
    }
}

pub struct DarkOverlayService {
    service: IDarkOverlayService,
}

impl DarkOverlayService {
    /// Sets the current overlay handler. Only one handler can be active at a time;
    /// pass null to clear.
    ///
    /// # Safety
    /// `handler` must be a valid `IDarkOverlayHandler` pointer, or null.
    /// See `darkoverlay.h` — `SetHandler`.
    pub unsafe fn set_handler(&self, handler: *mut c_void) {
        unsafe { self.service.SetHandler(handler) };
    }

    /// Loads a bitmap for HUD drawing. `path` is the resource directory
    /// (e.g. `"intrface\\"`).
    ///
    /// Returns a [`BitmapHandle`], or `BitmapHandle(-1)` on failure.
    /// Max 128 bitmaps; cleared on db reset.
    /// See `darkoverlay.h` — `GetBitmap`.
    #[must_use]
    pub fn get_bitmap(&self, name: &str, path: &str) -> BitmapHandle {
        let name = CString::new(name).unwrap();
        let path = CString::new(path).unwrap();
        BitmapHandle(unsafe { self.service.GetBitmap(name.as_ptr(), path.as_ptr()) })
    }

    /// Discards a loaded bitmap, freeing its slot. Only needed when using many
    /// bitmaps to stay below the 128 limit.
    /// See `darkoverlay.h` — `FlushBitmap`.
    pub fn flush_bitmap(&self, handle: BitmapHandle) {
        unsafe { self.service.FlushBitmap(handle.0) };
    }

    /// Returns the `(width, height)` of a loaded bitmap.
    /// See `darkoverlay.h` — `GetBitmapSize`.
    #[must_use]
    pub fn get_bitmap_size(&self, handle: BitmapHandle) -> ScreenSize {
        let mut s = ScreenSize { width: 0, height: 0 };
        unsafe { self.service.GetBitmapSize(handle.0, &mut s.width, &mut s.height) };
        s
    }

    /// Maps a 3D world position to 2D screen coordinates.
    ///
    /// Returns `None` if the point is behind the camera or off-screen.
    ///
    /// May **only** be called inside `DrawHUD` or `DrawTOverlay` handlers.
    /// See `darkoverlay.h` — `WorldToScreen`.
    #[must_use]
    pub fn world_to_screen(&self, pos: &sVector) -> Option<ScreenPoint> {
        let mut p = ScreenPoint { x: 0, y: 0 };
        let ok: bool = unsafe { self.service.WorldToScreen(pos, &mut p.x, &mut p.y).into() };
        if ok { Some(p) } else { None }
    }

    /// Gets the 2D screen-space bounding rectangle of an object.
    ///
    /// Returns `None` if the object is entirely off-screen.
    ///
    /// May **only** be called inside `DrawHUD` or `DrawTOverlay` handlers.
    /// See `darkoverlay.h` — `GetObjectScreenBounds`.
    #[must_use]
    pub fn get_object_screen_bounds(&self, obj: ObjectId) -> Option<ScreenRect> {
        let mut r = ScreenRect { x1: 0, y1: 0, x2: 0, y2: 0 };
        let ok: bool = unsafe { self.service.GetObjectScreenBounds(&obj.0, &mut r.x1, &mut r.y1, &mut r.x2, &mut r.y2).into() };
        if ok { Some(r) } else { None }
    }

    /// Creates a transparent overlay item from a loaded bitmap.
    ///
    /// Unlike [`create_t_overlay_item`](Self::create_t_overlay_item), the overlay is sized
    /// automatically to match the bitmap and does not require `Begin/EndTOverlayUpdate`
    /// content updates. `alpha` is 0–255. Max 64 overlays; cleared on db reset.
    ///
    /// Returns the overlay handle, or `OverlayHandle(-1)` if `bitmap_handle` is invalid.
    /// See `darkoverlay.h` — `CreateTOverlayItemFromBitmap`.
    #[must_use]
    pub fn create_t_overlay_item_from_bitmap(&self, pos: ScreenPoint, alpha: Alpha, bitmap_handle: BitmapHandle, transparent_bg: bool) -> OverlayHandle {
        OverlayHandle(unsafe { self.service.CreateTOverlayItemFromBitmap(pos.x, pos.y, alpha.0 as c_int, bitmap_handle.0, transparent_bg.into()) })
    }

    /// Creates a transparent overlay item. Its contents must be filled via
    /// [`begin_t_overlay_update`](Self::begin_t_overlay_update) /
    /// [`end_t_overlay_update`](Self::end_t_overlay_update) and draw calls.
    /// `alpha` is 0–255. Max 64 overlays; cleared on db reset.
    ///
    /// Returns the overlay handle, or `OverlayHandle(-1)` on failure.
    /// See `darkoverlay.h` — `CreateTOverlayItem`.
    #[must_use]
    pub fn create_t_overlay_item(&self, pos: ScreenPoint, size: ScreenSize, alpha: Alpha, transparent_bg: bool) -> OverlayHandle {
        OverlayHandle(unsafe { self.service.CreateTOverlayItem(pos.x, pos.y, size.width, size.height, alpha.0 as c_int, transparent_bg.into()) })
    }

    /// Destroys a transparent overlay item, freeing its resources and slot.
    /// Not strictly necessary — all overlays are cleared on db reset.
    /// See `darkoverlay.h` — `DestroyTOverlayItem`.
    pub fn destroy_t_overlay_item(&self, handle: OverlayHandle) {
        unsafe { self.service.DestroyTOverlayItem(handle.0) };
    }

    /// Changes the alpha (0–255) of a transparent overlay.
    /// See `darkoverlay.h` — `UpdateTOverlayAlpha`.
    pub fn update_t_overlay_alpha(&self, handle: OverlayHandle, alpha: Alpha) {
        unsafe { self.service.UpdateTOverlayAlpha(handle.0, alpha.0 as c_int) };
    }

    /// Moves a transparent overlay to a new screen position.
    /// See `darkoverlay.h` — `UpdateTOverlayPosition`.
    pub fn update_t_overlay_position(&self, handle: OverlayHandle, pos: ScreenPoint) {
        unsafe { self.service.UpdateTOverlayPosition(handle.0, pos.x, pos.y) };
    }

    /// Changes the display size of a transparent overlay (for scaled items).
    /// See `darkoverlay.h` — `UpdateTOverlaySize`.
    pub fn update_t_overlay_size(&self, handle: OverlayHandle, size: ScreenSize) {
        unsafe { self.service.UpdateTOverlaySize(handle.0, size.width, size.height) };
    }

    /// Draws a loaded bitmap at the given position, unscaled.
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `DrawBitmap`.
    pub fn draw_bitmap(&self, handle: BitmapHandle, pos: ScreenPoint) {
        unsafe { self.service.DrawBitmap(handle.0, pos.x, pos.y) };
    }

    /// Draws a sub-rectangle of a loaded bitmap. `src_origin` and `src_size`
    /// define the region within the bitmap to draw (relative to its upper-left corner).
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `DrawSubBitmap`.
    pub fn draw_sub_bitmap(&self, handle: BitmapHandle, pos: ScreenPoint, src_origin: BitmapPoint, src_size: ScreenSize) {
        unsafe { self.service.DrawSubBitmap(handle.0, pos.x, pos.y, src_origin.x, src_origin.y, src_size.width, src_size.height) };
    }

    /// Sets the current text color from an `eStyleColorKind` value.
    /// Has no effect on `FONTT_FLAT8` fonts (color is baked into the font).
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `SetTextColorFromStyle`.
    pub fn set_text_color_from_style(&self, style_color: i32) {
        unsafe { self.service.SetTextColorFromStyle(style_color) };
    }

    /// Sets the current text color with explicit RGB values.
    /// Has no effect on `FONTT_FLAT8` fonts (color is baked into the font).
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `SetTextColor`.
    pub fn set_text_color(&self, color: Rgb) {
        unsafe { self.service.SetTextColor(color.r as c_int, color.g as c_int, color.b as c_int) };
    }

    /// Returns the `(width, height)` of a text string with the current font.
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `GetStringSize`.
    #[must_use]
    pub fn get_string_size(&self, text: &str) -> ScreenSize {
        let text = CString::new(text).unwrap();
        let mut s = ScreenSize { width: 0, height: 0 };
        unsafe { self.service.GetStringSize(text.as_ptr(), &mut s.width, &mut s.height) };
        s
    }

    /// Draws a text string at the given position with the current font and text color.
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `DrawString`.
    pub fn draw_string(&self, text: &str, pos: ScreenPoint) {
        let text = CString::new(text).unwrap();
        unsafe { self.service.DrawString(text.as_ptr(), pos.x, pos.y) };
    }

    /// Draws a line with the current text color.
    ///
    /// May **only** be called inside `DrawHUD` or a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `DrawLine`.
    pub fn draw_line(&self, from: ScreenPoint, to: ScreenPoint) {
        unsafe { self.service.DrawLine(from.x, from.y, to.x, to.y) };
    }

    /// Fills a transparent overlay with a palette index color (0 = black).
    /// `alpha` sets the alpha component of the image data (normally 255) — not to
    /// be confused with the overlay-level alpha from `create_t_overlay_item`,
    /// which is applied on top of this image data alpha.
    ///
    /// May **only** be called inside a `Begin/EndTOverlayUpdate` pair.
    /// See `darkoverlay.h` — `FillTOverlay`.
    pub fn fill_t_overlay(&self, color_idx: i32, alpha: Alpha) {
        unsafe { self.service.FillTOverlay(color_idx, alpha.0 as c_int) };
    }

    /// Begins updating a transparent overlay's contents. Draw calls after this
    /// (and before [`end_t_overlay_update`](Self::end_t_overlay_update)) render
    /// into the overlay. Returns `false` if the overlay cannot be updated.
    ///
    /// Constant updating is not optimal for large overlays — only update when
    /// contents have changed.
    ///
    /// May **only** be called inside the `DrawTOverlay` handler.
    /// See `darkoverlay.h` — `BeginTOverlayUpdate`.
    #[must_use]
    pub fn begin_t_overlay_update(&self, handle: OverlayHandle) -> bool {
        unsafe { self.service.BeginTOverlayUpdate(handle.0).into() }
    }

    /// Ends a transparent overlay content update. May only be called after a
    /// successful [`begin_t_overlay_update`](Self::begin_t_overlay_update).
    /// See `darkoverlay.h` — `EndTOverlayUpdate`.
    pub fn end_t_overlay_update(&self) {
        unsafe { self.service.EndTOverlayUpdate() };
    }

    /// Draws a transparent overlay item to the screen.
    ///
    /// May **only** be called inside the `DrawTOverlay` handler.
    /// See `darkoverlay.h` — `DrawTOverlayItem`.
    pub fn draw_t_overlay_item(&self, handle: OverlayHandle) {
        unsafe { self.service.DrawTOverlayItem(handle.0) };
    }
}

// ---- DarkUI Service (0x19f) ----

#[dark_engine_service(DarkUI)]
unsafe trait IDarkUIService: IUnknown {
    fn Init(&self);
    fn End(&self);
    // [5] TextMessage — returns long
    fn TextMessage(&self, message: *const c_char, color: c_int, timeout: c_int) -> HRESULT;
    // [6] ReadBook — returns long
    fn ReadBook(&self, text: *const c_char, art: *const c_char) -> HRESULT;
    // [7] InvItem — aggregate return (object), hidden retval_ptr
    fn InvItem(&self, retval: *mut c_int) -> *mut c_int;
    // [8] InvWeapon — aggregate return (object), hidden retval_ptr
    fn InvWeapon(&self, retval: *mut c_int) -> *mut c_int;
    // [9] InvSelect — returns long
    fn InvSelect(&self, obj: c_int) -> HRESULT;
    // [10] IsCommandBound — aggregate return (true_bool), hidden retval_ptr
    fn IsCommandBound(&self, retval: *mut c_int, cmd: *const c_char) -> *mut c_int;
    // [11] DescribeKeyBinding — aggregate return (string), hidden retval_ptr
    fn DescribeKeyBinding(&self, retval: *mut *const c_char, cmd: *const c_char) -> *mut *const c_char;
}

pub struct DarkUIService {
    service: IDarkUIService,
}

impl DarkUIService {
    /// Displays a text message on the HUD.
    /// See `darkui.h` — `TextMessage`.
    pub fn text_message(&self, msg: &str, color: i32, timeout: i32) -> Result<()> {
        let msg = CString::new(msg).unwrap();
        unsafe { self.service.TextMessage(msg.as_ptr(), color, timeout) }.ok()
    }

    /// Displays a book/scroll/note UI with the given text and art resource.
    /// See `darkui.h` — `ReadBook`.
    pub fn read_book(&self, text: &str, art: &str) -> Result<()> {
        let text = CString::new(text).unwrap();
        let art = CString::new(art).unwrap();
        unsafe { self.service.ReadBook(text.as_ptr(), art.as_ptr()) }.ok()
    }

    /// Returns the currently selected inventory item, or `None` if nothing is selected.
    /// See `darkui.h` — `InvItem`.
    #[must_use]
    pub fn inv_item(&self) -> Option<ObjectId> {
        let mut result: c_int = 0;
        unsafe { self.service.InvItem(&mut result) };
        (result != 0).then_some(ObjectId(result))
    }

    /// Returns the currently selected inventory weapon, or `None` if nothing is selected.
    /// Will never return ObjectId(0)
    /// See `darkui.h` — `InvWeapon`.
    #[must_use]
    pub fn inv_weapon(&self) -> Option<ObjectId> {
        let mut result: c_int = 0;
        unsafe { self.service.InvWeapon(&mut result) };
        (result != 0).then_some(ObjectId(result))
    }

    /// Selects an inventory object (weapon or item) by its object ID.
    /// N.B. no-op if passed ObjectId(0), you need to call InventoryService.inv_clear
    /// See `darkui.h` — `InvSelect`.
    pub fn inv_select(&self, obj: ObjectId) -> Result<()> {
        unsafe { self.service.InvSelect(obj.0) }.ok()
    }

    /// Checks whether a command has a key binding.
    /// See `darkui.h` — `IsCommandBound`.
    #[must_use]
    pub fn is_command_bound(&self, cmd: &str) -> bool {
        let cmd = CString::new(cmd).unwrap();
        let mut result: c_int = 0;
        unsafe { self.service.IsCommandBound(&mut result, cmd.as_ptr()) };
        result != 0
    }

    /// Returns the human-readable key binding string for a command,
    /// for help/tutorial overlay
    /// or `None` if the command is not bound.
    /// See `darkui.h` — `DescribeKeyBinding`.
    #[must_use]
    pub fn describe_key_binding(&self, cmd: &str) -> Option<String> {
        let cmd = CString::new(cmd).unwrap();
        let mut ptr: *const c_char = null();
        unsafe {
            self.service.DescribeKeyBinding(&mut ptr, cmd.as_ptr());
            let val = if ptr.is_null() {
                None
            } else {
                let s = CStr::from_ptr(ptr).to_string_lossy().into_owned();
                if s.is_empty() { None } else { Some(s) }
            };
            if !ptr.is_null() {
                malloc::free(ptr as *const c_void);
            }
            val
        }
    }
}

// ---- Link Service (0xee) ----

#[dark_engine_service(Link)]
unsafe trait ILinkService: IUnknown {
    fn Init(&self);
    fn End(&self);
    // [5] Create — aggregate return (link), hidden retval_ptr
    fn Create(&self, retval: *mut c_int, kind: c_int, from: c_int, to: c_int) -> *mut c_int;
    // [6] Destroy — returns long
    fn Destroy(&self, link_id: c_int) -> HRESULT;
    // [7] AnyExist — aggregate return (true_bool), hidden retval_ptr
    fn AnyExist(&self, retval: *mut c_int, kind: c_int, from: c_int, to: c_int) -> *mut c_int;
    // [8] GetAll — aggregate return (linkset), called via raw vtable in get_all()
    fn GetAllPlaceholder(&self, retval: *mut *mut c_void, kind: c_int, from: c_int, to: c_int) -> *mut *mut c_void;
    // [9] GetOne — aggregate return (link), hidden retval_ptr
    fn GetOne(&self, retval: *mut c_int, kind: c_int, from: c_int, to: c_int) -> *mut c_int;
    // [10] BroadcastOnAllLinks — returns long (NO data param)
    fn BroadcastOnAllLinks(&self, self_obj: *const c_int, message: *const c_char, recipients: c_int) -> HRESULT;
    // [11] BroadcastOnAllLinksData — returns long (WITH data param)
    fn BroadcastOnAllLinksData(&self, self_obj: *const c_int, message: *const c_char, recipients: c_int, linkdata: *const sMultiParm) -> HRESULT;
    // [12] CreateMany — returns long
    fn CreateMany(&self, kind: c_int, from_set: *const c_char, to_set: *const c_char) -> HRESULT;
    // [13] DestroyMany — returns long
    fn DestroyMany(&self, kind: c_int, from_set: *const c_char, to_set: *const c_char) -> HRESULT;
    // [14] GetAllInherited — aggregate return (linkset), hidden retval_ptr
    fn GetAllInheritedPlaceholder(&self, retval: *mut *mut c_void, kind: c_int, from: c_int, to: c_int) -> *mut *mut c_void;
    // [15] GetAllInheritedSingle — aggregate return (linkset), hidden retval_ptr
    fn GetAllInheritedSinglePlaceholder(&self, retval: *mut *mut c_void, kind: c_int, from: c_int, to: c_int) -> *mut *mut c_void;
}

/// Raw vtable for ILinkQuery COM iterator returned by Link.GetAll().
/// Vtable order from NVScript lg/links.h: Done, Link, ID, Data, Next, Inverse
#[repr(C)]
struct ILinkQueryVtbl {
    // IUnknown [0-2]
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    // ILinkQuery [3-8]
    done: unsafe extern "system" fn(*mut c_void) -> c_int,
    link: unsafe extern "system" fn(*mut c_void, *mut c_void) -> c_int,
    id: unsafe extern "system" fn(*mut c_void) -> c_int,
    data: unsafe extern "system" fn(*mut c_void) -> *const c_void,
    next: unsafe extern "system" fn(*mut c_void) -> c_int,
    inverse: unsafe extern "system" fn(*mut c_void) -> *mut c_void,
}

/// Raw vtable for ILinkService — needed because GetAll/GetAllInherited return linkset
/// (non-trivial C++ struct) which uses MSVC hidden return parameter convention.
#[repr(C)]
struct ILinkServiceVtbl {
    // IUnknown [0-2]
    query_interface: *const c_void,
    add_ref: *const c_void,
    release: *const c_void,
    // ILinkService [3-4]
    init: *const c_void,
    end: *const c_void,
    // [5] Create
    create: *const c_void,
    // [6] Destroy
    destroy: *const c_void,
    // [7] AnyExist
    any_exist: *const c_void,
    // [8] GetAll — returns linkset via hidden retval param
    get_all: unsafe extern "system" fn(*mut c_void, *mut *mut c_void, c_int, c_int, c_int) -> *mut c_void,
    // [9] GetOne
    get_one: *const c_void,
    // [10] BroadcastOnAllLinks
    broadcast_on_all_links: *const c_void,
    // [11] BroadcastOnAllLinksData
    broadcast_on_all_links_data: *const c_void,
    // [12] CreateMany
    create_many: *const c_void,
    // [13] DestroyMany
    destroy_many: *const c_void,
    // [14] GetAllInherited — returns linkset via hidden retval param
    get_all_inherited: unsafe extern "system" fn(*mut c_void, *mut *mut c_void, c_int, c_int, c_int) -> *mut c_void,
    // [15] GetAllInheritedSingle — returns linkset via hidden retval param
    get_all_inherited_single: unsafe extern "system" fn(*mut c_void, *mut *mut c_void, c_int, c_int, c_int) -> *mut c_void,
}

pub struct LinkService {
    service: ILinkService,
}

/// Drain an ILinkQuery COM iterator into a Vec<LinkEntry>, then release it.
///
/// Each iteration calls both `id()` and `link()` to capture the link instance ID
/// along with the full link data (source, dest, flavor).
///
/// # Safety
/// `query_ptr` must be a valid ILinkQuery COM object pointer, or null.
unsafe fn drain_link_query(query_ptr: *mut c_void) -> Vec<LinkEntry> {
    let mut result = Vec::new();
    if query_ptr.is_null() {
        return result;
    }
    unsafe {
        let qvtbl_ptr = *(query_ptr as *const *const ILinkQueryVtbl);
        let qvtbl = &*qvtbl_ptr;
        while (qvtbl.done)(query_ptr) == 0 {
            let link_id = (qvtbl.id)(query_ptr);
            let mut link = sLink {
                source: ObjectId(0),
                dest: ObjectId(0),
                flavor: LinkKind(0),
            };
            (qvtbl.link)(query_ptr, &mut link as *mut sLink as *mut c_void);
            result.push(LinkEntry {
                id: LinkId(link_id),
                source: link.source,
                dest: link.dest,
                flavor: link.flavor,
            });
            (qvtbl.next)(query_ptr);
        }
        (qvtbl.release)(query_ptr);
    }
    result
}

impl LinkService {
    fn raw_this(&self) -> *mut c_void {
        self.service.as_raw()
    }

    /// Call a linkset-returning vtable slot and drain the resulting ILinkQuery.
    fn call_linkset_slot(
        &self,
        slot_fn: impl FnOnce(&ILinkServiceVtbl) -> unsafe extern "system" fn(*mut c_void, *mut *mut c_void, c_int, c_int, c_int) -> *mut c_void,
        kind: LinkKind,
        from: ObjectId,
        to: ObjectId,
    ) -> Vec<LinkEntry> {
        let this = self.raw_this();
        if this.is_null() {
            return Vec::new();
        }
        unsafe {
            let vtbl = &**(this as *const *const ILinkServiceVtbl);
            let mut query_ptr: *mut c_void = null_mut();
            (slot_fn(vtbl))(this, &mut query_ptr, kind.0, from.0, to.0);
            drain_link_query(query_ptr)
        }
    }

    #[must_use]
    pub fn any_exist(&self, kind: LinkKind, from: ObjectId, to: ObjectId) -> bool {
        let mut result: c_int = 0;
        unsafe { self.service.AnyExist(&mut result, kind.0, from.0, to.0) };
        result != 0
    }

    #[must_use]
    pub fn get_one(&self, kind: LinkKind, from: ObjectId, to: ObjectId) -> Option<LinkId> {
        let mut result: c_int = 0;
        unsafe { self.service.GetOne(&mut result, kind.0, from.0, to.0) };
        if result != 0 { Some(LinkId(result)) } else { None }
    }

    /// Get all matching links via raw vtable call (hidden return param for linkset).
    #[must_use]
    pub fn get_all(&self, kind: LinkKind, from: ObjectId, to: ObjectId) -> Vec<LinkEntry> {
        self.call_linkset_slot(|vtbl| vtbl.get_all, kind, from, to)
    }

    /// Get all matching links including those inherited from archetypes and metaproperties.
    #[must_use]
    pub fn get_all_inherited(&self, kind: LinkKind, from: ObjectId, to: ObjectId) -> Vec<LinkEntry> {
        self.call_linkset_slot(|vtbl| vtbl.get_all_inherited, kind, from, to)
    }

    /// Get all matching links including those inherited via direct archetype chain only
    /// (no metaproperty inheritance).
    #[must_use]
    pub fn get_all_inherited_single(&self, kind: LinkKind, from: ObjectId, to: ObjectId) -> Vec<LinkEntry> {
        self.call_linkset_slot(|vtbl| vtbl.get_all_inherited_single, kind, from, to)
    }
}

// ---- Object Service (0xdf) ----
//
// Vtable layout matches T1.
// T2 divergence: T2 inserts IsPositionValid and FindClosestObjectNamed at slots [22-23],
// shifting everything after Teleport[21] by +2. Archetype is slot [28] on T1, [30] on T2.
// Use raw vtable dispatch for Archetype — see ObjectService::archetype().
//
// Many IObjectSrv methods return aggregate types (object, true_bool, string, vector)
// which use MSVC hidden return parameter convention. These are marked with comments below.
// Methods we actually call use raw vtable calls to handle the hidden parameter correctly.

#[dark_engine_service(Object)]
unsafe trait IObjectService: IUnknown {
    // [3] Init, [4] End
    fn Init(&self);
    fn End(&self);
    // [5] BeginCreate — aggregate return (object), hidden retval_ptr
    fn BeginCreate(&self, retval: *mut c_int, archetype: c_int) -> *mut c_int;
    // [6] EndCreate — returns long
    fn EndCreate(&self, obj: c_int) -> HRESULT;
    // [7] Create — aggregate return (object), hidden retval_ptr
    fn Create(&self, retval: *mut c_int, archetype: c_int) -> *mut c_int;
    // [8] Destroy — returns long
    fn Destroy(&self, obj: c_int) -> HRESULT;
    // [9] Exists — aggregate return (true_bool), hidden retval_ptr
    fn Exists(&self, retval: *mut c_int, obj: c_int) -> *mut c_int;
    // [10] SetName — returns long
    fn SetName(&self, obj: c_int, name: *const c_char) -> HRESULT;
    // [11] GetName — aggregate return (string), hidden retval_ptr
    fn GetName(&self, retval: *mut *const c_char, obj: c_int) -> *mut *const c_char;
    // [12] Named — aggregate return (object), hidden retval_ptr
    fn Named(&self, retval: *mut c_int, name: *const c_char) -> *mut c_int;
    // [13-14] AddMetaProperty, RemoveMetaProperty — return long
    fn AddMetaProperty(&self, obj: c_int, mp: c_int) -> HRESULT;
    fn RemoveMetaProperty(&self, obj: c_int, mp: c_int) -> HRESULT;
    // [15] HasMetaProperty — aggregate return (true_bool), hidden retval_ptr
    fn HasMetaProperty(&self, retval: *mut c_int, obj: c_int, mp: c_int) -> *mut c_int;
    // [16] InheritsFrom — aggregate return (true_bool), hidden retval_ptr
    fn InheritsFrom(&self, retval: *mut c_int, obj: c_int, archetype: c_int) -> *mut c_int;
    // [17] IsTransient — aggregate return (true_bool), hidden retval_ptr
    fn IsTransient(&self, retval: *mut c_int, obj: c_int) -> *mut c_int;
    // [18] SetTransience — returns long
    fn SetTransience(&self, obj: c_int, transient: c_int) -> HRESULT;
    // [19] Position — aggregate return (vector), hidden retval_ptr
    fn Position(&self, retval: *mut sVector, obj: c_int) -> *mut sVector;
    // [20] Facing — aggregate return (vector), hidden retval_ptr
    fn Facing(&self, retval: *mut sVector, obj: c_int) -> *mut sVector;
    // [21] Teleport — returns long
    fn Teleport(&self, obj: c_int, pos: *const sVector, facing: *const sVector, rel: c_int);
    // [22-23] T2-only IsPositionValid, FindClosestObjectNamed — NOT present in T1
    // [22] AddMetaPropertyToMany — returns int
    fn AddMetaPropertyToMany(&self, mp: c_int, set: *const c_char) -> c_int;
    // [23] RemoveMetaPropertyFromMany — returns int
    fn RemoveMetaPropertyFromMany(&self, mp: c_int, set: *const c_char) -> c_int;
    // [24] RenderedThisFrame — aggregate return (true_bool), hidden retval_ptr
    fn RenderedThisFrame(&self, retval: *mut c_int, obj: c_int) -> *mut c_int;
    // [25] ObjectToWorld — aggregate return (vector), hidden retval_ptr
    fn ObjectToWorld(&self, retval: *mut sVector, obj: c_int, pt: *const sVector) -> *mut sVector;
    // [26] WorldToObject — aggregate return (vector), hidden retval_ptr
    fn WorldToObject(&self, retval: *mut sVector, obj: c_int, pt: *const sVector) -> *mut sVector;
    // [27] CalcRelTransform — writes to output pointers, no aggregate return
    fn CalcRelTransform(&self, obj: c_int, rel: c_int, pos: *mut sVector, facing: *mut sVector);
    // [28] Archetype — aggregate return (object), hidden retval_ptr
    fn Archetype(&self, retval: *mut c_int, obj: c_int) -> *mut c_int;
}

pub struct ObjectService {
    service: IObjectService,
}

impl ObjectService {
    fn raw_this(&self) -> *mut c_void {
        self.service.as_raw()
    }

    #[must_use]
    pub fn exists(&self, obj: ObjectId) -> bool {
        let mut result: c_int = 0;
        unsafe { self.service.Exists(&mut result, obj.0) };
        result != 0
    }

    #[must_use]
    pub fn get_name(&self, obj: ObjectId) -> Option<String> {
        let mut name: *const c_char = null();
        unsafe { self.service.GetName(&mut name, obj.0) };
        if name.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        // String is allocated by the engine; free it
        unsafe { crate::malloc::free(name as *mut c_void) };
        if s.is_empty() { None } else { Some(s) }
    }

    /// Archetype: slot 28 on T1, slot 30 on T2 (T2 inserts 2 extra methods at [22-23]).
    #[must_use]
    pub fn archetype(&self, obj: ObjectId) -> Option<ObjectId> {
        let mut result: c_int = 0;
        vtable_dispatch!(
            self,
            null_fallback: None,
            slots: { T1: 28, T2: 30 },
            fn(*mut c_int, c_int) -> *mut c_int,
            &mut result, obj.0
        );
        (result != 0).then_some(ObjectId(result))
    }

    #[must_use]
    pub fn inherits_from(&self, obj: ObjectId, archetype: ObjectId) -> bool {
        let mut result: c_int = 0;
        unsafe { self.service.InheritsFrom(&mut result, obj.0, archetype.0) };
        result != 0
    }

    #[must_use]
    pub fn named(&self, name: &str) -> Option<ObjectId> {
        let name = CString::new(name).unwrap();
        let mut result: c_int = 0;
        unsafe { self.service.Named(&mut result, name.as_ptr()) };
        (result != 0).then_some(ObjectId(result))
    }

    #[must_use]
    pub fn has_meta_property(&self, obj: ObjectId, mp: ObjectId) -> bool {
        let mut result: c_int = 0;
        unsafe { self.service.HasMetaProperty(&mut result, obj.0, mp.0) };
        result != 0
    }
}

// ---- LinkTools Service (0xef) ----

#[dark_engine_service(LinkTools)]
unsafe trait ILinkToolsService: IUnknown {
    fn Init(&self);
    fn End(&self);
    // [5] LinkKindNamed — returns long
    fn LinkKindNamed(&self, name: *const c_char) -> c_int;
    // [6] LinkKindName — aggregate return (string), hidden retval_ptr
    fn LinkKindName(&self, retval: *mut *const c_char, kind: c_int) -> *mut *const c_char;
    // [7] LinkGet — returns long
    fn LinkGet(&self, link_id: c_int, link: *mut sLink) -> HRESULT;
    // [8] LinkGetData — aggregate return (cMultiParm), hidden retval_ptr
    fn LinkGetData(&self, retval: *mut sMultiParm, link_id: c_int, field: *const c_char) -> *mut sMultiParm;
    // [9] LinkSetData — returns long
    fn LinkSetData(&self, link_id: c_int, field: *const c_char, data: *const sMultiParm) -> HRESULT;
}

pub struct LinkToolsService {
    service: ILinkToolsService,
}

impl LinkToolsService {
    #[must_use]
    pub fn link_kind_named(&self, name: &str) -> Option<LinkKind> {
        let name = CString::new(name).unwrap();
        let id = unsafe { self.service.LinkKindNamed(name.as_ptr()) };
        if id != 0 { Some(LinkKind(id)) } else { None }
    }

    /// Reverse lookup: returns the human-readable name for a link kind (e.g. "Contains", "~FrobProxy").
    #[must_use]
    pub fn link_kind_name(&self, kind: LinkKind) -> Option<String> {
        let mut ptr: *const c_char = null();
        unsafe { self.service.LinkKindName(&mut ptr, kind.0) };
        if ptr.is_null() {
            return None;
        }
        let name = unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        // String is allocated by the engine; free it
        unsafe { crate::malloc::free(ptr as *const c_void) };
        if name.is_empty() { None } else { Some(name) }
    }

    /// Read link data as an integer. Pass None for field to get the default/only data field.
    #[must_use]
    pub fn link_get_data_int(&self, link_id: LinkId, field: Option<&str>) -> i32 {
        let mut result = sMultiParm {
            val: 0,
            t: MultiParmType::Int as c_int,
        };
        let field_cstr;
        let field_ptr = match field {
            Some(f) => {
                field_cstr = CString::new(f).unwrap();
                field_cstr.as_ptr()
            }
            None => null(),
        };
        unsafe { self.service.LinkGetData(&mut result, link_id.0, field_ptr) };
        result.val
    }

    #[must_use]
    pub fn link_get(&self, link_id: LinkId) -> Option<sLink> {
        let mut link = sLink {
            source: ObjectId(0),
            dest: ObjectId(0),
            flavor: LinkKind(0),
        };
        let hr = unsafe { self.service.LinkGet(link_id.0, &mut link) };
        if HRESULT::is_ok(hr) { Some(link) } else { None }
    }
}

// ---- Property Service (0xda) ----
// NVScript uses IID_IPropertyScriptService = 0xDA, NOT 0xB4 (which is IID_IProperty, internal).
//
// Vtable layout matches T1.
// T2 divergence: T2 inserts SetLocal at slot [8], shifting Add/Remove/CopyFrom/Possessed by +1.
//   T1: Get[5] Set[6] SetSimple[7] Add[8] Remove[9] CopyFrom[10] Possessed[11]
//   T2: Get[5] Set[6] SetSimple[7] SetLocal[8] Add[9] Remove[10] CopyFrom[11] Possessed[12]
// Use raw vtable dispatch for Possessed — see PropertyService::possessed().

#[dark_engine_service(Property)]
unsafe trait IPropertyService: IUnknown {
    fn Init(&self);
    fn End(&self);
    // [5] Get — aggregate return (cMultiParm), hidden retval_ptr
    fn Get(&self, retval: *mut sMultiParm, obj: c_int, prop: *const c_char, subprop: *const c_char) -> *mut sMultiParm;
    // [6] Set — returns long
    fn Set(&self, obj: c_int, prop: *const c_char, subprop: *const c_char, value: *const sMultiParm) -> HRESULT;
    // [7] SetSimple — returns long
    fn SetSimple(&self, obj: c_int, prop: *const c_char, value: *const sMultiParm) -> HRESULT;
    // [8] Add — returns long
    fn Add(&self, obj: c_int, prop: *const c_char) -> HRESULT;
    // [9] Remove — returns long
    fn Remove(&self, obj: c_int, prop: *const c_char) -> HRESULT;
    // [10] CopyFrom — returns long
    fn CopyFrom(&self, dst: c_int, prop: *const c_char, src: c_int) -> HRESULT;
    // T1: [11] Possessed, T2: [12] Possessed (SetLocal at [8] shifts everything)
    // Use raw vtable dispatch — see PropertyService::possessed()
}

pub struct PropertyService {
    service: IPropertyService,
}

impl PropertyService {
    fn raw_this(&self) -> *mut c_void {
        self.service.as_raw()
    }

    /// Possessed — returns int (scalar, NOT aggregate).
    /// T1 slot 11, T2 slot 12 (SetLocal inserted at slot 8 in T2).
    #[must_use]
    pub fn possessed(&self, obj: ObjectId, prop: &str) -> bool {
        let prop = CString::new(prop).unwrap();
        vtable_dispatch!(
            self,
            null_fallback: false,
            slots: { T1: 11, T2: 12 },
            fn(c_int, *const c_char) -> c_int,
            obj.0, prop.as_ptr()
        ) != 0
    }

    /// PossessedSimple — checks local property only, ignoring archetype/metaproperty inheritance.
    /// NewDark extension. Slot 13 on both T1 and T2. Returns BOOL (scalar).
    #[must_use]
    pub fn possessed_simple(&self, obj: ObjectId, prop: &str) -> bool {
        let prop = CString::new(prop).unwrap();
        vtable_dispatch!(
            self,
            null_fallback: false,
            slots: { T1: 13, T2: 13 },
            fn(c_int, *const c_char) -> c_int,
            obj.0, prop.as_ptr()
        ) != 0
    }

    /// Get a property value as a typed [`MultiParm`].
    ///
    /// This is the unified getter — typed convenience methods (`get_int`, `get_str`)
    /// delegate to this internally. For string values, the engine-allocated buffer is
    /// copied and then freed via the engine's `IMalloc`.
    #[must_use]
    pub fn get(&self, obj: ObjectId, prop: &str, subprop: Option<&str>) -> Option<MultiParm> {
        if !self.possessed(obj, prop) {
            return None;
        }
        let mut result = sMultiParm {
            val: 0,
            t: MultiParmType::Int as c_int,
        };
        let prop = CString::new(prop).unwrap();
        let subprop_c = subprop.map(|s| CString::new(s).unwrap());
        let subprop_ptr = subprop_c.as_ref().map_or(null(), |s| s.as_ptr());
        unsafe {
            self.service.Get(&mut result, obj.0, prop.as_ptr(), subprop_ptr);
            let mp = MultiParm::from_raw(&result);
            // Free engine-allocated string buffer after copying
            if result.t == MultiParmType::String as c_int && result.val != 0 {
                crate::malloc::free(result.val as *const c_void);
            }
            Some(mp)
        }
    }

    #[must_use]
    pub fn get_int(&self, obj: ObjectId, prop: &str, subprop: &str) -> Option<i32> {
        match self.get(obj, prop, Some(subprop))? {
            MultiParm::Int(v) => Some(v),
            _ => None,
        }
    }

    /// Get a simple (non-structured) property value as integer. Passes NULL for subprop.
    #[must_use]
    pub fn get_simple_int(&self, obj: ObjectId, prop: &str) -> Option<i32> {
        match self.get(obj, prop, None)? {
            MultiParm::Int(v) => Some(v),
            _ => None,
        }
    }

    /// Get a structured property value as string.
    #[must_use]
    pub fn get_str(&self, obj: ObjectId, prop: &str, subprop: &str) -> Option<String> {
        match self.get(obj, prop, Some(subprop))? {
            MultiParm::String(s) => s.to_str().ok().map(|s| s.to_owned()),
            _ => None,
        }
    }

    /// Set a structured property value. Slot [6] on both T1 and T2.
    pub fn set(&self, obj: ObjectId, prop: &str, subprop: &str, value: &sMultiParm) -> Result<()> {
        let prop = CString::new(prop).unwrap();
        let subprop = CString::new(subprop).unwrap();
        unsafe { self.service.Set(obj.0, prop.as_ptr(), subprop.as_ptr(), value) }.ok()
    }

    /// Set a simple (non-structured) property value. Slot [7] on both T1 and T2.
    pub fn set_simple(&self, obj: ObjectId, prop: &str, value: &sMultiParm) -> Result<()> {
        let prop = CString::new(prop).unwrap();
        unsafe { self.service.SetSimple(obj.0, prop.as_ptr(), value) }.ok()
    }

    /// Set a structured property value as integer.
    pub fn set_int(&self, obj: ObjectId, prop: &str, subprop: &str, value: i32) -> Result<()> {
        let parm = sMultiParm {
            val: value,
            t: MultiParmType::Int as c_int,
        };
        self.set(obj, prop, subprop, &parm)
    }

    /// Set a simple (non-structured) property value as integer.
    pub fn set_simple_int(&self, obj: ObjectId, prop: &str, value: i32) -> Result<()> {
        let parm = sMultiParm {
            val: value,
            t: MultiParmType::Int as c_int,
        };
        self.set_simple(obj, prop, &parm)
    }

    /// Set a structured property value as string.
    pub fn set_str(&self, obj: ObjectId, prop: &str, subprop: &str, value: &str) -> Result<()> {
        const _: () = assert!(size_of::<*const c_char>() == size_of::<c_int>(), "sMultiParm.val is a c_int/pointer union; requires 32-bit target");
        let s = CString::new(value).unwrap();
        let parm = sMultiParm {
            val: s.as_ptr() as c_int,
            t: MultiParmType::String as c_int,
        };
        self.set(obj, prop, subprop, &parm)
    }

    /// SetLocal — set property locally (not inherited).
    /// T1 NewDark: slot [12], T2: slot [8].
    pub fn set_local(&self, obj: ObjectId, prop: &str, subprop: &str, value: &sMultiParm) -> Result<()> {
        let prop = CString::new(prop).unwrap();
        let subprop = CString::new(subprop).unwrap();
        vtable_dispatch!(
            self,
            null_fallback: HRESULT(1).ok(),
            slots: { T1: 12, T2: 8 },
            fn(c_int, *const c_char, *const c_char, *const sMultiParm) -> HRESULT,
            obj.0, prop.as_ptr(), subprop.as_ptr(), value
        )
        .ok()
    }

    /// Add a property to an object. T1 slot [8], T2 slot [9].
    pub fn add(&self, obj: ObjectId, prop: &str) -> Result<()> {
        let prop = CString::new(prop).unwrap();
        vtable_dispatch!(
            self,
            null_fallback: HRESULT(1).ok(),
            slots: { T1: 8, T2: 9 },
            fn(c_int, *const c_char) -> HRESULT,
            obj.0, prop.as_ptr()
        )
        .ok()
    }

    /// Remove a property from an object. T1 slot [9], T2 slot [10].
    pub fn remove(&self, obj: ObjectId, prop: &str) -> Result<()> {
        let prop = CString::new(prop).unwrap();
        vtable_dispatch!(
            self,
            null_fallback: HRESULT(1).ok(),
            slots: { T1: 9, T2: 10 },
            fn(c_int, *const c_char) -> HRESULT,
            obj.0, prop.as_ptr()
        )
        .ok()
    }

    /// Get the inventory type of an object from its `InvType` property.
    #[must_use]
    pub fn get_inv_type(&self, obj: ObjectId) -> Option<InventoryType> {
        let val = match self.get(obj, "InvType", None)? {
            MultiParm::Int(v) => v,
            _ => return None,
        };
        InventoryType::try_from(val).ok()
    }

    /// Copy a property from one object to another. T1 slot [10], T2 slot [11].
    pub fn copy_from(&self, dst: ObjectId, prop: &str, src: ObjectId) -> Result<()> {
        let prop = CString::new(prop).unwrap();
        vtable_dispatch!(
            self,
            null_fallback: HRESULT(1).ok(),
            slots: { T1: 10, T2: 11 },
            fn(c_int, *const c_char, c_int) -> HRESULT,
            dst.0, prop.as_ptr(), src.0
        )
        .ok()
    }
}

// ---- Key Service (0x10d) ----
// TryToUseKey takes C++ references (const int&), which are pointers at the ABI level.

/// How the key should be used when testing against a lock.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUseMode {
    Default = 0,
    Open = 1,
    Close = 2,
    Check = 3,
}

#[dark_engine_service(Key)]
unsafe trait IKeyService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn TryToUseKey(&self, key_obj: *const c_int, lock_obj: *const c_int, how: c_int) -> c_int;
}

pub struct KeyService {
    service: IKeyService,
}

impl KeyService {
    /// Check if a key fits a lock without consuming it.
    #[must_use]
    pub fn try_to_use_key(&self, key_obj: ObjectId, lock_obj: ObjectId, how: KeyUseMode) -> bool {
        let key = key_obj.0;
        let lock = lock_obj.0;
        unsafe { self.service.TryToUseKey(&key, &lock, how as c_int) != 0 }
    }
}

// ---- Weapon Service (0x10e) ----
// All methods return scalars (HRESULT or BOOL) — no aggregate return complications.
// Not available in System Shock 2.

/// Dark Engine weapon type (`eDarkWeaponType`), used by `WeaponService::equip`.
/// `Sword` covers all non-blackjack melee weapons (sword, dagger, etc.).
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeaponType {
    Sword = 0,
    Blackjack = 1,
}

impl TryFrom<i32> for WeaponType {
    type Error = i32;
    fn try_from(v: i32) -> std::result::Result<Self, i32> {
        match v {
            0 => Ok(Self::Sword),
            1 => Ok(Self::Blackjack),
            _ => Err(v),
        }
    }
}

#[dark_engine_service(Weapon)]
unsafe trait IWeaponService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn Equip(&self, weapon: c_int, weapon_type: c_int) -> HRESULT;
    fn UnEquip(&self, weapon: c_int) -> HRESULT;
    fn IsEquipped(&self, owner: c_int, weapon: c_int) -> BOOL;
    fn StartAttack(&self, owner: c_int, weapon: c_int) -> HRESULT;
    fn FinishAttack(&self, owner: c_int, weapon: c_int) -> HRESULT;
}

pub struct WeaponService {
    service: IWeaponService,
}

impl WeaponService {
    /// Equip a melee weapon. `weapon_type` specifies the category
    /// (`Sword` for all non-blackjack melee weapons, `Blackjack` for blackjacks).
    pub fn equip(&self, weapon: ObjectId, weapon_type: WeaponType) -> Result<()> {
        unsafe { self.service.Equip(weapon.0, weapon_type as c_int) }.ok()
    }

    /// Unequip a melee weapon.
    pub fn unequip(&self, weapon: ObjectId) -> Result<()> {
        unsafe { self.service.UnEquip(weapon.0) }.ok()
    }

    /// Check whether `owner` currently has `weapon` equipped.
    #[must_use]
    pub fn is_equipped(&self, owner: ObjectId, weapon: ObjectId) -> bool {
        unsafe { self.service.IsEquipped(owner.0, weapon.0).into() }
    }

    /// Begin a melee attack with `weapon` on behalf of `owner`.
    pub fn start_attack(&self, owner: ObjectId, weapon: ObjectId) -> Result<()> {
        unsafe { self.service.StartAttack(owner.0, weapon.0) }.ok()
    }

    /// End a melee attack with `weapon` on behalf of `owner`.
    pub fn finish_attack(&self, owner: ObjectId, weapon: ObjectId) -> Result<()> {
        unsafe { self.service.FinishAttack(owner.0, weapon.0) }.ok()
    }
}

// ---- Inventory Type (eInventoryType) ----

/// Dark Engine inventory type, from the `InvType` property.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryType {
    /// Not a real inventory item (carried objects, bodies, loot).
    Junk = 0,
    /// Usable inventory item (keys, potions, tools, etc.).
    Item = 1,
    /// Weapon (sword, blackjack, bow).
    Weapon = 2,
}

impl TryFrom<i32> for InventoryType {
    type Error = i32;
    fn try_from(v: i32) -> std::result::Result<Self, i32> {
        match v {
            0 => Ok(Self::Junk),
            1 => Ok(Self::Item),
            2 => Ok(Self::Weapon),
            _ => Err(v),
        }
    }
}

// ---- Container Service (0x17d) ----

/// Containment type returned by `ContainerService::is_held`.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainType {
    Generic = 0,
    Hand = 1,
    Belt = 2,
    Alt = 3,
}

impl TryFrom<i32> for ContainType {
    type Error = i32;
    fn try_from(v: i32) -> std::result::Result<Self, i32> {
        match v {
            0 => Ok(Self::Generic),
            1 => Ok(Self::Hand),
            2 => Ok(Self::Belt),
            3 => Ok(Self::Alt),
            _ => Err(v),
        }
    }
}

#[dark_engine_service(Container)]
unsafe trait IContainerService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn Add(&self, obj: c_int, container: c_int, contain_type: c_int, flags: c_int) -> HRESULT;
    fn Remove(&self, obj: c_int, container: c_int) -> HRESULT;
    fn MoveAllContents(&self, src: c_int, targ: c_int, flags: c_int) -> HRESULT;
    fn StackAdd(&self, src: c_int, quantity: c_int) -> HRESULT;
    fn IsHeld(&self, container: c_int, containee: c_int) -> c_int;
}

pub struct ContainerService {
    service: IContainerService,
}

impl ContainerService {
    /// Query containment type for a (container, containee) pair.
    /// Returns `None` if `containee` is not in `container` (engine returns -1).
    #[must_use]
    pub fn is_held(&self, container: ObjectId, containee: ObjectId) -> Option<ContainType> {
        let result = unsafe { self.service.IsHeld(container.0, containee.0) };
        ContainType::try_from(result).ok()
    }

    pub fn add(&self, obj: ObjectId, container: ObjectId, contain_type: ContainType, flags: i32) -> Result<()> {
        unsafe { self.service.Add(obj.0, container.0, contain_type as c_int, flags) }.ok()
    }

    pub fn remove(&self, obj: ObjectId, container: ObjectId) -> Result<()> {
        unsafe { self.service.Remove(obj.0, container.0) }.ok()
    }

    pub fn move_all_contents(&self, src: ObjectId, dst: ObjectId, flags: i32) -> Result<()> {
        unsafe { self.service.MoveAllContents(src.0, dst.0, flags) }.ok()
    }

    pub fn stack_add(&self, obj: ObjectId, quantity: i32) -> Result<()> {
        unsafe { self.service.StackAdd(obj.0, quantity) }.ok()
    }
}

// ---- INTERNAL Inventory Service (0x15e) ----
// This is an internal engine service, not a scripting service,
// so it needs to be fetched with `try_qi_service`
//
// N.B. more methods, didn't map anything over than inv_clear

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhichInvObj {
    CurrentWeapon = 0,
    CurrentItem = 1,
}

#[dark_engine_service(Inventory)]
unsafe trait IInventoryService: IUnknown {
    fn Init(&self);
    fn End(&self);
}

pub struct InventoryService {
    service: IInventoryService,
}

impl InventoryService {
    fn raw_this(&self) -> *mut c_void {
        self.service.as_raw()
    }

    /// Immediately clear the current weapon/item selection.
    /// Vtable slot 8: HRESULT InvClear(eWhichInvObj)
    pub fn inv_clear(&self, which: WhichInvObj) {
        let _ = vtable_dispatch!(
            self,
            null_fallback: (),
            slots: { T1: 8, T2: 8 },
            fn(c_int) -> HRESULT,
            which as c_int
        );
    }
}

// ---- DarkGame Service (0x1b4) ----
// Minimal definition — only needed for GUID generation via #[dark_engine_service].
// All actual calls use raw vtable dispatch.

#[dark_engine_service(DarkGame)]
unsafe trait IDarkGameService: IUnknown {
    fn Init(&self);
    fn End(&self);
}

pub struct DarkGameService {
    #[allow(dead_code)] // retained for future vtable dispatch methods
    service: IDarkGameService,
}

// ---- Damage Service (0xfe) ----
// Vtable: [3] Init, [4] End, [5] Damage, [6] Slay, [7] Resurrect, [8] Terminate
// TODO: verify Resurrect vtable slot on T1.
// TODO: Damage `damage_type` param is engine-internal; document known values.

#[dark_engine_service(Damage)]
unsafe trait IDamageService: IUnknown {
    fn Init(&self);
    fn End(&self);
    fn Damage(&self, victim: c_int, culprit: c_int, amount: c_int, damage_type: c_int) -> HRESULT;
    fn Slay(&self, victim: c_int, culprit: c_int) -> HRESULT;
    fn Resurrect(&self, victim: c_int, culprit: c_int) -> HRESULT;
    fn Terminate(&self, victim: c_int) -> HRESULT;
}

pub struct DamageService {
    service: IDamageService,
}

impl DamageService {
    /// Resurrect an object (restore to life, reset HP to MAX_HP).
    pub fn resurrect(&self, victim: ObjectId, culprit: ObjectId) -> Result<()> {
        unsafe { self.service.Resurrect(victim.0, culprit.0) }.ok()
    }

    /// Deal damage to an object.
    pub fn damage(&self, victim: ObjectId, culprit: ObjectId, amount: i32, damage_type: i32) -> Result<()> {
        unsafe { self.service.Damage(victim.0, culprit.0, amount, damage_type) }.ok()
    }

    /// Kill an object.
    pub fn slay(&self, victim: ObjectId, culprit: ObjectId) -> Result<()> {
        unsafe { self.service.Slay(victim.0, culprit.0) }.ok()
    }
}

// ---- Data Service (0x1a0) ----
// Vtable: [3] Init, [4] End, [5] GetString, [6] GetObjString, [7] DirectRand,
//         [8] RandInt, [9] RandFlt0to1, [10] RandFltNeg1to1

#[dark_engine_service(Data)]
unsafe trait IDataService: IUnknown {
    fn Init(&self);
    fn End(&self);
    // [5] GetString — aggregate return (string), hidden retval_ptr
    fn GetString(&self, retval: *mut *const c_char, table: *const c_char, name: *const c_char, def: *const c_char, relpath: *const c_char) -> *mut *const c_char;
    // [6] GetObjString — aggregate return (string), hidden retval_ptr
    fn GetObjString(&self, retval: *mut *const c_char, obj: c_int, table: *const c_char) -> *mut *const c_char;
}

pub struct DataService {
    service: IDataService,
}

impl DataService {
    /// Look up a localized string for an object from a named string table.
    ///
    /// For example, `get_obj_string(obj, "objnames")` resolves the object's `GameName`
    /// property (e.g. `name_compass`) through the `objnames.str` string table to produce
    /// the display name (e.g. `"Compass"`).
    #[must_use]
    pub fn get_obj_string(&self, obj: ObjectId, table: &str) -> Option<String> {
        let table = CString::new(table).ok()?;
        let mut result: *const c_char = null();
        unsafe { self.service.GetObjString(&mut result, obj.0, table.as_ptr()) };
        if result.is_null() {
            return None;
        }
        let s = unsafe { CStr::from_ptr(result).to_string_lossy().into_owned() };
        unsafe { crate::malloc::free(result as *mut c_void) };
        if s.is_empty() { None } else { Some(s) }
    }
}
