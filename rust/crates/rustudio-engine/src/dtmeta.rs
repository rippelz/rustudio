//! Datatype, RBXScriptSignal and RBXScriptConnection metatables as in
//! Studio 0.744.
//!
//! mlua builds each userdata metatable itself: `__index` / `__newindex` are
//! generated Lua closures (raising with mlua's own `error` C function, which
//! a script can capture with `debug.info(2, "f")` and call), `__namecall` is
//! mlua's dispatcher, and failures are mlua error userdata. Studio's are
//! unnamed C functions (`debug.info` `[C], -1, "", 0, true`, the same one
//! for every value of the type) raising strings:
//!
//! - unknown members: `%s is not a valid member of T`, or `'%s' …` for the
//!   "quoted" family, location-free;
//! - known read-only members: `%s member of T is read-only and cannot be
//!   assigned to`;
//! - bad direct calls: Luau's argument errors with the caller's position
//!   (`missing argument #2 (string expected)`), the key checked before self;
//! - methods: a fresh C closure named after the method on every read, its
//!   self checked with either Luau's argument errors or Studio's reflection
//!   ones (`Argument 1 missing or nil`, `Unable to cast Array to Vector2`);
//! - `__namecall`: a separate C function for the plain family; the quoted
//!   family has none (namecall goes through `__index`).
//!
//! `install` rebuilds an mlua metatable in place from mlua's own getter,
//! setter and method tables (the generated closures' upvalues), so the
//! types keep their Rust implementations.

use crate::cmeta::LOCATED;
use mlua::{ffi, AnyUserData, Function, Lua, Value};
use std::ffi::{c_char, c_int, c_void, CStr};

/// How a type's methods refuse a bad self.
#[derive(Clone, Copy)]
pub enum SelfErr {
    /// Luau's `missing argument #1 to 'M' (T expected)` with the caller's
    /// position.
    Luau,
    /// Studio's reflection bridge: `Argument 1 missing or nil` /
    /// `Unable to cast Array to <cast>`, location-free.
    Reflect,
}

pub struct TypeMeta {
    /// `__type`.
    pub name: &'static str,
    /// Unknown members quoted (`'%s' is not a valid member of T`); such
    /// types have no `__namecall` (namecall reads `__index`).
    pub quoted: bool,
    /// Index / newindex check self (#1) before the key.
    pub self_first: bool,
    /// Writes to a known member say "not a valid member" (Connection).
    pub write_known_invalid: bool,
    /// Every write fails with `%s cannot be assigned to` (Vector3).
    pub assign_any: bool,
    /// Method self errors: the default, the reflection name in casts, and
    /// methods that use the other style.
    pub methods: SelfErr,
    pub cast: &'static str,
    pub other_style: &'static [&'static str],
}

impl TypeMeta {
    const fn new(name: &'static str, quoted: bool) -> Self {
        TypeMeta {
            name,
            quoted,
            self_first: false,
            write_known_invalid: false,
            assign_any: false,
            methods: SelfErr::Luau,
            cast: name,
            other_style: &[],
        }
    }
    const fn reflect(mut self, cast: &'static str, luau_methods: &'static [&'static str]) -> Self {
        self.methods = SelfErr::Reflect;
        self.cast = cast;
        self.other_style = luau_methods;
        self
    }
    fn style(&self, method: &str) -> SelfErr {
        match (self.methods, self.other_style.contains(&method)) {
            (SelfErr::Luau, false) | (SelfErr::Reflect, true) => SelfErr::Luau,
            _ => SelfErr::Reflect,
        }
    }
}

/// Per type, as measured (Studio probes `read` / `write` / `namecall` /
/// `method.*` rows; `index.namecall.same` true = quoted family).
pub static TYPES: &[TypeMeta] = &[
    TypeMeta { assign_any: true, ..TypeMeta::new("Vector3", false) },
    TypeMeta::new("Vector2", false).reflect("Vector2", &[]),
    TypeMeta::new("Vector3int16", true),
    TypeMeta::new("Vector2int16", true),
    TypeMeta::new("CFrame", false).reflect("CoordinateFrame", &["GetComponents", "components"]),
    TypeMeta::new("Color3", false).reflect("Color3", &["ToHSV", "ToHex"]),
    TypeMeta::new("BrickColor", true),
    TypeMeta::new("UDim", true),
    TypeMeta::new("UDim2", false).reflect("UDim2", &[]),
    TypeMeta::new("Rect", true),
    TypeMeta::new("Ray", false),
    TypeMeta::new("Region3", false),
    TypeMeta::new("Region3int16", true),
    TypeMeta::new("NumberRange", true),
    TypeMeta::new("NumberSequence", true),
    TypeMeta::new("NumberSequenceKeypoint", true),
    TypeMeta::new("ColorSequence", true),
    TypeMeta::new("ColorSequenceKeypoint", true),
    TypeMeta::new("TweenInfo", true),
    TypeMeta::new("PhysicalProperties", true),
    TypeMeta::new("Faces", true),
    TypeMeta::new("Axes", true),
    TypeMeta::new("Font", true),
    TypeMeta::new("DateTime", false).reflect("DateTime", &[]),
    TypeMeta::new("Random", false),
    TypeMeta::new("OverlapParams", false),
    TypeMeta::new("RaycastParams", false),
    TypeMeta::new("RaycastResult", true),
    TypeMeta::new("CatalogSearchParams", true),
    TypeMeta::new("PathWaypoint", true),
    TypeMeta::new("Content", false),
    TypeMeta::new("RBXScriptSignal", true),
    TypeMeta {
        self_first: true,
        write_known_invalid: true,
        ..TypeMeta::new("RBXScriptConnection", false)
    },
];

pub fn meta(name: &str) -> Option<&'static TypeMeta> {
    TYPES.iter().find(|t| t.name == name)
}

// ------------------------------------------------------------ raw helpers

/// Raises `msg` as a string; a leading `LOCATED` takes the caller's
/// position. Everything Rust-owned is dropped before the raise.
unsafe fn raise(l: *mut ffi::lua_State, msg: &str) -> ! {
    unsafe {
        let (located, body) = match msg.strip_prefix(LOCATED) {
            Some(b) => (true, b),
            None => (false, msg),
        };
        if located {
            ffi::luaL_where(l, 1);
        }
        ffi::lua_pushlstring(l, body.as_ptr() as *const c_char, body.len());
        if located {
            ffi::lua_concat(l, 2);
        }
    }
    unsafe { ffi::lua_error(l) }
}

/// Raises the failure on top of the stack (from a protected call) as a
/// string, converting mlua errors with `errtext` (an upvalue index).
unsafe fn reraise(l: *mut ffi::lua_State, errtext: c_int) -> ! {
    unsafe { reraise_named(l, errtext, "") }
}

/// `reraise` for a method `name`: an argument error that doesn't name its
/// function gets `to 'name'` (`name_arg_error`), as mlua's own methods
/// worded it (natively stored types' methods are plain functions).
unsafe fn reraise_named(l: *mut ffi::lua_State, errtext: c_int, name: &str) -> ! {
    unsafe {
        if ffi::lua_type(l, -1) != ffi::LUA_TSTRING {
            ffi::lua_pushvalue(l, errtext);
            ffi::lua_insert(l, -2);
            if ffi::lua_pcall(l, 1, 1, 0) != 0 || ffi::lua_type(l, -1) != ffi::LUA_TSTRING {
                ffi::lua_settop(l, 0);
                ffi::lua_pushstring(l, c"invalid argument".as_ptr());
            }
        }
        if !name.is_empty() {
            let msg = name_arg_error(str_at(l, -1), name);
            ffi::lua_pop(l, 1);
            ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
            drop(msg);
        }
        let mut len = 0usize;
        let s = ffi::lua_tolstring(l, -1, &mut len);
        if len > 0 && *s == LOCATED as u8 as c_char {
            ffi::lua_pushlstring(l, s.add(1), len - 1);
            ffi::luaL_where(l, 1);
            ffi::lua_insert(l, -2);
            ffi::lua_concat(l, 2);
        }
        ffi::lua_error(l)
    }
}

unsafe fn str_at<'a>(l: *mut ffi::lua_State, idx: c_int) -> &'a str {
    unsafe {
        let mut len = 0usize;
        let p = ffi::lua_tolstring(l, idx, &mut len);
        if p.is_null() {
            return "";
        }
        std::str::from_utf8_unchecked(std::slice::from_raw_parts(p as *const u8, len))
    }
}

unsafe fn meta_at<'a>(l: *mut ffi::lua_State, upv: c_int) -> &'a TypeMeta {
    unsafe { &*(ffi::lua_touserdata(l, ffi::lua_upvalueindex(upv)) as *const TypeMeta) }
}

/// Whether the value at `idx` is of the type whose metatable is upvalue
/// `mt` (native vectors: the vector type).
unsafe fn is_self(l: *mut ffi::lua_State, idx: c_int, mt: c_int) -> bool {
    unsafe {
        match ffi::lua_type(l, idx) {
            ffi::LUA_TUSERDATA => {
                if ffi::lua_getmetatable(l, idx) == 0 {
                    return false;
                }
                let same = ffi::lua_rawequal(l, -1, ffi::lua_upvalueindex(mt)) != 0;
                ffi::lua_pop(l, 1);
                same
            }
            ffi::LUA_TVECTOR => ffi::lua_type(l, ffi::lua_upvalueindex(mt)) == ffi::LUA_TBOOLEAN,
            _ => false,
        }
    }
}

/// Luau's argument error for self (#1) naming `T`.
unsafe fn check_self(l: *mut ffi::lua_State, meta: &TypeMeta, mt: c_int) {
    unsafe {
        if !is_self(l, 1, mt) {
            typeerror(l, 1, meta.name)
        }
    }
}

/// A value's `__type` (userdata) or basic type name.
unsafe fn type_name_at(l: *mut ffi::lua_State, idx: c_int) -> String {
    unsafe {
        let basic = CStr::from_ptr(ffi::luaL_typename(l, idx)).to_string_lossy().into_owned();
        if ffi::lua_type(l, idx) != ffi::LUA_TUSERDATA || ffi::lua_getmetatable(l, idx) == 0 {
            return basic;
        }
        ffi::lua_pushstring(l, c"__type".as_ptr());
        ffi::lua_rawget(l, -2);
        let out = if ffi::lua_type(l, -1) == ffi::LUA_TSTRING { str_at(l, -1).to_string() } else { basic };
        ffi::lua_pop(l, 2);
        out
    }
}

/// `luaL_typeerror` with a Rust type name (copied into Lua first).
unsafe fn typeerror(l: *mut ffi::lua_State, narg: c_int, tname: &str) -> ! {
    unsafe {
        let mut buf = [0u8; 64];
        let n = tname.len().min(63);
        buf[..n].copy_from_slice(&tname.as_bytes()[..n]);
        ffi::luaL_typeerror(l, narg, buf.as_ptr() as *const c_char)
    }
}

/// The key (#2): a string (numbers become their text), else Luau's
/// argument error.
unsafe fn check_key(l: *mut ffi::lua_State) {
    unsafe {
        match ffi::lua_type(l, 2) {
            ffi::LUA_TSTRING => {}
            ffi::LUA_TNUMBER => {
                ffi::lua_tolstring(l, 2, std::ptr::null_mut());
            }
            _ => ffi::luaL_typeerror(l, 2, c"string".as_ptr()),
        }
    }
}

