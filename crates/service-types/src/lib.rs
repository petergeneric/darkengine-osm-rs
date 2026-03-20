/// All script services exposed by the Dark Engine's IScriptMan::GetService.
macro_rules! script_services {
    ($($name:ident = $id:expr),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u16)]
        pub enum ScriptService {
            $($name = $id),*
        }

        impl ScriptService {
            pub const ALL: &[ScriptService] = &[$(Self::$name),*];

            pub fn from_name(name: &str) -> Option<Self> {
                match name {
                    $(stringify!($name) => Some(Self::$name),)*
                    _ => None,
                }
            }
        }
    }
}

script_services! {
    Null          = 0x0F2,
    Sound         = 0x0F1,
    ActReact      = 0x0F4,
    Locked        = 0x0FB,
    Puppet        = 0x0FD,
    Damage        = 0x0FE,
    Door          = 0x0F6,
    AI            = 0x0E5,
    Debug         = 0x0D7,
    Object        = 0x0DF,
    Property      = 0x0DA,
    Link          = 0x0EE,
    LinkTools     = 0x0EF,
    Key           = 0x10D,
    Weapon        = 0x10E,
    PickLock      = 0x111,
    Bow           = 0x115,
    Camera        = 0x140,
    Physics       = 0x141,
    DrkInv        = 0x150,
    Inventory     = 0x15E,
    Quest         = 0x152,
    DrkPowerups   = 0x153,
    PlayerLimbs   = 0x15D,
    AnimTexture   = 0x16A,
    Light         = 0x16C,
    Container     = 0x17D,
    DarkUI        = 0x19F,
    Data          = 0x1A0,
    DarkGame      = 0x1B4,
    ShockPsi      = 0x1D7,
    ShockObj      = 0x1D9,
    PGroup        = 0x1F8,
    ShockGame     = 0x108,
    ShockWeapon   = 0x213,
    ShockAI       = 0x21B,
    Networking    = 0x225,
    CD            = 0x226,
    Version       = 0x228,
    Engine        = 0x229,
    ShockOverlay  = 0x22A,
    DarkOverlay   = 0x22B,
}

impl ScriptService {
    /// Returns the short service ID (e.g. `0x17D` for Container).
    pub const fn id(self) -> u16 {
        self as u16
    }

    /// Returns the GUID as a hyphenated uppercase string,
    /// e.g. `"7D00017D-7BFD-134C-8348-00AA00A82B51"`.
    pub fn guid_string(self) -> String {
        let id = self as u32;
        let dword = ((id & 0xFF) << 24) | ((id & 0xFF00) << 16) | (id & 0xFFFF);
        let w1 = 0x7A80 + id;
        let w2 = 0x11CF + id;
        format!("{:08X}-{:04X}-{:04X}-8348-00AA00A82B51", dword, w1, w2,)
    }
}
