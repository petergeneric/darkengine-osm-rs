//! Keyring OSM — automatic key/lockpick selection for Thief 1/2.
//!
//! When the player frobs a locked door or chest, this module automatically
//! selects a matching key or lockpick from inventory.
//!
//! ## Setup
//!
//! Attach scripts via DML metaproperties (see `keyring.dml`):
//! - `XCHANGEX_KeyringTarget` on lockable objects (doors, chests)
//! - `XCHANGEX_KeyringSource` on keys and lockpicks
//!
//! Based on saracoth's J4FKeyring Squirrel mod, reimplemented in Rust.

use std::result::Result;
use std::str::FromStr;
use std::sync::atomic::{AtomicU8, Ordering};

use kc_osm::*;

kc_osm::unwind_resume_stub!();

// ---- J4F compatibility ----

/// 0 = not checked, 1 = our keyring active, 2 = J4F detected (disabled)
static J4F_STATE: AtomicU8 = AtomicU8::new(0);

/// Check if the original J4FKeyring mod is installed by looking for its metaproperties.
/// If any of them exist as archetypes, we disable our keyring for the rest of this run.
fn j4f_keyring_detected() -> bool {
    let state = J4F_STATE.load(Ordering::Relaxed);
    if state != 0 {
        return state == 2;
    }
    let obj_svc = match services().object.as_ref() {
        Some(s) => s,
        None => return false, // can't check yet, don't cache
    };
    let detected = obj_svc.named("J4FKeyringEnabled").is_some() || obj_svc.named("J4FKeyringSwapEnabled").is_some() || obj_svc.named("J4FKeyringTarget").is_some();
    if detected {
        services().debug.print("keyring: J4FKeyring mod detected, disabling our keyring");
    }
    J4F_STATE.store(if detected { 2 } else { 1 }, Ordering::Relaxed);
    detected
}

// ---- Helpers ----

fn get_display_name(obj: ObjectId) -> String {
    let obj_svc = match services().object.as_ref() {
        Some(s) => s,
        None => return format!("obj {}", obj),
    };
    if let Some(name) = obj_svc.get_name(obj) {
        return name;
    }
    if let Some(arch) = obj_svc.archetype(obj) {
        if arch != obj {
            if let Some(name) = obj_svc.get_name(arch) {
                return name;
            }
        }
    }
    format!("obj {}", obj)
}

fn get_link_dests(kind: LinkKind, from: ObjectId) -> Vec<ObjectId> {
    let link_svc = match services().link.as_ref() {
        Some(s) => s,
        None => return Vec::new(),
    };
    link_svc.get_all(kind, from, ObjectId(0)).into_iter().map(|entry| entry.dest).collect()
}

// ---- Key matching ----

fn is_key_match(key_obj: ObjectId, lock_obj: ObjectId) -> bool {
    if let Some(key_svc) = services().key.as_ref() {
        return key_svc.try_to_use_key(key_obj, lock_obj, KeyUseMode::Check);
    }

    // Fallback: compare KeySrc/KeyDst region masks and lock IDs manually.
    let prop = match services().property.as_ref() {
        Some(p) => p,
        None => return false,
    };
    let lock_region = prop.get_int(lock_obj, "KeyDst", "RegionMask").unwrap_or(0);
    let lock_id = prop.get_int(lock_obj, "KeyDst", "LockID").unwrap_or(0);
    let key_region = prop.get_int(key_obj, "KeySrc", "RegionMask").unwrap_or(0);
    let key_lock_id = prop.get_int(key_obj, "KeySrc", "LockID").unwrap_or(0);

    (key_region & lock_region) != 0 && key_lock_id == lock_id
}

/// Returns 0 if invalid, 1 for valid key, 2 for valid lockpick.
fn is_valid_tool(item: ObjectId, target: ObjectId, wants_key_region: i32, wants_picks: i32) -> i32 {
    let prop = match services().property.as_ref() {
        Some(p) => p,
        None => return 0,
    };

    if wants_key_region != 0 && prop.possessed(item, "KeySrc") && is_key_match(item, target) {
        return 1;
    }

    if wants_picks != 0 && prop.possessed(item, "PickSrc") {
        let pick_bits = prop.get_simple_int(item, "PickSrc").unwrap_or(0);
        if (pick_bits & wants_picks) != 0 {
            return 2;
        }
    }

    0
}

// ---- Core keyring logic ----