/// Pushes `t[key]` for the table at upvalue `upv` (nil if it is no table).
unsafe fn upv_get(l: *mut ffi::lua_State, upv: c_int, key: c_int) -> c_int {
    unsafe {
        if ffi::lua_type(l, ffi::lua_upvalueindex(upv)) != ffi::LUA_TTABLE {
            ffi::lua_pushnil(l);
            return ffi::LUA_TNIL;
        }
        ffi::lua_pushvalue(l, key);
        ffi::lua_rawget(l, ffi::lua_upvalueindex(upv))
    }
}

fn unknown(meta: &TypeMeta, key: &str) -> String {
    if meta.quoted {
        format!("'{key}' is not a valid member of {}", meta.name)
    } else {
        format!("{key} is not a valid member of {}", meta.name)
    }
}

// -------------------------------------------------------------- __index

/// Upvalues: 1 getters, 2 methods, 3 fallback `__index` (table, function
/// or nil), 4 `TypeMeta`, 5 errtext, 6 the type's metatable (`true` for
/// native vectors).
unsafe extern "C-unwind" fn index_fn(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let meta = meta_at(l, 4);
        if meta.self_first {
            check_self(l, meta, 6);
        }
        check_key(l);
        ffi::lua_settop(l, 2);
        // As Studio's CFrame / Vector3 getters (CFrame __index, Position:
        // the value read and pushed, no call).
        if let Some(n) = native_get(l, meta) {
            return n;
        }
        if upv_get(l, 1, 2) != ffi::LUA_TNIL {
            if !meta.self_first {
                check_self(l, meta, 6);
            }
            ffi::lua_pushvalue(l, 1);
            if ffi::lua_pcall(l, 1, 1, 0) != 0 {
                reraise(l, ffi::lua_upvalueindex(5));
            }
            return 1;
        }
        ffi::lua_pop(l, 1);
        if upv_get(l, 2, 2) != ffi::LUA_TNIL {
            if !meta.self_first {
                check_self(l, meta, 6);
            }
            push_method(l, 3, 2, 4, 5, 6);
            return 1;
        }
        ffi::lua_pop(l, 1);
        match ffi::lua_type(l, ffi::lua_upvalueindex(3)) {
            ffi::LUA_TFUNCTION => {
                ffi::lua_pushvalue(l, ffi::lua_upvalueindex(3));
                ffi::lua_pushvalue(l, 1);
                ffi::lua_pushvalue(l, 2);
                if ffi::lua_pcall(l, 2, 1, 0) != 0 {
                    reraise(l, ffi::lua_upvalueindex(5));
                }
                return 1;
            }
            ffi::LUA_TTABLE => {
                ffi::lua_pushvalue(l, 2);
                if ffi::lua_rawget(l, ffi::lua_upvalueindex(3)) != ffi::LUA_TNIL {
                    return 1;
                }
                ffi::lua_pop(l, 1);
            }
            _ => {}
        }
        let msg = unknown(meta, str_at(l, 2));
        ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
        drop(msg);
        ffi::lua_error(l)
    }
}

/// The CFrame at 1 (self already known to be one).
unsafe fn cframe_at(l: *mut ffi::lua_State) -> Option<rustudio_core::math::Xform> {
    unsafe { crate::ltypes::cframe_at(l, 1) }
}

/// The CFrame at `i`, if the value there is one.
unsafe fn cframe_idx(l: *mut ffi::lua_State, i: c_int) -> Option<rustudio_core::math::Xform> {
    unsafe { crate::ltypes::cframe_at(l, i) }
}

/// A CFrame / Vector3 member answered natively (as the type's mlua getter
/// would: same values), after the self check; None: not one of these.
unsafe fn native_get(l: *mut ffi::lua_State, meta: &TypeMeta) -> Option<c_int> {
    unsafe {
        let key = str_at(l, 2);
        match meta.name {
            "Vector3" => {
                let f: fn(glam::Vec3) -> Pushed = match key {
                    "Magnitude" | "magnitude" => |v| Pushed::Num(v.length() as f64),
                    "Unit" | "unit" => |v| Pushed::Vec(v.normalize_or_zero()),
                    _ => return None,
                };
                if ffi::lua_type(l, 1) != ffi::LUA_TVECTOR {
                    return None;
                }
                let p = ffi::lua_tovector(l, 1);
                Some(f(glam::Vec3::new(*p, *p.add(1), *p.add(2))).push(l))
            }
            "Color3" | "Vector2" if dt_fast() => {
                let i = match (meta.name, key) {
                    ("Color3", "R" | "r") | ("Vector2", "X" | "x") => 0,
                    ("Color3", "G" | "g") | ("Vector2", "Y" | "y") => 1,
                    ("Color3", "B" | "b") => 2,
                    _ => return None,
                };
                let lua = Lua::get_or_init_from_ptr(l);
                let v = if meta.name == "Color3" {
                    lua.peek_userdata_copy::<crate::ltypes::LColor3>(l, 1)?.0[i]
                } else {
                    lua.peek_userdata_copy::<crate::ltypes::LVector2>(l, 1)?.0[i]
                };
                Some(Pushed::Num(v as f64).push(l))
            }
            "CFrame" => {
                let f: fn(&rustudio_core::math::Xform) -> Pushed = match key {
                    "Position" | "p" => |x| Pushed::Vec(x.pos),
                    "X" | "x" => |x| Pushed::Num(x.pos.x as f64),
                    "Y" | "y" => |x| Pushed::Num(x.pos.y as f64),
                    "Z" | "z" => |x| Pushed::Num(x.pos.z as f64),
                    "LookVector" | "lookVector" => |x| Pushed::Vec(-x.rot.z_axis),
                    "RightVector" | "rightVector" | "XVector" => |x| Pushed::Vec(x.rot.x_axis),
                    "UpVector" | "upVector" | "YVector" => |x| Pushed::Vec(x.rot.y_axis),
                    "ZVector" => |x| Pushed::Vec(x.rot.z_axis),
                    "Rotation" => |x| Pushed::Cf(rustudio_core::math::Xform { pos: glam::Vec3::ZERO, rot: x.rot }),
                    _ => return None,
                };
                // (A CFrame's tag is its type: no metatable compare.)
                if let Some(x) = cframe_at(l) {
                    return Some(f(&x).push(l));
                }
                if !meta.self_first {
                    check_self(l, meta, 6);
                }
                let x = cframe_at(l)?;
                Some(f(&x).push(l))
            }
            _ => None,
        }
    }
}

enum Pushed {
    Num(f64),
    Vec(glam::Vec3),
    Cf(rustudio_core::math::Xform),
}

impl Pushed {
    unsafe fn push(self, l: *mut ffi::lua_State) -> c_int {
        unsafe {
            match self {
                Pushed::Num(n) => ffi::lua_pushnumber(l, n),
                Pushed::Vec(v) => ffi::lua_pushvector(l, v.x, v.y, v.z),
                Pushed::Cf(x) => {
                    ffi::luaL_checkstack(l, 1, std::ptr::null());
                    crate::ltypes::cframe_push(l, x)
                }
            }
        }
        1
    }
}

/// Pushes a fresh method closure for the method on top of the stack
/// (popped), named by the key at `key`. `meta`, `errtext`, `mt`: upvalue
/// numbers of the caller.
unsafe fn push_method(l: *mut ffi::lua_State, _unused: c_int, key: c_int, meta: c_int, errtext: c_int, mt: c_int) {
    unsafe {
        // The name lives as long as the methods table's key does (forever).
        let name = ffi::lua_tolstring(l, key, std::ptr::null_mut());
        let m = meta_at(l, meta);
        let style = m.style(CStr::from_ptr(name).to_str().unwrap_or(""));
        ffi::luaL_checkstack(l, 5, std::ptr::null());
        // the method stays on top: upvalue 1
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(meta));
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(errtext));
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(mt));
        ffi::lua_pushboolean(l, matches!(style, SelfErr::Reflect) as c_int);
        ffi::lua_pushvalue(l, key);
        ffi::lua_pushcclosurek(l, method_fn, name, 6, None);
    }
}

/// Studio's reflection name of a Lua value in `Unable to cast X to T`.
fn cast_name(t: c_int, typename: &str) -> &str {
    match t {
        ffi::LUA_TTABLE => "Array",
        ffi::LUA_TNUMBER => "double",
        ffi::LUA_TBOOLEAN => "bool",
        ffi::LUA_TSTRING => "string",
        ffi::LUA_TFUNCTION => "function",
        _ => typename,
    }
}

/// A method. Upvalues: 1 the implementation (mlua callback), 2 `TypeMeta`,
/// 3 errtext, 4 the type's metatable, 5 reflection-style self errors,
/// 6 its name.
unsafe extern "C-unwind" fn method_fn(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        if !is_self(l, 1, 4) {
            let meta = meta_at(l, 2);
            if ffi::lua_toboolean(l, ffi::lua_upvalueindex(5)) == 0 {
                typeerror(l, 1, meta.name);
            }
            let t = ffi::lua_type(l, 1);
            let msg = if t == ffi::LUA_TNIL || t == ffi::LUA_TNONE {
                "Argument 1 missing or nil".to_string()
            } else {
                let tn = type_name_at(l, 1);
                format!("Unable to cast {} to {}", cast_name(t, &tn), meta.cast)
            };
            ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
            drop(msg);
            ffi::lua_error(l);
        }
        let n = ffi::lua_gettop(l);
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(1));
        ffi::lua_insert(l, 1);
        if ffi::lua_pcall(l, n, ffi::LUA_MULTRET, 0) != 0 {
            let name = str_at(l, ffi::lua_upvalueindex(6));
            reraise_named(l, ffi::lua_upvalueindex(3), name);
        }
        ffi::lua_gettop(l)
    }
}

// ----------------------------------------------------------- __newindex

/// Upvalues: 1 setters, 2 getters, 3 methods, 4 fallback `__newindex`
/// function (or nil), 5 `TypeMeta`, 6 errtext, 7 the type's metatable.
unsafe extern "C-unwind" fn newindex_fn(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let meta = meta_at(l, 5);
        if meta.self_first {
            check_self(l, meta, 7);
        }
        check_key(l);
        ffi::lua_settop(l, 3);
        if meta.assign_any {
            let msg = format!("{} cannot be assigned to", str_at(l, 2));
            ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
            drop(msg);
            ffi::lua_error(l);
        }
        if upv_get(l, 1, 2) != ffi::LUA_TNIL {
            if !meta.self_first {
                check_self(l, meta, 7);
            }
            ffi::lua_pushvalue(l, 1);
            ffi::lua_pushvalue(l, 3);
            if ffi::lua_pcall(l, 2, 0, 0) != 0 {
                reraise(l, ffi::lua_upvalueindex(6));
            }
            return 0;
        }
        ffi::lua_pop(l, 1);
        // A custom `__newindex` decides for types without mlua fields
        // (CatalogSearchParams); with fields, unknown keys are ours.
        if ffi::lua_type(l, ffi::lua_upvalueindex(4)) == ffi::LUA_TFUNCTION
            && ffi::lua_type(l, ffi::lua_upvalueindex(2)) != ffi::LUA_TTABLE
        {
            ffi::lua_pushvalue(l, ffi::lua_upvalueindex(4));
            ffi::lua_pushvalue(l, 1);
            ffi::lua_pushvalue(l, 2);
            ffi::lua_pushvalue(l, 3);
            if ffi::lua_pcall(l, 3, 0, 0) != 0 {
                reraise(l, ffi::lua_upvalueindex(6));
            }
            return 0;
        }
        let known = upv_get(l, 2, 2) != ffi::LUA_TNIL || upv_get(l, 3, 2) != ffi::LUA_TNIL;
        let key = str_at(l, 2);
        let msg = if known && !meta.write_known_invalid {
            format!("{key} member of {} is read-only and cannot be assigned to", meta.name)
        } else {
            format!("{key} is not a valid member of {}", meta.name)
        };
        ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
        drop(msg);
        ffi::lua_error(l)
    }
}

