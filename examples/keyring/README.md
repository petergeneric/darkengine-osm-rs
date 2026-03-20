# Magic Keyring Example

This example is a simplified reimplementation of saracoth's [J4FKeyring](https://github.com/saracoth/newdark-mods/blob/main/sq_scripts/just4fun_keyring.nut) Squirrel mod in Rust using osm-rs.
It demonstrates how to write an osm that does something useful, with a helpful comparison to an existing .nut approach.

When the player frobs a locked door, chest, or container, the module automatically selects a matching key or lockpick from inventory (saving the user from manually cycling through their inventory).

This example also demonstrates good practise (prefixing archetypes, and detecting other mods and yielding if there is a conflict)

## Setup

1. Build: `cargo build --release -p keyring`
2. Copy `keyring.dll` to your Thief installation, renamed to `keyring.osm`
3. Add `keyring.dml` to your mod's DML chain

## How it works

Two scripts are distributed via DML metaproperties:

- **`KeyringTarget`** — attached to `Door`, `Locks`, and `Container` archetypes. Handles `FrobWorldEnd` (player frobs the locked object directly).
- **`KeyringSource`** — attached to `Lockpick` and `Key` archetypes. Handles `FrobToolEnd` (player uses a tool on a locked object, but it's the wrong one).

Both scripts share the same core logic: check if the frobbed object is locked, determine what it accepts (key region mask and/or pick bits), search the player's `Contains` links for a match, and select it via `DarkUIService::inv_select`.