fn handle_lock_frob(self_obj: ObjectId, target_obj: ObjectId, using_tool: Option<ObjectId>, frobber: ObjectId) {
    if j4f_keyring_detected() {
        return;
    }

    let obj_svc = match services().object.as_ref() {
        Some(s) => s,
        None => return,
    };
    let prop = match services().property.as_ref() {
        Some(p) => p,
        None => return,
    };

    // Only care about player frobbing.
    let avatar = match obj_svc.named("Avatar") {
        Some(id) => id,
        None => return,
    };
    if frobber.0 < 1 || !obj_svc.inherits_from(frobber, avatar) {
        return;
    }

    // Only care about locked objects.
    if !prop.possessed(target_obj, "Locked") || prop.get_simple_int(target_obj, "Locked").unwrap_or(0) == 0 {
        return;
    }

    // What does the lock accept?
    let wants_key_region = if prop.possessed(target_obj, "KeyDst") {
        prop.get_int(target_obj, "KeyDst", "RegionMask").unwrap_or(0)
    } else {
        0
    };

    let wants_picks = if prop.possessed(target_obj, "PickCfg") {
        let tumbler = prop.get_int(target_obj, "PickState", "CurTumbler/State").unwrap_or(0);
        match tumbler {
            0 => prop.get_int(target_obj, "PickCfg", "LockBits 1").unwrap_or(0),
            1 => prop.get_int(target_obj, "PickCfg", "LockBits 2").unwrap_or(0),
            2 => prop.get_int(target_obj, "PickCfg", "LockBits 3").unwrap_or(0),
            _ => 0,
        }
    } else {
        0
    };

    if wants_picks == 0 && wants_key_region == 0 {
        return;
    }

    // If the current tool is already valid, don't switch.
    if let Some(tool) = using_tool {
        if is_valid_tool(tool, target_obj, wants_key_region, wants_picks) > 0 {
            return;
        }
    }

    // Search the player's inventory for a matching key or pick.
    let contains = match services().link_tools.as_ref().and_then(|lt| lt.link_kind_named("Contains")) {
        Some(id) => id,
        None => return,
    };

    let items = get_link_dests(contains, frobber);
    let mut found_item = ObjectId(0);
    let mut found_type = 0;

    for item_id in items {
        let validity = is_valid_tool(item_id, target_obj, wants_key_region, wants_picks);
        if validity > 0 {
            found_item = item_id;
            found_type = validity;
            if validity == 1 {
                break; // Prefer keys over picks.
            }
        }
    }

    if found_item.0 <= 0 {
        return;
    }

    let dark_ui = match services().dark_ui.as_ref() {
        Some(ui) => ui,
        None => return,
    };

    if dark_ui.inv_item().map_or(true, |cur| cur != found_item) {
        services().debug.print(&format!(
            "keyring: auto-select {} {} ({}) for lock {} ({})",
            if found_type == 1 { "key" } else { "pick" },
            found_item,
            get_display_name(found_item),
            target_obj,
            get_display_name(target_obj)
        ));
        set_timed_message(
            self_obj,
            "KeyringSelect",
            10,
            0, // kSTM_OneShot
            Some(MultiParm::from(found_item.0)),
        );
    }
}

fn keyring_on_timer(msg: &sScrTimerMsg) {
    if msg.timer_name() != "KeyringSelect" {
        return;
    }

    let found_item = ObjectId(msg.data.val);
    if found_item.0 <= 0 {
        return;
    }

    let dark_ui = match services().dark_ui.as_ref() {
        Some(ui) => ui,
        None => return,
    };

    if dark_ui.inv_item().map_or(true, |cur| cur != found_item) {
        let _ = dark_ui.inv_select(found_item);
    }
}

// ---- Scripts ----

/// Attached to lockable world objects (doors, chests, etc.).
/// Auto-selects a matching key/pick when the player frobs the object directly.
#[dark_script(FrobWorldEnd, Timer)]
pub struct KeyringTarget {}

impl KeyringTarget {
    pub fn on_frob_world_end(&self, _services: &Services, msg: &sFrobMsg, _reply: &mut sMultiParm) -> HRESULT {
        handle_lock_frob(msg.to, msg.to, None, msg.frobber);
        HRESULT(1)
    }

    pub fn on_timer(&self, _services: &Services, msg: &sScrTimerMsg, _reply: &mut sMultiParm) -> HRESULT {
        keyring_on_timer(msg);
        HRESULT(1)
    }
}

/// Attached to keys and lockpicks.
/// When the player uses an invalid tool on a locked object, auto-selects a better one.
#[dark_script(FrobToolEnd, Timer)]
pub struct KeyringSource {}

impl KeyringSource {
    pub fn on_frob_tool_end(&self, _services: &Services, msg: &sFrobMsg, _reply: &mut sMultiParm) -> HRESULT {
        handle_lock_frob(msg.to, msg.destobj, Some(msg.srcobj), msg.frobber);
        HRESULT(1)
    }

    pub fn on_timer(&self, _services: &Services, msg: &sScrTimerMsg, _reply: &mut sMultiParm) -> HRESULT {
        keyring_on_timer(msg);
        HRESULT(1)
    }
}

// ---- Module entry point ----

#[unsafe(no_mangle)]
pub extern "Rust" fn module_init(module: &mut ScriptModule) -> Result<(), &'static str> {
    module.register_script::<KeyringTarget>();
    module.register_script::<KeyringSource>();
    Ok(())
}
