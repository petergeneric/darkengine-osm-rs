use dark_engine_service_types::ScriptService;
use heck::ToSnakeCase;
use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, format_ident, quote};
use syn::{Ident, ItemStruct, Meta, parse_macro_input, punctuated::Punctuated};

/// Attribute macro for Dark Engine script service interfaces.
///
/// Takes a `ScriptService` variant name and computes the COM GUID, then
/// delegates to `#[interface("GUID")]` from the `windows` crate.
///
/// # Example
/// ```ignore
/// #[dark_engine_service(Container)]
/// unsafe trait IContainerService: IUnknown { ... }
/// ```
///
/// Expands to:
/// ```ignore
/// #[interface("7D00017D-7BFD-134C-8348-00AA00A82B51")]
/// unsafe trait IContainerService: IUnknown { ... }
/// ```
#[proc_macro_attribute]
pub fn dark_engine_service(attr: TokenStream, item: TokenStream) -> TokenStream {
    let ident: Ident = syn::parse(attr).expect("dark_engine_service expects a service name, e.g. Container");
    let svc = ScriptService::from_name(&ident.to_string()).unwrap_or_else(|| panic!("Unknown script service: `{ident}`. See ScriptService enum for valid names."));
    let guid = svc.guid_string();

    let item: TokenStream2 = item.into();
    quote! {
        #[interface(#guid)]
        #item
    }
    .into()
}

/// Returns the message type identifier for a given Dark Engine message name.
///
/// Frob messages (FrobWorldBegin/End, FrobToolBegin/End, FrobInvBegin/End) carry
/// extra fields in `sFrobMsg`. Timer messages carry the timer name in `sScrTimerMsg`.
/// All other messages use the base `sScrMsg`.
fn msg_type_for(message: &str) -> Ident {
    let type_name = match message {
        "FrobWorldBegin" | "FrobWorldEnd" | "FrobToolBegin" | "FrobToolEnd" | "FrobInvBegin" | "FrobInvEnd" => "sFrobMsg",
        "Timer" => "sScrTimerMsg",
        "DoorOpen" | "DoorClose" | "DoorOpening" | "DoorClosing" | "DoorHalt" => "sDoorMsg",
        "Damage" => "sDamageScrMsg",
        "Slain" => "sSlayMsg",
        "Contained" => "sContainedScrMsg",
        "Container" => "sContainerScrMsg",
        "Stimulus" => "sStimMsg",
        "MotionStart" | "MotionEnd" | "MotionFlagReached" => "sBodyMsg",
        "PhysMadePhysical" | "PhysMadeNonPhysical" | "PhysCollision" | "PhysFellAsleep" | "PhysWokeUp" | "PhysContactCreate" | "PhysContactDestroy" => "sPhysMsg",
        "ObjRoomTransit" | "CreatureRoomEnter" | "CreatureRoomExit" => "sRoomMsg",
        "TweqComplete" => "sTweqMsg",
        "CombineAdd" => "sCombineScrMsg",
        "StartAttack" | "StartWindup" | "EndAttack" => "sAttackMsg",
        "QuestChange" => "sQuestMsg",
        "MediumTransition" => "sMediumTransMsg",
        _ => "sScrMsg",
    };
    Ident::new(type_name, Span::call_site())
}

#[proc_macro_attribute]
pub fn dark_script(attr: TokenStream, item: TokenStream) -> TokenStream {
    let messages = parse_macro_input!(attr with Punctuated::<Meta, syn::Token![,]>::parse_terminated);

    let mut match_arms = TokenStream2::default();
    for msg in messages {
        let message = msg.to_token_stream().to_string();
        let message_func = Ident::new(&format!("on_{}", message.to_snake_case()), Span::call_site());
        let msg_type = msg_type_for(&message);

        if msg_type == "sScrMsg" {
            match_arms.extend(quote! {
                #message => self.#message_func(services, msg, reply),
            });
        } else {
            match_arms.extend(quote! {
                #message => {
                    let typed = unsafe { &*(msg as *const sScrMsg as *const #msg_type) };
                    self.#message_func(services, typed, reply)
                }
            });
        }
    }

    let item = parse_macro_input!(item as ItemStruct);

    let name = &item.ident;
    let script_name = name.to_string();
    let script_name_bytes = proc_macro2::Literal::byte_string(format!("{}\0", script_name).as_bytes());
    let script_impl_block = format_ident!("{}_Impl", &name);

    quote! {
        #[implement(IScript)]
        #[derive(Default, Debug)]
        #item

        impl IScript_Impl for #script_impl_block {
            unsafe fn GetClassName(&self) -> *const std::ffi::c_char {
                static NAME: &std::ffi::CStr = unsafe {
                    std::ffi::CStr::from_bytes_with_nul_unchecked(#script_name_bytes)
                };
                NAME.as_ptr()
            }

            unsafe fn ReceiveMessage(&self, msg: &mut sScrMsg, reply: &mut sMultiParm, _: i32) -> HRESULT {
                let services = services();

                let message_name = unsafe {
                    std::ffi::CStr::from_ptr(msg.message).to_str().unwrap()
                };
                match message_name {
                    #match_arms
                    _ => HRESULT(1), // S_FALSE — message not handled
                }
            }
        }

        impl DarkScript for #name {
            fn get_desc(mod_name: &str) -> sScrClassDesc {
                // into_raw() leaks intentionally — these strings must outlive the descriptor
                // because the engine holds pointers to them for the process lifetime.
                let mod_ = std::ffi::CString::from_str(mod_name).unwrap();
                let name = std::ffi::CString::from_str(#script_name).unwrap();
                sScrClassDesc {
                    mod_: mod_.into_raw(),
                    name: name.into_raw(),
                    base: std::ptr::null(),
                    factory: Self::factory,
                }
            }

            extern "C" fn factory(
                _name: *const std::ffi::c_char,
                _id: std::ffi::c_int
            ) -> *mut IScript {
                let mut ret: *mut std::ffi::c_void = std::ptr::null_mut();
                let script: IScript = Self::default().into();
                let guid = IScript::IID;
                let query_result = unsafe { script.query(&raw const guid, &mut ret) };
                if !HRESULT::is_ok(query_result) {
                    return std::ptr::null_mut();
                }
                ret as *mut IScript
            }
        }
    }
    .into()
}