// ----------------------------------------------------------- __namecall

/// Upvalues: 1 methods, 2 `TypeMeta`, 3 errtext.
unsafe extern "C-unwind" fn namecall_fn(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let name = ffi::lua_namecallatom(l, std::ptr::null_mut());
        let meta = meta_at(l, 2);
        if name.is_null() {
            raise(l, "attempt to call a nil value");
        }
        let n = ffi::lua_gettop(l);
        // (The fast path first: it answers only methods the type has, on a
        // CFrame self, without making the name a Lua string to look up.)
        if meta.name == "CFrame" && DT_FAST.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(r) = cf_method_fast(l, CStr::from_ptr(name).to_bytes(), n) {
                return r;
            }
        }
        ffi::luaL_checkstack(l, 2, std::ptr::null());
        ffi::lua_pushstring(l, name);
        let found = upv_get(l, 1, n + 1) != ffi::LUA_TNIL;
        if !found {
            let msg = unknown(&TypeMeta { quoted: false, ..TypeMeta::new(meta.name, false) }, str_at(l, n + 1));
            ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
            drop(msg);
            ffi::lua_error(l);
        }
        ffi::lua_remove(l, n + 1);
        ffi::lua_insert(l, 1);
        if ffi::lua_pcall(l, n, ffi::LUA_MULTRET, 0) != 0 {
            // (The atom's text lives as long as the interned string.)
            let name = CStr::from_ptr(name).to_str().unwrap_or("");
            reraise_named(l, ffi::lua_upvalueindex(3), name);
        }
        ffi::lua_gettop(l)
    }
}

// ------------------------------------------------------- CFrame fast paths
//
// The commonest CFrame constructors, operators and methods answered in the
// C function itself: the arguments read off the stack, the result pushed as
// the same mlua userdata `create_userdata` makes (`push_userdata_unref`).
// Only exact cases (the argument types and counts the Rust implementation
// accepts, finite numbers): anything else, errors included, goes through the
// implementation as before. The arithmetic is the implementation's own
// (ltypes cf_mul / cf_inv / look_at_legacy, glam's euler / slerp).

/// Whether the fast paths are on (RUSTUDIO_NO_DT_FAST=1 turns them off: the
/// implementation answers everything, for comparisons).
static DT_FAST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

fn dt_fast() -> bool {
    static ENV: std::sync::Once = std::sync::Once::new();
    ENV.call_once(|| {
        if std::env::var_os("RUSTUDIO_NO_DT_FAST").is_some() {
            DT_FAST.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    });
    DT_FAST.load(std::sync::atomic::Ordering::Relaxed)
}

/// The fast constructor for `CFrame.<f>` (0 none).
fn ctor_fast_id(ty: &str, f: &str) -> i32 {
    if !dt_fast() {
        return 0;
    }
    match (ty, f) {
        ("CFrame", "new") => 1,
        ("CFrame", "Angles" | "fromEulerAnglesXYZ") => 2,
        ("CFrame", "fromOrientation" | "fromEulerAnglesYXZ") => 3,
        ("Color3", "new") => 4,
        ("Color3", "fromRGB") => 5,
        ("UDim2", "new") => 6,
        ("UDim2", "fromScale") => 7,
        ("UDim2", "fromOffset") => 8,
        ("Vector2", "new") => 9,
        _ => 0,
    }
}

/// A finite number at `i` (that converts to f32 as mlua converts it).
unsafe fn num_at(l: *mut ffi::lua_State, i: c_int) -> Option<f32> {
    unsafe {
        if ffi::lua_type(l, i) != ffi::LUA_TNUMBER {
            return None;
        }
        let x = ffi::lua_tonumber(l, i);
        (x.is_finite() && x.abs() <= f32::MAX as f64).then_some(x as f32)
    }
}

unsafe fn vec_at(l: *mut ffi::lua_State, i: c_int) -> Option<glam::Vec3> {
    unsafe {
        if ffi::lua_type(l, i) != ffi::LUA_TVECTOR {
            return None;
        }
        let p = ffi::lua_tovector(l, i);
        Some(glam::Vec3::new(*p, *p.add(1), *p.add(2)))
    }
}

/// Pushes a new CFrame; false (nothing pushed) if it can't be made here.
/// (Reached only from the CFrame type's own functions: the metatable is
/// installed.)
unsafe fn push_cframe(l: *mut ffi::lua_State, x: rustudio_core::math::Xform) -> bool {
    unsafe {
        if ffi::lua_checkstack(l, 2) == 0 {
            return false;
        }
        crate::ltypes::cframe_push(l, x);
        true
    }
}

unsafe fn cf_ctor_fast(l: *mut ffi::lua_State, id: ffi::lua_Integer, n: c_int) -> Option<rustudio_core::math::Xform> {
    use rustudio_core::math::Xform;
    use glam::{EulerRot, Mat3, Vec3};
    unsafe {
        match (id, n) {
            (1, 0) => Some(Xform { pos: Vec3::ZERO, rot: Mat3::IDENTITY }),
            (1, 1) => Some(Xform { pos: vec_at(l, 1)?, rot: Mat3::IDENTITY }),
            (1, 2) => Some(crate::ltypes::look_at_legacy(vec_at(l, 1)?, vec_at(l, 2)?)),
            (1, 3) => Some(Xform { pos: Vec3::new(num_at(l, 1)?, num_at(l, 2)?, num_at(l, 3)?), rot: Mat3::IDENTITY }),
            (1, 12) => {
                let mut c = [0f32; 12];
                for (k, v) in c.iter_mut().enumerate() {
                    *v = num_at(l, k as c_int + 1)?;
                }
                Some(Xform {
                    pos: Vec3::new(c[0], c[1], c[2]),
                    rot: Mat3::from_cols(
                        Vec3::new(c[3], c[6], c[9]),
                        Vec3::new(c[4], c[7], c[10]),
                        Vec3::new(c[5], c[8], c[11]),
                    ),
                })
            }
            (2, 3) => {
                let (x, y, z) = (num_at(l, 1)?, num_at(l, 2)?, num_at(l, 3)?);
                Some(Xform { pos: Vec3::ZERO, rot: Mat3::from_euler(EulerRot::XYZ, x, y, z) })
            }
            (3, 3) => {
                let (x, y, z) = (num_at(l, 1)?, num_at(l, 2)?, num_at(l, 3)?);
                Some(Xform { pos: Vec3::ZERO, rot: Mat3::from_euler(EulerRot::YXZ, y, x, z) })
            }
            _ => None,
        }
    }
}

/// CFrame `*` / `+` / `-` (op 1 / 2 / 3) on the two operands; the number
/// of results pushed, None to take the implementation.
unsafe fn cf_op_fast(l: *mut ffi::lua_State, op: ffi::lua_Integer) -> Option<c_int> {
    use rustudio_core::math::Xform;
    unsafe {
        let a = cframe_idx(l, 1)?;
        let out = match op {
            1 => {
                if let Some(v) = vec_at(l, 2) {
                    let p = a.pos + a.rot * v;
                    ffi::lua_pushvector(l, p.x, p.y, p.z);
                    return Some(1);
                }
                crate::ltypes::cf_mul(&a, &cframe_idx(l, 2)?)
            }
            2 => Xform { pos: a.pos + vec_at(l, 2)?, rot: a.rot },
            3 => Xform { pos: a.pos - vec_at(l, 2)?, rot: a.rot },
            _ => return None,
        };
        push_cframe(l, out).then_some(1)
    }
}

/// Vector2 `+` / `-` / `*` / `/` (op 4..7) with the implementation's
/// arithmetic (ltypes LVector2): Vector2 with Vector2, and a number on
/// either side of `*` or after `/`; None for anything else.
unsafe fn v2_op_fast(l: *mut ffi::lua_State, op: ffi::lua_Integer) -> Option<c_int> {
    use crate::ltypes::LVector2;
    unsafe {
        let lua = Lua::get_or_init_from_ptr(l);
        let v = |i: c_int| lua.peek_userdata_copy::<LVector2>(l, i).map(|v| v.0);
        let k = |i: c_int| (ffi::lua_type(l, i) == ffi::LUA_TNUMBER).then(|| ffi::lua_tonumber(l, i));
        let out = match op {
            4 => v(1)? + v(2)?,
            5 => v(1)? - v(2)?,
            6 => match (v(1), v(2)) {
                (Some(a), Some(b)) => a * b,
                (Some(a), None) => a * k(2)? as f32,
                (None, Some(b)) => b * k(1)? as f32,
                _ => return None,
            },
            _ => {
                let a = v(1)?;
                match v(2) {
                    Some(b) => a / b,
                    None => a * (1.0 / k(2)?) as f32,
                }
            }
        };
        if ffi::lua_checkstack(l, 3) == 0 {
            return None;
        }
        matches!(lua.push_userdata_unref(l, LVector2(out)), Ok(true)).then_some(1)
    }
}

/// `cf:Method(...)` for the commonest methods (self at 1, `n` values on the
/// stack before the method name pushed at n + 1).
unsafe fn cf_method_fast(l: *mut ffi::lua_State, name: &[u8], n: c_int) -> Option<c_int> {
    use crate::ltypes::{cf_inv, cf_mul};
    use rustudio_core::math::Xform;
    use glam::Quat;
    unsafe {
        let a = cframe_idx(l, 1)?;
        // A CFrame argument (its tag says so).
        let other = |i: c_int| -> Option<Xform> { cframe_idx(l, i) };
        let vec_out = |v: glam::Vec3| {
            ffi::lua_pushvector(l, v.x, v.y, v.z);
            Some(1)
        };
        let nums = |v: &[f32]| {
            ffi::luaL_checkstack(l, v.len() as c_int, std::ptr::null());
            for x in v {
                ffi::lua_pushnumber(l, *x as f64);
            }
            Some(v.len() as c_int)
        };
        let cf_out = match (name, n) {
            (b"GetComponents" | b"components", 1) => {
                let r = &a.rot;
                return nums(&[
                    a.pos.x, a.pos.y, a.pos.z, r.x_axis.x, r.y_axis.x, r.z_axis.x, r.x_axis.y, r.y_axis.y,
                    r.z_axis.y, r.x_axis.z, r.y_axis.z, r.z_axis.z,
                ]);
            }
            (b"ToEulerAnglesXYZ" | b"toEulerAnglesXYZ", 1) => {
                let (x, y, z) = a.rot.to_euler(glam::EulerRot::XYZ);
                return nums(&[x, y, z]);
            }
            (b"ToEulerAnglesYXZ" | b"toEulerAnglesYXZ" | b"ToOrientation" | b"toOrientation", 1) => {
                let (y, x, z) = a.rot.to_euler(glam::EulerRot::YXZ);
                return nums(&[x, y, z]);
            }
            (b"ToAxisAngle" | b"toAxisAngle", 1) => {
                let (axis, angle) = Quat::from_mat3(&a.rot).to_axis_angle();
                ffi::luaL_checkstack(l, 2, std::ptr::null());
                ffi::lua_pushvector(l, axis.x, axis.y, axis.z);
                ffi::lua_pushnumber(l, angle as f64);
                return Some(2);
            }
            (b"Inverse" | b"inverse", 1) => cf_inv(&a),
            (b"ToWorldSpace" | b"toWorldSpace", 2) => cf_mul(&a, &other(2)?),
            (b"ToObjectSpace" | b"toObjectSpace", 2) => cf_mul(&cf_inv(&a), &other(2)?),
            (b"PointToWorldSpace" | b"pointToWorldSpace", 2) => return vec_out(a.pos + a.rot * vec_at(l, 2)?),
            (b"PointToObjectSpace" | b"pointToObjectSpace", 2) => {
                return vec_out(a.rot.transpose() * (vec_at(l, 2)? - a.pos))
            }
            (b"VectorToWorldSpace" | b"vectorToWorldSpace", 2) => return vec_out(a.rot * vec_at(l, 2)?),
            (b"VectorToObjectSpace" | b"vectorToObjectSpace", 2) => return vec_out(a.rot.transpose() * vec_at(l, 2)?),
            (b"Lerp" | b"lerp", 3) => {
                let o = other(2)?;
                let t = num_at(l, 3)?;
                let q = Quat::from_mat3(&a.rot).slerp(Quat::from_mat3(&o.rot), t);
                Xform { pos: a.pos.lerp(o.pos, t), rot: glam::Mat3::from_quat(q) }
            }
            _ => return None,
        };
        push_cframe(l, cf_out).then_some(1)
    }
}

// ------------------------------------------------------------ operators

/// An operator. Upvalues: 1 the implementation, 2 errtext, 3 its fast path
/// (0 none, CFrame 1 `*`, 2 `+`, 3 `-`), 4 the type's metatable.
unsafe extern "C-unwind" fn op_fn(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let n = ffi::lua_gettop(l);
        let fast = ffi::lua_tointegerx(l, ffi::lua_upvalueindex(3), std::ptr::null_mut());
        if fast != 0 && n == 2 {
            let r = if fast >= 4 { v2_op_fast(l, fast) } else { cf_op_fast(l, fast) };
            if let Some(r) = r {
                return r;
            }
        }
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(1));
        ffi::lua_insert(l, 1);
        if ffi::lua_pcall(l, n, ffi::LUA_MULTRET, 0) != 0 {
            reraise(l, ffi::lua_upvalueindex(2));
        }
        ffi::lua_gettop(l)
    }
}

// --------------------------------------------------------------- errors

/// Studio's text for an mlua failure inside a datatype member: argument
/// conversion failures become Luau's argument errors (with the caller's
/// position), everything else its message.
pub fn studio_text(e: &mlua::Error) -> String {
    use mlua::Error as E;
    match e {
        E::RuntimeError(m) => m.clone(),
        E::CallbackError { cause, .. } => studio_text(cause),
        E::WithContext { cause, .. } => studio_text(cause),
        E::BadArgument { to, pos, cause, .. } => {
            let fname = to
                .as_deref()
                .map(|t| t.rsplit(['.', ':']).next().unwrap_or(t).to_string())
                .filter(|f| !f.starts_with("__"));
            let (from, want) = match cause.as_ref() {
                E::FromLuaConversionError { from, to, .. } => (from.to_string(), lua_type_of(to)),
                other => match expected_got(&studio_text(other)) {
                    Some(p) => p,
                    None => return studio_text(other),
                },
            };
            let named = fname.map(|f| format!(" to '{f}'")).unwrap_or_default();
            if from == "nil" || from == "missing argument" {
                format!("{LOCATED}missing argument #{pos}{named} ({want} expected)")
            } else {
                let from = if from == "integer" { "number".to_string() } else { from };
                format!("{LOCATED}invalid argument #{pos}{named} ({want} expected, got {from})")
            }
        }
        E::FromLuaConversionError { from, to, .. } => {
            format!("{LOCATED}invalid argument ({} expected, got {from})", lua_type_of(to))
        }
        other => crate::runtime::describe_error(other),
    }
}

/// `"Vector3 expected, got integer"` (ltypes' own conversion failures).
fn expected_got(msg: &str) -> Option<(String, String)> {
    let (want, got) = msg.split_once(" expected, got ")?;
    let want = want.rsplit(' ').next()?;
    let got = got.split_whitespace().next()?;
    Some((got.to_string(), lua_type_of(want)))
}

/// Luau's name for an mlua conversion target (`LVector3` → `Vector3`).
fn lua_type_of(rust: &str) -> String {
    let t = rust.rsplit("::").next().unwrap_or(rust);
    match t {
        "f32" | "f64" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize"
        | "Number" | "Integer" => "number".into(),
        "String" | "BorrowedStr" | "BorrowedBytes" | "str" => "string".into(),
        "bool" => "boolean".into(),
        "Function" => "function".into(),
        "Table" => "table".into(),
        "userdata" => "userdata".into(),
        _ => match t.strip_prefix('L') {
            Some(rest) if rest.starts_with(|c: char| c.is_ascii_uppercase()) => rest.into(),
            _ => t.into(),
        },
    }
}

fn errtext(lua: &Lua) -> mlua::Result<Function> {
    crate::cmeta::cached(lua, "rs_dtmeta_errtext", |lua| {
        lua.create_function(|_, v: Value| {
            Ok(match v {
                Value::Error(e) => studio_text(&e),
                Value::String(s) => s.to_string_lossy(),
                other => format!("invalid argument ({})", other.type_name()),
            })
        })
    })
}

// --------------------------------------------------------------- install

/// mlua's tables behind a generated `__index` / `__newindex` closure.
struct Parts {
    getters: Value,
    methods: Value,
    fallback: Value,
}

/// Reads the upvalues of mlua's generated `__index`:
/// `(field_getters, __index)` or `(field_getters, methods, isfunction,
/// __index, error)` (mlua 0.12 userdata/util.rs). A table `__index` is the
/// methods (plus constant fields); a C function is a custom `__index`.
fn index_parts(lua: &Lua, index: &Value) -> mlua::Result<Parts> {
    let none = || Parts { getters: Value::Nil, methods: Value::Nil, fallback: Value::Nil };
    match index {
        Value::Table(t) => {
            let methods = lua.create_table()?;
            let consts = lua.create_table()?;
            for pair in t.pairs::<Value, Value>() {
                let (k, v) = pair?;
                if matches!(v, Value::Function(_)) {
                    methods.raw_set(k, v)?;
                } else {
                    consts.raw_set(k, v)?;
                }
            }
            Ok(Parts { getters: Value::Nil, methods: Value::Table(methods), fallback: Value::Table(consts) })
        }
        Value::Function(f) if f.info().what == "Lua" => {
            let ups = upvalues(lua, f)?;
            match ups.len() {
                // `__index` here is mlua's methods table when there was none.
                2 => match &ups[1] {
                    t @ Value::Table(_) => Ok(Parts { getters: ups[0].clone(), ..index_parts(lua, t)? }),
                    other => Ok(Parts { getters: ups[0].clone(), methods: Value::Nil, fallback: other.clone() }),
                },
                5 => Ok(Parts { getters: ups[0].clone(), methods: ups[1].clone(), fallback: ups[3].clone() }),
                n => Err(mlua::Error::runtime(format!("dtmeta: unexpected __index shape ({n} upvalues)"))),
            }
        }
        Value::Function(_) => Ok(Parts { fallback: index.clone(), ..none() }),
        _ => Ok(none()),
    }
}

/// `(setters, fallback)` behind mlua's generated `__newindex`:
/// `(field_setters, isfunction, __newindex, error)`.
fn newindex_parts(lua: &Lua, newindex: &Value) -> mlua::Result<(Value, Value)> {
    match newindex {
        Value::Function(f) if f.info().what == "Lua" => {
            let ups = upvalues(lua, f)?;
            match ups.len() {
                4 => Ok((ups[0].clone(), ups[2].clone())),
                n => Err(mlua::Error::runtime(format!("dtmeta: unexpected __newindex shape ({n} upvalues)"))),
            }
        }
        Value::Function(_) => Ok((Value::Nil, newindex.clone())),
        _ => Ok((Value::Nil, Value::Nil)),
    }
}

fn upvalues(lua: &Lua, f: &Function) -> mlua::Result<Vec<Value>> {
    unsafe {
        lua.exec_raw::<mlua::MultiValue>(f, |l| {
            let mut n = 1;
            while !ffi::lua_getupvalue(l, 1, n).is_null() {
                n += 1;
            }
            ffi::lua_remove(l, 1);
        })
        .map(|m| m.into_iter().collect())
    }
}

/// Arithmetic / comparison metamethods Studio's type has (the rest are
/// removed, so the VM gives its own `attempt to perform arithmetic` /
/// `attempt to compare`); kept ones become unnamed C functions.
fn studio_ops(name: &str) -> &'static [&'static str] {
    match name {
        "Vector2" => &["__add", "__sub", "__mul", "__div", "__unm", "__idiv"],
        "Vector3int16" | "Vector2int16" => &["__add", "__sub", "__mul", "__div", "__unm"],
        "UDim" | "UDim2" => &["__add", "__sub", "__unm"],
        "CFrame" => &["__add", "__sub", "__mul"],
        _ => &[],
    }
}

const ARITH: &[&str] = &["__add", "__sub", "__mul", "__div", "__idiv", "__mod", "__pow", "__unm", "__lt", "__le", "__len", "__concat", "__call"];

/// Rebuilds the metatable of `sample`'s type (see the module docs).
pub fn install(lua: &Lua, sample: &AnyUserData) -> mlua::Result<()> {
    let mt: mlua::Table = unsafe {
        lua.exec_raw::<mlua::Table>(sample, |l| {
            if ffi::lua_getmetatable(l, -1) == 0 {
                ffi::lua_pushnil(l);
            }
            ffi::lua_remove(l, -2);
        })?
    };
    let name: String = mt.raw_get("__type")?;
    let Some(meta) = meta(&name) else {
        return Err(mlua::Error::runtime(format!("dtmeta: no spec for {name}")));
    };
    if mt.raw_get::<Option<bool>>("__rs_dtmeta")?.is_some() {
        return Ok(());
    }
    install_on(lua, &mt, meta, Value::Table(mt.clone()))?;
    mt.raw_set("__rs_dtmeta", true)?;
    Ok(())
}

// ------------------------------------------------------ native CFrame

/// The CFrame type's metatable for a VM (ltypes `CFRAME_TAG`): its members
/// (`LCFrame::add_fields` / `add_methods`, written against mlua's
/// registration traits) collected by `NativeReg` into the getter and
/// method tables `install_parts` builds the C `__index` / `__newindex` /
/// `__namecall` from, then set as the tag's metatable. Once per VM; done
/// by ltypes' install (and on the first CFrame made in a VM without it).
pub fn install_cframe(lua: &Lua) -> mlua::Result<()> {
    let ready = unsafe { lua.exec_raw::<bool>((), |l| ffi::lua_pushboolean(l, crate::ltypes::cframe_ready(l) as c_int))? };
    if ready {
        return Ok(());
    }
    let mut reg = NativeReg::new(lua, "LCFrame")?;
    crate::ltypes::LCFrame::add_fields(&mut reg);
    crate::ltypes::LCFrame::add_methods(&mut reg);
    if let Some(e) = reg.err.take() {
        return Err(e);
    }
    let mt = reg.mt.clone();
    let meta = meta("CFrame").expect("CFrame spec");
    let parts = Parts { getters: Value::Table(reg.getters.clone()), methods: Value::Table(reg.methods.clone()), fallback: Value::Nil };
    install_parts(lua, &mt, meta, Value::Table(mt.clone()), parts, Value::Nil, Value::Nil)?;
    // `==` compares the components (as LCFrame's PartialEq), in C.
    let eq = unsafe { lua.exec_raw::<Function>((), |l| ffi::lua_pushcclosurek(l, cframe_eq, std::ptr::null(), 0, None))? };
    mt.raw_set("__eq", eq)?;
    mt.raw_set("__rs_dtmeta", true)?;
    unsafe {
        lua.exec_raw::<()>(&mt, |l| {
            ffi::lua_setuserdatametatable(l, crate::ltypes::CFRAME_TAG);
            crate::ltypes::cframe_push(l, rustudio_core::math::Xform { pos: glam::Vec3::ZERO, rot: glam::Mat3::IDENTITY });
            crate::ltypes::probe_cframe_header(l);
            ffi::lua_settop(l, 0);
        })?;
    }
    Ok(())
}

/// CFrame `__eq` (Luau calls it for two userdata sharing it: two CFrames).
unsafe extern "C-unwind" fn cframe_eq(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let same = match (cframe_idx(l, 1), cframe_idx(l, 2)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        ffi::lua_pushboolean(l, same as c_int);
        1
    }
}

/// Collects a natively stored type's members as mlua's userdata registry
/// would, as plain functions taking self (converted with `FromLua`) first.
/// Only the registration calls the native types use are supported; others
/// fail `install_cframe`.
struct NativeReg {
    lua: Lua,
    ty: &'static str,
    mt: mlua::Table,
    getters: mlua::Table,
    methods: mlua::Table,
    err: Option<mlua::Error>,
}

impl NativeReg {
    fn new(lua: &Lua, ty: &'static str) -> mlua::Result<Self> {
        Ok(NativeReg {
            lua: lua.clone(),
            ty,
            mt: lua.create_table()?,
            getters: lua.create_table()?,
            methods: lua.create_table()?,
            err: None,
        })
    }
    fn keep(&mut self, r: mlua::Result<()>) {
        if let Err(e) = r {
            self.err.get_or_insert(e);
        }
    }
    fn unsupported(&mut self, what: &str, name: String) {
        self.err.get_or_insert(mlua::Error::runtime(format!("NativeReg: {what} not supported ({name})")));
    }
    /// `f(self, args…)` with mlua's argument positions (self #1, the rest
    /// from #2).
    fn self_fn<T, A, R>(
        &self,
        name: &str,
        f: impl Fn(&Lua, &T, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
    ) -> mlua::Result<Function>
    where
        T: mlua::FromLua + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        // (mlua's stack conversion; the error path names the method,
        // `reraise_named`.)
        let _ = name;
        self.lua.create_function(move |lua, (this, a): (T, A)| f(lua, &this, a))
    }
}

impl<T: mlua::FromLua + 'static> mlua::UserDataFields<T> for NativeReg {
    fn add_field<V: mlua::IntoLua + 'static>(&mut self, name: impl Into<String>, _value: V) {
        self.unsupported("add_field", name.into());
    }
    fn add_field_method_get<M, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        R: mlua::IntoLua,
    {
        let name = name.into();
        let r = self.self_fn(&name, move |lua, this: &T, ()| method(lua, this)).and_then(|f| self.getters.raw_set(name, f));
        self.keep(r);
    }
    fn add_field_method_set<M, A>(&mut self, name: impl Into<String>, _method: M)
    where
        M: FnMut(&Lua, &mut T, A) -> mlua::Result<()> + mlua::MaybeSend + 'static,
        A: mlua::FromLua,
    {
        self.unsupported("add_field_method_set", name.into());
    }
    fn add_field_function_get<F, R>(&mut self, name: impl Into<String>, _function: F)
    where
        F: Fn(&Lua, mlua::AnyUserData) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        R: mlua::IntoLua,
    {
        self.unsupported("add_field_function_get", name.into());
    }
    fn add_field_function_set<F, A>(&mut self, name: impl Into<String>, _function: F)
    where
        F: FnMut(&Lua, mlua::AnyUserData, A) -> mlua::Result<()> + mlua::MaybeSend + 'static,
        A: mlua::FromLua,
    {
        self.unsupported("add_field_function_set", name.into());
    }
    fn add_meta_field<V: mlua::IntoLua + 'static>(&mut self, name: impl Into<String>, value: V) {
        let r = value.into_lua(&self.lua).and_then(|v| self.mt.raw_set(name.into(), v));
        self.keep(r);
    }
    fn add_meta_field_with<F, R>(&mut self, name: impl Into<String>, f: F)
    where
        F: FnOnce(&Lua) -> mlua::Result<R> + 'static,
        R: mlua::IntoLua,
    {
        let r = f(&self.lua).and_then(|v| v.into_lua(&self.lua)).and_then(|v| self.mt.raw_set(name.into(), v));
        self.keep(r);
    }
}

impl<T: mlua::FromLua + 'static> mlua::UserDataMethods<T> for NativeReg {
    fn add_method<M, A, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        let name = name.into();
        let r = self.self_fn(&name, method).and_then(|f| self.methods.raw_set(name, f));
        self.keep(r);
    }
    fn add_method_mut<M, A, R>(&mut self, name: impl Into<String>, _method: M)
    where
        M: FnMut(&Lua, &mut T, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        self.unsupported("add_method_mut", name.into());
    }
    fn add_function<F, A, R>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        let name = name.into();
        let r = self.lua.create_function(function).and_then(|f| self.methods.raw_set(name, f));
        self.keep(r);
    }
    fn add_function_mut<F, A, R>(&mut self, name: impl Into<String>, _function: F)
    where
        F: FnMut(&Lua, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        self.unsupported("add_function_mut", name.into());
    }
    fn add_meta_method<M, A, R>(&mut self, name: impl Into<String>, method: M)
    where
        M: Fn(&Lua, &T, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        let name = name.into();
        let r = self.self_fn(&name, method).and_then(|f| self.mt.raw_set(name, f));
        self.keep(r);
    }
    fn add_meta_method_mut<M, A, R>(&mut self, name: impl Into<String>, _method: M)
    where
        M: FnMut(&Lua, &mut T, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        self.unsupported("add_meta_method_mut", name.into());
    }
    fn add_meta_function<F, A, R>(&mut self, name: impl Into<String>, function: F)
    where
        F: Fn(&Lua, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        let name = name.into();
        let r = self.lua.create_function(function).and_then(|f| self.mt.raw_set(name, f));
        self.keep(r);
    }
    fn add_meta_function_mut<F, A, R>(&mut self, name: impl Into<String>, _function: F)
    where
        F: FnMut(&Lua, A) -> mlua::Result<R> + mlua::MaybeSend + 'static,
        A: mlua::FromLuaMulti,
        R: mlua::IntoLuaMulti,
    {
        self.unsupported("add_meta_function_mut", name.into());
    }
}

/// `self_mt`: the metatable self must have, or `true` for native vectors.
pub fn install_on(lua: &Lua, mt: &mlua::Table, meta: &'static TypeMeta, self_mt: Value) -> mlua::Result<()> {
    let parts = index_parts(lua, &mt.raw_get("__index")?)?;
    let (setters, newfallback) = newindex_parts(lua, &mt.raw_get("__newindex")?)?;
    install_parts(lua, mt, meta, self_mt, parts, setters, newfallback)
}

/// Vector3 (Luau's native vector): the vector type's metatable, built from
/// `getters` / `methods` (functions taking the vector first).
pub fn install_vector(lua: &Lua, getters: mlua::Table, methods: mlua::Table) -> mlua::Result<()> {
    let mt = lua.create_table()?;
    mt.raw_set("__type", "Vector3")?;
    let parts = Parts { getters: Value::Table(getters), methods: Value::Table(methods), fallback: Value::Nil };
    install_parts(lua, &mt, meta("Vector3").expect("Vector3 spec"), Value::Boolean(true), parts, Value::Nil, Value::Nil)?;
    lua.set_type_metatable::<mlua::Vector>(Some(mt));
    Ok(())
}

fn install_parts(
    lua: &Lua,
    mt: &mlua::Table,
    meta: &'static TypeMeta,
    self_mt: Value,
    parts: Parts,
    setters: Value,
    newfallback: Value,
) -> mlua::Result<()> {
    let errtext = errtext(lua)?;
    let meta_ptr = mlua::LightUserData(meta as *const TypeMeta as *mut c_void);

    let index = unsafe {
        lua.exec_raw::<Function>(
            (
                parts.getters.clone(),
                parts.methods.clone(),
                parts.fallback.clone(),
                meta_ptr,
                errtext.clone(),
                self_mt.clone(),
            ),
            |l| ffi::lua_pushcclosurek(l, index_fn, std::ptr::null(), 6, None),
        )?
    };
    let newindex = unsafe {
        lua.exec_raw::<Function>(
            (
                setters,
                parts.getters.clone(),
                parts.methods.clone(),
                newfallback,
                meta_ptr,
                errtext.clone(),
                self_mt.clone(),
            ),
            |l| ffi::lua_pushcclosurek(l, newindex_fn, std::ptr::null(), 7, None),
        )?
    };
    mt.raw_set("__index", index)?;
    mt.raw_set("__newindex", newindex)?;
    if meta.quoted {
        mt.raw_set("__namecall", Value::Nil)?;
    } else {
        let namecall = unsafe {
            lua.exec_raw::<Function>((parts.methods.clone(), meta_ptr, errtext.clone()), |l| {
                ffi::lua_pushcclosurek(l, namecall_fn, std::ptr::null(), 3, None)
            })?
        };
        mt.raw_set("__namecall", namecall)?;
    }
    let keep = studio_ops(meta.name);
    for op in ARITH {
        let v: Value = mt.raw_get(*op)?;
        let Value::Function(f) = v else { continue };
        if keep.contains(op) {
            // CFrame's * + -: a fast path for CFrame and Vector3 operands
            // (upvalue 3 which operator, 4 the type's metatable).
            let fast: i32 = match (meta.name, *op) {
                _ if !dt_fast() => 0,
                ("CFrame", "__mul") => 1,
                ("CFrame", "__add") => 2,
                ("CFrame", "__sub") => 3,
                ("Vector2", "__add") => 4,
                ("Vector2", "__sub") => 5,
                ("Vector2", "__mul") => 6,
                ("Vector2", "__div") => 7,
                _ => 0,
            };
            let w = unsafe {
                lua.exec_raw::<Function>((f, errtext.clone(), fast, self_mt.clone()), |l| {
                    ffi::lua_pushcclosurek(l, op_fn, std::ptr::null(), 4, None)
                })?
            };
            mt.raw_set(*op, w)?;
        } else if *op != "__call" {
            mt.raw_set(*op, Value::Nil)?;
        }
    }
    mt.raw_set("__metatable", crate::cmeta::LOCKED)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn catalog_params_use_the_studio_off_sale_key() {
        let rt = crate::runtime::Runtime::new().unwrap();
        use crate::runtime::{RunMode, World};
        rt.enter(World::new(rustudio_core::doc::Document::new_place(), RunMode::Run));
        rt.bind_globals().unwrap();
        rt.lua.load(r#"
            local p = CatalogSearchParams.new()
            assert(p.IncludeOffSale == false)
            p.IncludeOffSale = true
            assert(p.IncludeOffSale == true and string.find(tostring(p), "IncludeOffSale=true"))
            assert(not pcall(function() return p.IncludeOffSaleItems end))
            assert(not pcall(function() p.IncludeOffSaleItems = true end))
        "#).exec().unwrap();
    }

    use super::*;

    /// The CFrame fast paths (constructors, * + -, methods) give exactly
    /// what the Rust implementation gives, bit for bit, errors included.
    #[test]
    fn cframe_fast_paths_match_the_implementation() {
        let src = r#"
            local out = {}
            local function bits(v)
                if typeof(v) == "CFrame" then return "CF(" .. table.concat({ v:GetComponents() }, ",") .. ")" end
                if typeof(v) == "Vector3" then return "V(" .. v.X .. "," .. v.Y .. "," .. v.Z .. ")" end
                return typeof(v) .. ":" .. tostring(v)
            end
            local function try(name, f)
                local ok, v = pcall(f)
                table.insert(out, name .. "=" .. (ok and bits(v) or ("ERR " .. tostring(v))))
            end
            local xs = { 0, 1, -2.5, 1 / 3, 1e7, -1e-7, 123.456, math.pi, 1e39, -1e39, 0 / 0, math.huge }
            for i, x in xs do
                local y, z = xs[(i % #xs) + 1], xs[((i + 4) % #xs) + 1]
                try("new3 " .. i, function() return CFrame.new(x, y, z) end)
                try("angles " .. i, function() return CFrame.Angles(x, y, z) end)
                try("xyz " .. i, function() return CFrame.fromEulerAnglesXYZ(z, x, y) end)
                try("orient " .. i, function() return CFrame.fromOrientation(x, y, z) end)
                try("yxz " .. i, function() return CFrame.fromEulerAnglesYXZ(y, z, x) end)
                try("new12 " .. i, function() return CFrame.new(x, y, z, 1, 0, 0, 0, z, 1, y, 0, x) end)
            end
            local a = CFrame.new(1, 2, 3) * CFrame.Angles(0.1, 0.2, 0.3)
            local b = CFrame.fromEulerAnglesYXZ(0.5, -0.4, 1.2) + Vector3.new(4, -5, 6)
            local v = Vector3.new(1.5, -2.25, 3)
            try("new0", function() return CFrame.new() end)
            try("new1", function() return CFrame.new(v) end)
            try("new2", function() return CFrame.new(v, Vector3.new(-4, 5, 9)) end)
            try("new2 up", function() return CFrame.new(v, v + Vector3.new(0, 5, 0)) end)
            try("new7", function() return CFrame.new(1, 2, 3, 0, 0, 0, 1) end)
            try("new str", function() return CFrame.new("1", 2, 3) end)
            try("new bad", function() return CFrame.new(1, 2) end)
            try("new nil", function() return CFrame.new(1, nil, 3) end)
            try("angles 2", function() return CFrame.Angles(1, 2) end)
            try("angles 4", function() return CFrame.Angles(1, 2, 3, 4) end)
            try("mul", function() return a * b end)
            try("mul v", function() return a * v end)
            try("mul n", function() return a * 2 end)
            try("mul s", function() return a * "x" end)
            try("add", function() return a + v end)
            try("sub", function() return a - v end)
            try("add cf", function() return a + b end)
            try("v add", function() return v + a end)
            try("inverse", function() return a:Inverse() end)
            try("inverse 2", function() return a:Inverse(b) end)
            try("tows", function() return a:ToWorldSpace(b) end)
            try("toos", function() return a:ToObjectSpace(b) end)
            try("toos v", function() return a:ToObjectSpace(v) end)
            try("ptws", function() return a:PointToWorldSpace(v) end)
            try("ptos", function() return a:PointToObjectSpace(v) end)
            try("vtws", function() return a:VectorToWorldSpace(v) end)
            try("vtos", function() return a:VectorToObjectSpace(v) end)
            try("ptos cf", function() return a:PointToObjectSpace(b) end)
            try("lerp", function() return a:Lerp(b, 0.3) end)
            try("lerp str", function() return a:Lerp(b, "0.3") end)
            try("lerp v", function() return a:Lerp(v, 0.3) end)
            try("lower", function() return a:toObjectSpace(b) * a:inverse() end)
            return table.concat(out, "\n")
        "#;
        let fast = run(&lua(), src);
        DT_FAST.store(false, std::sync::atomic::Ordering::Relaxed);
        let slow = run(&lua(), src);
        DT_FAST.store(true, std::sync::atomic::Ordering::Relaxed);
        for (f, s) in fast.lines().zip(slow.lines()) {
            assert_eq!(f, s);
        }
        assert_eq!(fast.lines().count(), slow.lines().count());
        assert!(fast.contains("new3 2=CF(1,-2.5,"), "{fast}");
    }

    fn lua() -> Lua {
        let lua = Lua::new();
        crate::ltypes::install(&lua, &lua.globals()).unwrap();
        install_all(&lua).unwrap();
        lua
    }

    fn run(lua: &Lua, src: &str) -> String {
        lua.load(src).set_name("=T").eval::<String>().unwrap_or_else(|e| panic!("{src}: {e}"))
    }

    /// DateTime local time is the machine's zone (Argon names its undo
    /// steps with FormatLocalTime("LTS")).
    #[test]
    fn date_time_local_time_uses_the_machine_zone() {
        use chrono::{Local, TimeZone};
        let lua = lua();
        let ms: i64 = 1_763_215_200_000; // 2025-11-15 14:00:00 UTC
        let off = Local.timestamp_millis_opt(ms).single().unwrap().offset().local_minus_utc() as i64;
        let got = run(&lua, &format!("local d = DateTime.fromUnixTimestampMillis({ms}) local l, u = d:ToLocalTime(), d:ToUniversalTime() return ((l.Day * 24 + l.Hour) * 60 + l.Minute) - ((u.Day * 24 + u.Hour) * 60 + u.Minute) .. '|' .. d:FormatUniversalTime('LTS', 'en-us')"));
        assert_eq!(got, format!("{}|2:00:00 PM", off / 60));
        let want = (Local.timestamp_millis_opt(ms).single().unwrap()).format("%-I:%M:%S %p").to_string();
        assert_eq!(run(&lua, &format!("return DateTime.fromUnixTimestampMillis({ms}):FormatLocalTime('LTS', 'en-us')")), want);
    }

    /// the implementation rows read / write / writeX / namecall / *.info.
    #[test]
    fn members_and_raisers_like_studio() {
        let lua = lua();
        let probe = r#"
            local o = ...
            local function e(f) local ok, m = pcall(f) return tostring(ok) .. "|" .. type(m) .. "|" .. tostring(m) end
            local function cap(f) local r; xpcall(f, function() r = debug.info(2, "f") end) return r end
            local rd = cap(function() return o.____ end)
            local s, l, n, a, v = debug.info(rd, "slna")
            return table.concat({
                e(function() return o.____ end), e(function() o.____ = 1 end), e(function() o.X = 1 end),
                e(function() return o:____() end), s .. l .. n .. a .. tostring(v),
                e(function() return rd() end), e(function() return rd({}, {}) end),
                tostring(getmetatable(o)),
            }, " ## ")
        "#;
        let f = lua.load(probe).set_name("=T").into_function().unwrap();
        let cases: &[(&str, &str)] = &[
            ("Vector2.new()", "false|string|____ is not a valid member of Vector2 ## false|string|____ is not a valid member of Vector2 ## false|string|X member of Vector2 is read-only and cannot be assigned to ## false|string|____ is not a valid member of Vector2 ## [C]-10true ## false|string|T:10: missing argument #2 (string expected) ## false|string|T:10: invalid argument #2 (string expected, got table) ## The metatable is locked"),
            ("UDim.new()", "false|string|'____' is not a valid member of UDim ## false|string|____ is not a valid member of UDim ## false|string|X is not a valid member of UDim ## false|string|'____' is not a valid member of UDim ## [C]-10true ## false|string|T:10: missing argument #2 (string expected) ## false|string|T:10: invalid argument #2 (string expected, got table) ## The metatable is locked"),
        ];
        for (ctor, want) in cases {
            let o: Value = lua.load(*ctor).eval().unwrap();
            let got: String = f.call(o).unwrap();
            assert_eq!(got, *want, "{ctor}");
        }
        // Same raiser for every value; methods are fresh named C functions.
        assert_eq!(
            run(&lua, r#"
                local function cap(f) local r; xpcall(f, function() r = debug.info(2, "f") end) return r end
                local a, b = Vector2.new(), Vector2.new(1, 2)
                local d = a.Dot
                return tostring(cap(function() return a.x_ end) == cap(function() return b.x_ end))
                    .. tostring(d == a.Dot) .. debug.info(d, "n") .. "|" .. select(2, pcall(d)) .. "|" .. select(2, pcall(d, {}))
                    .. "|" .. select(2, pcall(function() return CFrame.new().GetComponents() end))
            "#),
            "truefalseDot|Argument 1 missing or nil|Unable to cast Array to Vector2|T:7: missing argument #1 to 'GetComponents' (CFrame expected)"
        );
    }

    /// Operators Studio lacks are absent (the VM's message); kept ones
    /// check their arguments.
    #[test]
    fn operators_like_studio() {
        let lua = lua();
        let e = |src: &str| run(&lua, &format!("local ok, m = pcall(function() return {src} end) return tostring(m)"));
        assert_eq!(e("Ray.new(Vector3.new(), Vector3.new()) + 1"), "T:1: attempt to perform arithmetic (add) on Ray and number");
        assert_eq!(e("UDim.new() + 1"), "T:1: invalid argument #2 (UDim expected, got number)");
        assert_eq!(e("CFrame.new() + 1"), "T:1: invalid argument #2 (Vector3 expected, got number)");
        assert_eq!(e("DateTime.now() < DateTime.now()"), "T:1: attempt to compare DateTime < DateTime");
    }

    /// Studio probes constructor / constructor.bad / values / writeKnown
    /// rows and the message replacement inventory.
    #[test]
    fn constructors_and_messages_like_studio() {
        let lua = lua();
        let e = |src: &str| {
            run(&lua, &format!("local ok, m = pcall(function() return {src} end) return tostring(ok) .. '|' .. type(m) .. '|' .. tostring(m)"))
        };
        assert_eq!(e("table.isfrozen(Vector3) and getmetatable(Vector3) == nil"), "true|boolean|true");
        assert_eq!(e("(function() Vector3.foo = 1 end)()"), "false|string|T:1: attempt to modify a readonly table");
        assert_eq!(run(&lua, "return debug.info(Vector3.new, 'n') .. debug.info(PhysicalProperties.new, 'n')"), "newnewPhysicalPropertiesWithClampedMsgClosure");
        assert_eq!(e("tostring(Vector2.new('a').X + UDim2.new({}).X.Offset + Color3.new(nil).R)"), "true|string|0");
        // OverlapParams.IncludeInstances / ExcludeInstances (Brushtool's sweeps)
        assert_eq!(
            e("(function() local o = OverlapParams.new() o.IncludeInstances = {1, 2} local a = o.FilterType.Name .. #o.FilterDescendantsInstances .. #o.ExcludeInstances o.ExcludeInstances = {3} return a .. o.FilterType.Name .. #o.IncludeInstances end)()"),
            "true|string|Include20Exclude0"
        );
        // UDim2.new(UDim, UDim) (Archimedes' scroll frame sizes its clip area so)
        assert_eq!(e("tostring(UDim2.new(UDim.new(1, -16), UDim.new(0.5, 3)))"), "true|string|{1, -16}, {0.5, 3}");
        assert_eq!(e("CFrame.new('a')"), "false|string|T:1: invalid argument #1 to 'new' (Vector3 expected, got string)");
        assert_eq!(e("CFrame.new(1, 2)"), "false|string|T:1: invalid argument #1 to 'new' (Vector3 expected, got number)");
        assert_eq!(e("Random.new('a')"), "false|string|T:1: invalid argument #1 to 'new' (number expected, got string)");
        assert_eq!(e("Ray.new('a')"), "false|string|T:1: invalid argument #1 to 'new' (Vector3 expected, got string)");
        assert_eq!(e("Rect.new('a')"), "false|string|Invalid number of arguments: 1");
        assert_eq!(e("TweenInfo.new('a')"), "false|string|TweenInfo.new first argument expects a number for time.");
        assert_eq!(e("NumberSequence.new('a')"), "false|string|NumberSequence.new(): table of NumberSequenceKeypoints expected.");
        assert_eq!(e("ColorSequence.new('a')"), "false|string|ColorSequence.new(): table expected.");
        assert_eq!(e("tostring(Faces.new('a'))"), "true|string|");
        assert_eq!(e("Random.new(42) == Random.new(42) and RaycastParams.new() == RaycastParams.new() and OverlapParams.new() == OverlapParams.new() and CatalogSearchParams.new() == CatalogSearchParams.new()"), "true|boolean|true");
        assert_eq!(e("Vector2.new() * Color3.new()"), "false|string|attempt to multiply a Vector2 with an incompatible value type or nil");
        assert_eq!(e("tostring(Vector3int16.new(1, 2, 3) * Color3.new())"), "true|string|0, 0, 0");
        assert_eq!(e("CFrame.new() * Color3.new()"), "false|string|T:1: invalid argument #2 (Vector3 expected, got Color3)");
        assert_eq!(e("(-UDim.new(0.5, 7)).Offset"), "true|number|-7");
        assert_eq!(e("(Vector2.new(3, 5) // 2).Y"), "true|number|2");
        assert_eq!(e("(function() local f = Font.new('a') f.Family = 'b' f.Bold = true return f.Family .. tostring(f.Bold) end)()"), "true|string|btrue");
    }

    /// Studio probes `create` / `unm` rows: per-type number formatting.
    #[test]
    fn tostring_like_studio() {
        use crate::ltypes::fmt_g;
        assert_eq!(fmt_g((1.0f32 / 3.0) as f64, 9), "0.333333343");
        assert_eq!(fmt_g((1.0f32 / 3.0) as f64, 6), "0.333333");
        assert_eq!(fmt_g(-0.0, 9), "-0");
        assert_eq!(fmt_g(1e20, 6), "1e+20");
        assert_eq!(fmt_g(0.0001, 6), "0.0001");
        assert_eq!(fmt_g(0.00001, 6), "1e-05");
        assert_eq!(fmt_g(10000.6, 6), "10000.6");
        assert_eq!(fmt_g(123456789.0, 6), "1.23457e+08");
        let lua = lua();
        let cases = [
            ("Vector2.new(1/3, -0)", "0.333333343, -0"),
            ("-Vector2.new(1/3, -0)", "-0.333333343, 0"),
            ("CFrame.new(1/3, 2, 3)", "0.333333343, 2, 3, 1, 0, 0, 0, 1, 0, 0, 0, 1"),
            ("Color3.new(1/3, 0.5, 1)", "0.333333, 0.5, 1"),
            ("UDim.new(1/3, 7)", "0.333333343, 7"),
            ("UDim2.new(1/3, 7, 0.5, -8)", "{0.333333343, 7}, {0.5, -8}"),
            ("NumberRange.new(1/3, 2)", "0.333333 2 "),
            ("NumberSequenceKeypoint.new(0, 0.5, 0.1)", "0 0.5 0.1 "),
            ("TweenInfo.new(1/3)", "Time:0.333333 DelayTime:0 RepeatCount:0 Reverses:False EasingDirection:Out EasingStyle:Quad"),
            ("PhysicalProperties.new(1, 0.5, 0.5)", "1, 0.5, 0.5, 1, 1, 1"),
            ("DateTime.fromUnixTimestamp(0)", "0"),
            ("RaycastParams.new()", "RaycastParams{IgnoreWater=false, BruteForceAllSlow=false, RespectCanCollide=false, CollisionGroup=Default, FilterDescendantsInstances={}}"),
            ("OverlapParams.new()", "OverlapParams{MaxParts=0, Tolerance=0, BruteForceAllSlow=false, RespectCanCollide=false, CollisionGroup=Default, FilterDescendantsInstances={}}"),
            ("CatalogSearchParams.new()", "CatalogSearchParams{SearchKeyword=, CategoryFilter=None, SortType=Relevance, SortAggregation=AllTime, SalesTypeFilter=All, CreatorName=, Limit=30, CreatorType=All, BundleTypes=, AssetTypes=, IncludeOffSale=false}"),
        ];
        for (expr, want) in cases {
            assert_eq!(run(&lua, &format!("return tostring({expr})")), want, "{expr}");
        }
    }

    /// Studio probes Vector3 rows: Luau's native vector.
    #[test]
    fn vector3_is_native() {
        let lua = lua();
        let got = run(&lua, r#"
            local v, w = Vector3.new(1/3, -0, 1e20), Vector3.new(1/3, -0, 1e20)
            local t = { [v] = 1 }
            local function e(f) local ok, m = pcall(f) return tostring(m) end
            local function cap(f) local r; xpcall(f, function() r = debug.info(2, "f") end) return r end
            return table.concat({
                type(v), typeof(v), tostring(v), tostring(getmetatable(v)),
                tostring(v == w), tostring(rawequal(v, w)), tostring(t[w]),
                tostring(-v), tostring(v // 2), tostring(v.X), tostring(v.Magnitude > 0),
                e(function() return v.____ end), e(function() v.X = 1 end),
                e(function() return v:____() end), e(function() return v + 1 end),
                debug.info(v.Dot, "n"), tostring(v.Dot == v.Dot),
                e(function() return v.Dot() end), e(function() return cap(function() return v.zz end)() end),
                tostring(Vector3.new(1, 2, 3):Cross(Vector3.xAxis)), tostring(CFrame.new(1, 2, 3) * Vector3.one),
            }, " # ")
        "#);
        assert_eq!(
            got,
            "vector # Vector3 # 0.3333333432674408, -0, 100000002004087730000 # The metatable is locked # \
             true # true # 1 # -0.3333333432674408, 0, -100000002004087730000 # 0, -0, 50000001002043870000 # \
             0.3333333432674408 # true # ____ is not a valid member of Vector3 # X cannot be assigned to # \
             ____ is not a valid member of Vector3 # T:11: attempt to perform arithmetic (add) on Vector3 and number # \
             Dot # false # T:13: missing argument #1 to 'Dot' (Vector3 expected) # T:13: missing argument #2 (string expected) # \
             0, 3, -2 # 2, 3, 4"
        );
    }

    /// Studio probes PhysicalProperties.constructor.bad: a lone non-number
    /// is cast to a Material; an unknown one gives the "default" value.
    #[test]
    fn physical_properties_default() {
        let lua = lua();
        assert_eq!(run(&lua, "return tostring(PhysicalProperties.new('a'))"), "default");
        assert_eq!(run(&lua, "return tostring(PhysicalProperties.new('Plastic'))"), tostring_pp("Plastic"));
        assert_eq!(run(&lua, "return typeof(PhysicalProperties.new('a'))"), "PhysicalProperties");
    }

    fn tostring_pp(m: &str) -> String {
        let lua = lua();
        run(&lua, &format!("return tostring(PhysicalProperties.new(Enum.Material.{m}))"))
    }

    /// RaycastResult's metatable is the quoted family's (Studio probes
    /// RaycastResult read / write / namecall / writeKnown rows).
    #[test]
    fn raycast_result_metatable() {
        let lua = lua();
        let r = lua
            .create_userdata(crate::ltypes::LRaycastResult {
                name: "Part".into(),
                material: crate::ltypes::LEnumItem::from_name("Material", "Plastic"),
                ..crate::ltypes::LRaycastResult::sample()
            })
            .unwrap();
        let f = lua
            .load(r#"
                local o = ...
                local function e(f) local ok, m = pcall(f) return tostring(ok) .. "|" .. tostring(m) end
                local function cap(f) local r; xpcall(f, function() r = debug.info(2, "f") end) return r end
                local rd = cap(function() return o.____ end)
                return table.concat({
                    typeof(o), tostring(getmetatable(o)), e(function() return o.____ end), e(function() o.X = 1 end),
                    e(function() o.Distance = 1 end), e(function() return o:____() end), e(function() return #o end),
                    debug.info(rd, "s"), e(function() return rd() end), tostring(o),
                }, " # ")
            "#)
            .set_name("=T")
            .into_function()
            .unwrap();
        assert_eq!(
            f.call::<String>(r).unwrap(),
            "RaycastResult # The metatable is locked # false|'____' is not a valid member of RaycastResult # \
             false|X is not a valid member of RaycastResult # \
             false|Distance member of RaycastResult is read-only and cannot be assigned to # \
             false|'____' is not a valid member of RaycastResult # false|T:8: attempt to get length of a RaycastResult value # \
             [C] # false|T:9: missing argument #2 (string expected) # \
             RaycastResult{Part @ 0, 0, 0; normal = 0, 1, 0; material = Plastic}"
        );
    }

    #[test]
    fn type_names_of_mlua_targets() {
        assert_eq!(lua_type_of("LVector3"), "Vector3");
        assert_eq!(lua_type_of("f32"), "number");
        assert_eq!(lua_type_of("rustudio::ltypes::LCFrame"), "CFrame");
        assert_eq!(lua_type_of("Length"), "Length");
    }
}

/// Samples of every datatype made by its global constructor (types whose
/// constructor is missing in this VM are skipped).
const SAMPLES: &str = r#"
local out = {}
local function try(f) local ok, v = pcall(f) if ok and type(v) == "userdata" then table.insert(out, v) end end
try(function() return Vector2.new() end)
try(function() return Vector3int16.new() end)
try(function() return Vector2int16.new() end)
try(function() return CFrame.new() end)
try(function() return Color3.new() end)
try(function() return BrickColor.new(1) end)
try(function() return UDim.new() end)
try(function() return UDim2.new() end)
try(function() return Rect.new(0, 0, 0, 0) end)
try(function() return Ray.new(Vector3.new(), Vector3.new()) end)
try(function() return Region3.new(Vector3.new(), Vector3.new()) end)
try(function() return Region3int16.new(Vector3int16.new(), Vector3int16.new()) end)
try(function() return NumberRange.new(0) end)
try(function() return NumberSequence.new(0) end)
try(function() return NumberSequenceKeypoint.new(0, 0) end)
try(function() return ColorSequence.new(Color3.new()) end)
try(function() return ColorSequenceKeypoint.new(0, Color3.new()) end)
try(function() return TweenInfo.new() end)
try(function() return PhysicalProperties.new(1, 1, 1) end)
try(function() return Faces.new() end)
try(function() return Axes.new() end)
try(function() return Font.new("rbxasset://fonts/families/Arial.json") end)
try(function() return DateTime.fromUnixTimestamp(0) end)
try(function() return Random.new(1) end)
try(function() return OverlapParams.new() end)
try(function() return RaycastParams.new() end)
try(function() return CatalogSearchParams.new() end)
try(function() return PathWaypoint.new(Vector3.new()) end)
try(function() return Content.fromUri("rbxassetid://1") end)
return out
"#;

/// Rebuilds every datatype metatable (`install`) and constructor table
/// (`install_constructors`), once per VM.
pub fn install_all(lua: &Lua) -> mlua::Result<()> {
    let samples: Vec<Value> = lua.load(SAMPLES).set_name("=rustudio.dtmeta").eval()?;
    for v in samples {
        if let Value::UserData(ud) = v {
            install(lua, &ud)?;
        }
    }
    install(lua, &lua.create_userdata(crate::ltypes::LRaycastResult::sample())?)?;
    crate::sharedtable::seal(lua)?;
    install_constructors(lua)
}

// ---------------------------------------------------------- constructors

/// The datatype globals: frozen tables in Studio (Studio probes
/// `constructor` rows: `table.isfrozen` true, writes fail with "attempt to
/// modify a readonly table").
const CONSTRUCTORS: &[&str] = &[
    "Vector3", "Vector2", "Vector3int16", "Vector2int16", "CFrame", "Color3", "BrickColor", "UDim", "UDim2",
    "Rect", "Ray", "Region3", "Region3int16", "NumberRange", "NumberSequence", "NumberSequenceKeypoint",
    "ColorSequence", "ColorSequenceKeypoint", "TweenInfo", "PhysicalProperties", "Faces", "Axes", "Font",
    "DateTime", "Random", "OverlapParams", "RaycastParams", "CatalogSearchParams", "PathWaypoint", "Content",
    "SharedTable",
];

/// Constructors that take anything for a number (Studio: `Vector3.new('a')`,
/// `Color3.fromRGB(nil)`, `UDim2.new({})` succeed with 0).
fn lenient(ty: &str, f: &str) -> bool {
    matches!(
        (ty, f),
        ("Vector3" | "Vector2" | "Vector3int16" | "Vector2int16" | "UDim" | "UDim2" | "Color3", "new")
            | ("Color3", "fromRGB" | "fromHSV")
            | ("UDim2", "fromScale" | "fromOffset")
    )
}

/// Studio's debug name of a constructor function when it is not its key.
fn ctor_name(ty: &str, f: &str) -> Option<&'static CStr> {
    match (ty, f) {
        ("PhysicalProperties", "new") => Some(c"newPhysicalPropertiesWithClampedMsgClosure"),
        _ => None,
    }
}

/// A constructor. Upvalues: 1 the implementation, 2 errtext, 3 lenient
/// (non-numbers become 0), 4 its fast path (`ctor_fast_id`, 0 none).
/// Argument errors get the function's name.
unsafe extern "C-unwind" fn ctor_fn(l: *mut ffi::lua_State) -> c_int {
    unsafe {
        let n = ffi::lua_gettop(l);
        let fast = ffi::lua_tointegerx(l, ffi::lua_upvalueindex(4), std::ptr::null_mut());
        if fast == 4 {
            // Color3.new(r, g, b), three plain numbers (a vertex painter
            // makes one per vertex per frame): no call into the Rust
            // implementation. Anything else takes the path below.
            if n == 3 {
                if let (Some(r), Some(g), Some(b)) = (num_at(l, 1), num_at(l, 2), num_at(l, 3)) {
                    let lua = Lua::get_or_init_from_ptr(l);
                    if ffi::lua_checkstack(l, 3) != 0
                        && matches!(lua.push_userdata_unref(l, crate::ltypes::LColor3([r, g, b])), Ok(true))
                    {
                        return 1;
                    }
                }
            }
        } else if (5..=9).contains(&fast) {
            if let Some(r) = small_ctor_fast(l, fast, n) {
                return r;
            }
        } else if fast != 0 {
            if let Some(x) = cf_ctor_fast(l, fast, n) {
                if push_cframe(l, x) {
                    return 1;
                }
            }
        }
        let mode = ffi::lua_tointegerx(l, ffi::lua_upvalueindex(3), std::ptr::null_mut());
        // UDim2.new(UDim, UDim) keeps its two UDims (Archimedes' scroll frame).
        let udim_pair = mode == 2
            && n == 2
            && ffi::lua_type(l, 1) == ffi::LUA_TUSERDATA
            && ffi::lua_type(l, 2) == ffi::LUA_TUSERDATA;
        if mode != 0 && !udim_pair {
            for i in 1..=n {
                if ffi::lua_type(l, i) != ffi::LUA_TNUMBER {
                    let mut ok = 0;
                    let x = ffi::lua_tonumberx(l, i, &mut ok);
                    ffi::lua_pushnumber(l, if ok != 0 { x } else { 0.0 });
                    ffi::lua_replace(l, i);
                }
            }
        }
        ffi::lua_pushvalue(l, ffi::lua_upvalueindex(1));
        ffi::lua_insert(l, 1);
        if ffi::lua_pcall(l, n, ffi::LUA_MULTRET, 0) == 0 {
            return ffi::lua_gettop(l);
        }
        // To a string, then name the function in argument errors.
        if ffi::lua_type(l, -1) != ffi::LUA_TSTRING {
            ffi::lua_pushvalue(l, ffi::lua_upvalueindex(2));
            ffi::lua_insert(l, -2);
            if ffi::lua_pcall(l, 1, 1, 0) != 0 || ffi::lua_type(l, -1) != ffi::LUA_TSTRING {
                ffi::lua_settop(l, 0);
                ffi::lua_pushstring(l, c"invalid argument".as_ptr());
            }
        }
        let mut ar: ffi::lua_Debug = std::mem::zeroed();
        let fname = if ffi::lua_getinfo(l, 0, c"n".as_ptr(), &mut ar) != 0 && !ar.name.is_null() {
            CStr::from_ptr(ar.name).to_string_lossy().into_owned()
        } else {
            String::new()
        };
        let msg = name_arg_error(str_at(l, -1), &fname);
        ffi::lua_pop(l, 1);
        ffi::lua_pushlstring(l, msg.as_ptr() as *const c_char, msg.len());
        drop(msg);
        drop(fname);
        reraise(l, ffi::lua_upvalueindex(2))
    }
}

/// Color3.fromRGB, UDim2.new / fromScale / fromOffset and Vector2.new on
/// plain finite numbers (each missing one 0, as the implementations take
/// them: f32 components, offsets through f32 for new and f64 for
/// fromOffset), pushed as the mlua userdata the implementation makes;
/// None for anything else (UDim2.new with two arguments takes two UDims).
unsafe fn small_ctor_fast(l: *mut ffi::lua_State, id: ffi::lua_Integer, n: c_int) -> Option<c_int> {
    use crate::ltypes::{LColor3, LUDim, LUDim2, LVector2};
    unsafe {
        let max = match id {
            5 => 3,
            6 => 4,
            _ => 2,
        };
        if n > max || (id == 6 && n == 2) || ffi::lua_checkstack(l, 3) == 0 {
            return None;
        }
        let mut a = [0f32; 4];
        let mut d = [0f64; 2];
        for i in 0..n {
            a[i as usize] = num_at(l, i + 1)?;
            if i < 2 {
                d[i as usize] = ffi::lua_tonumber(l, i + 1);
            }
        }
        let lua = Lua::get_or_init_from_ptr(l);
        let pushed = match id {
            5 => lua.push_userdata_unref(l, LColor3([a[0] / 255.0, a[1] / 255.0, a[2] / 255.0])),
            6 => lua.push_userdata_unref(
                l,
                LUDim2 { x: LUDim { scale: a[0], offset: a[1] as i32 }, y: LUDim { scale: a[2], offset: a[3] as i32 } },
            ),
            7 => lua.push_userdata_unref(
                l,
                LUDim2 { x: LUDim { scale: a[0], offset: 0 }, y: LUDim { scale: a[1], offset: 0 } },
            ),
            8 => lua.push_userdata_unref(
                l,
                LUDim2 { x: LUDim { scale: 0.0, offset: d[0] as i32 }, y: LUDim { scale: 0.0, offset: d[1] as i32 } },
            ),
            _ => lua.push_userdata_unref(l, LVector2(glam::Vec2::new(a[0], a[1]))),
        };
        matches!(pushed, Ok(true)).then_some(1)
    }
}

/// `invalid argument #1 (…)` → `invalid argument #1 to 'new' (…)`.
fn name_arg_error(msg: &str, fname: &str) -> String {
    if fname.is_empty() {
        return msg.to_string();
    }
    for kind in ["invalid argument #", "missing argument #"] {
        if let Some(i) = msg.find(kind) {
            let rest = &msg[i + kind.len()..];
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if rest[digits..].starts_with(" (") {
                let at = i + kind.len() + digits;
                return format!("{} to '{fname}'{}", &msg[..at], &msg[at..]);
            }
        }
    }
    msg.to_string()
}

/// Names, wraps and freezes the datatype constructor tables.
pub fn install_constructors(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();
    let errtext = errtext(lua)?;
    for ty in CONSTRUCTORS {
        let Ok(t) = g.raw_get::<mlua::Table>(*ty) else { continue };
        if t.is_readonly() {
            continue;
        }
        let mut fns = Vec::new();
        for pair in t.pairs::<mlua::String, Value>() {
            let (k, v) = pair?;
            if let Value::Function(f) = v {
                fns.push((k, f));
            }
        }
        for (k, f) in fns {
            let key = k.to_string_lossy();
            // 0 strict, 1 lenient, 2 lenient but UDim2.new(UDim, UDim).
            let lenient: i32 = match (lenient(ty, &key), *ty, &*key) {
                (false, ..) => 0,
                (true, "UDim2", "new") => 2,
                (true, ..) => 1,
            };
            let fast = ctor_fast_id(ty, &key);
            let w = unsafe {
                lua.exec_raw::<Function>((f, errtext.clone(), lenient, fast, k.clone()), |l| {
                    // The key string lives as long as the (frozen) table.
                    let name = match ctor_name(ty, &key) {
                        Some(n) => n.as_ptr(),
                        None => ffi::lua_tolstring(l, -1, std::ptr::null_mut()),
                    };
                    ffi::lua_pop(l, 1);
                    ffi::lua_pushcclosurek(l, ctor_fn, name, 4, None);
                })?
            };
            t.raw_set(k, w)?;
        }
        t.set_readonly(true);
    }
    Ok(())
}
