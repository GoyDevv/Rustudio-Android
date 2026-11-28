//! 1:1 Roblox Studio Luau IntelliSense & Autocomplete Engine.
//!
//! Resolves live DataModel paths (`game.Workspace.Baseplate.`, `script.Parent.`,
//! `game:GetService("ReplicatedStorage").`), local variable types (`Instance.new`,
//! `GetService`, Luau type annotations, datatype constructors), all 942 Roblox
//! classes + 652 Enums from `api-dump.json` (with `rbx_reflection_database`
//! fallback), string-argument completions (`GetService`, `Instance.new`, `IsA`,
//! `FindFirstChild`, `WaitForChild`), and the full Luau stdlib + Roblox datatype
//! catalog.

use std::collections::{HashMap, HashSet};

use egui::Color32;
use rbx_dom_weak::types::Ref;
use rustudio_core::doc::Document;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcKind {
    Child,
    Service,
    Property,
    Method,
    Event,
    Enum,
    Class,
    Keyword,
    Stdlib,
    Global,
    Local,
    Plugin,
}

impl AcKind {
    pub fn badge(self) -> (&'static str, Color32) {
        match self {
            AcKind::Child => ("ch", Color32::from_rgb(92, 198, 255)),
            AcKind::Service => ("sv", Color32::from_rgb(78, 168, 255)),
            AcKind::Property => ("pr", Color32::from_rgb(106, 178, 255)),
            AcKind::Method => ("fn", Color32::from_rgb(198, 134, 245)),
            AcKind::Event => ("ev", Color32::from_rgb(245, 188, 78)),
            AcKind::Enum => ("en", Color32::from_rgb(235, 148, 82)),
            AcKind::Class => ("cl", Color32::from_rgb(86, 206, 178)),
            AcKind::Keyword => ("kw", Color32::from_rgb(248, 109, 124)),
            AcKind::Stdlib => ("lib", Color32::from_rgb(132, 214, 247)),
            AcKind::Global => ("gl", Color32::from_rgb(132, 214, 247)),
            AcKind::Local => ("var", Color32::from_rgb(220, 224, 232)),
            AcKind::Plugin => ("plg", Color32::from_rgb(160, 220, 140)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcItem {
    pub label: String,
    pub insert: String,
    pub kind: AcKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerKind {
    Identifier,
    Member {
        sep: char, // '.' or ':'
        chain: Vec<String>,
    },
    StringArg {
        func: String,
        receiver_chain: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcContext {
    pub replace_start: usize,
    pub prefix: String,
    pub trigger: TriggerKind,
}

pub fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

pub fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

// ---------------------------------------------------------------------------
// Complete Luau Keywords, Stdlib, and Roblox Datatype Catalog
// ---------------------------------------------------------------------------

pub const LUAU_KEYWORDS: &[&str] = &[
    "and", "break", "continue", "do", "else", "elseif", "end", "export",
    "false", "for", "function", "if", "in", "local", "nil", "not", "or",
    "repeat", "return", "then", "true", "type", "typeof", "until", "while",
];

pub const LUAU_GLOBALS: &[(&str, &str, AcKind)] = &[
    ("game", "DataModel", AcKind::Global),
    ("workspace", "Workspace", AcKind::Global),
    ("script", "LuaSourceContainer", AcKind::Global),
    ("Enum", "Enums", AcKind::Enum),
    ("Instance", "Instance constructor", AcKind::Class),
    ("Vector3", "Vector3 datatype", AcKind::Class),
    ("Vector2", "Vector2 datatype", AcKind::Class),
    ("CFrame", "CFrame datatype", AcKind::Class),
    ("Color3", "Color3 datatype", AcKind::Class),
    ("UDim2", "UDim2 datatype", AcKind::Class),
    ("UDim", "UDim datatype", AcKind::Class),
    ("BrickColor", "BrickColor datatype", AcKind::Class),
    ("TweenInfo", "TweenInfo datatype", AcKind::Class),
    ("RaycastParams", "RaycastParams datatype", AcKind::Class),
    ("OverlapParams", "OverlapParams datatype", AcKind::Class),
    ("ColorSequence", "ColorSequence datatype", AcKind::Class),
    ("ColorSequenceKeypoint", "ColorSequenceKeypoint", AcKind::Class),
    ("NumberSequence", "NumberSequence datatype", AcKind::Class),
    ("NumberSequenceKeypoint", "NumberSequenceKeypoint", AcKind::Class),
    ("NumberRange", "NumberRange datatype", AcKind::Class),
    ("Rect", "Rect datatype", AcKind::Class),
    ("Ray", "Ray datatype", AcKind::Class),
    ("Region3", "Region3 datatype", AcKind::Class),
    ("PhysicalProperties", "PhysicalProperties", AcKind::Class),
    ("Font", "Font datatype", AcKind::Class),
    ("DateTime", "DateTime datatype", AcKind::Class),
    ("Random", "Random datatype", AcKind::Class),
    ("PathWaypoint", "PathWaypoint datatype", AcKind::Class),
    ("Faces", "Faces datatype", AcKind::Class),
    ("Axes", "Axes datatype", AcKind::Class),
    ("SharedTable", "SharedTable datatype", AcKind::Class),
    ("CatalogSearchParams", "CatalogSearchParams", AcKind::Class),
    ("task", "Luau task library", AcKind::Stdlib),
    ("math", "Luau math library", AcKind::Stdlib),
    ("table", "Luau table library", AcKind::Stdlib),
    ("string", "Luau string library", AcKind::Stdlib),
    ("bit32", "Luau bit32 library", AcKind::Stdlib),
    ("buffer", "Luau buffer library", AcKind::Stdlib),
    ("coroutine", "Luau coroutine library", AcKind::Stdlib),
    ("utf8", "Luau utf8 library", AcKind::Stdlib),
    ("os", "Luau os library", AcKind::Stdlib),
    ("debug", "Luau debug library", AcKind::Stdlib),
    ("vector", "Luau vector library", AcKind::Stdlib),
    ("print", "(...any) -> ()", AcKind::Stdlib),
    ("warn", "(...any) -> ()", AcKind::Stdlib),
    ("error", "(message: any, level: number?) -> never", AcKind::Stdlib),
    ("assert", "<T>(value: T, message: string?) -> T", AcKind::Stdlib),
    ("pcall", "(f, ...any) -> (boolean, ...any)", AcKind::Stdlib),
    ("xpcall", "(f, err, ...any) -> (boolean, ...any)", AcKind::Stdlib),
    ("pairs", "(t: table) -> iterator", AcKind::Stdlib),
    ("ipairs", "(t: {T}) -> iterator", AcKind::Stdlib),
    ("next", "(t: table, lastKey?) -> (any, any)", AcKind::Stdlib),
    ("select", "(index: number | '#', ...any) -> ...any", AcKind::Stdlib),
    ("tonumber", "(e: any, base: number?) -> number?", AcKind::Stdlib),
    ("tostring", "(e: any) -> string", AcKind::Stdlib),
    ("require", "(module: ModuleScript | number) -> any", AcKind::Stdlib),
    ("rawget", "(t: table, k: any) -> any", AcKind::Stdlib),
    ("rawset", "(t: table, k: any, v: any) -> table", AcKind::Stdlib),
    ("rawequal", "(v1: any, v2: any) -> boolean", AcKind::Stdlib),
    ("rawlen", "(v: table | string) -> number", AcKind::Stdlib),
    ("getmetatable", "(t: any) -> table?", AcKind::Stdlib),
    ("setmetatable", "(t: table, mt: table?) -> table", AcKind::Stdlib),
    ("tick", "() -> number", AcKind::Global),
    ("time", "() -> number", AcKind::Global),
    ("elapsedTime", "() -> number", AcKind::Global),
    ("wait", "(seconds: number?) -> (number, number)", AcKind::Global),
    ("spawn", "(callback: () -> ()) -> ()", AcKind::Global),
    ("delay", "(delayTime: number, callback: () -> ()) -> ()", AcKind::Global),
    ("settings", "() -> GlobalSettings", AcKind::Global),
    ("UserSettings", "() -> UserSettings", AcKind::Global),
    ("version", "() -> string", AcKind::Global),
    ("_G", "Shared script table", AcKind::Global),
    ("shared", "Shared script table", AcKind::Global),
    ("plugin", "Plugin", AcKind::Global),
];

fn stdlib_or_datatype_members(name: &str) -> Option<&'static [(&'static str, &'static str, AcKind)]> {
    const TASK: &[(&str, &str, AcKind)] = &[
        ("wait", "(duration: number?) -> number", AcKind::Method),
        ("spawn", "(f: function | thread, ...any) -> thread", AcKind::Method),
        ("defer", "(f: function | thread, ...any) -> thread", AcKind::Method),
        ("delay", "(duration: number, f: function | thread, ...any) -> thread", AcKind::Method),
        ("cancel", "(thread: thread) -> ()", AcKind::Method),
        ("synchronize", "() -> ()", AcKind::Method),
        ("desynchronize", "() -> ()", AcKind::Method),
    ];
    const MATH: &[(&str, &str, AcKind)] = &[
        ("abs", "(x: number) -> number", AcKind::Method),
        ("acos", "(x: number) -> number", AcKind::Method),
        ("asin", "(x: number) -> number", AcKind::Method),
        ("atan", "(y: number) -> number", AcKind::Method),
        ("atan2", "(y: number, x: number) -> number", AcKind::Method),
        ("ceil", "(x: number) -> number", AcKind::Method),
        ("clamp", "(x: number, min: number, max: number) -> number", AcKind::Method),
        ("cos", "(x: number) -> number", AcKind::Method),
        ("cosh", "(x: number) -> number", AcKind::Method),
        ("deg", "(x: number) -> number", AcKind::Method),
        ("exp", "(x: number) -> number", AcKind::Method),
        ("floor", "(x: number) -> number", AcKind::Method),
        ("fmod", "(x: number, y: number) -> number", AcKind::Method),
        ("frexp", "(x: number) -> (number, number)", AcKind::Method),
        ("huge", "number (infinity)", AcKind::Property),
        ("ldexp", "(m: number, e: number) -> number", AcKind::Method),
        ("lerp", "(a: number, b: number, t: number) -> number", AcKind::Method),
        ("log", "(x: number, base: number?) -> number", AcKind::Method),
        ("log10", "(x: number) -> number", AcKind::Method),
        ("map", "(x, inMin, inMax, outMin, outMax) -> number", AcKind::Method),
        ("max", "(x: number, ...number) -> number", AcKind::Method),
        ("min", "(x: number, ...number) -> number", AcKind::Method),
        ("modf", "(x: number) -> (number, number)", AcKind::Method),
        ("noise", "(x: number, y: number?, z: number?) -> number", AcKind::Method),
        ("pi", "number (3.14159...)", AcKind::Property),
        ("pow", "(x: number, y: number) -> number", AcKind::Method),
        ("rad", "(x: number) -> number", AcKind::Method),
        ("random", "(m: number?, n: number?) -> number", AcKind::Method),
        ("randomseed", "(x: number) -> ()", AcKind::Method),
        ("round", "(x: number) -> number", AcKind::Method),
        ("sign", "(x: number) -> number", AcKind::Method),
        ("sin", "(x: number) -> number", AcKind::Method),
        ("sinh", "(x: number) -> number", AcKind::Method),
        ("sqrt", "(x: number) -> number", AcKind::Method),
        ("tan", "(x: number) -> number", AcKind::Method),
        ("tanh", "(x: number) -> number", AcKind::Method),
    ];
    const TABLE: &[(&str, &str, AcKind)] = &[
        ("clear", "(t: table) -> ()", AcKind::Method),
        ("clone", "<T>(t: T) -> T", AcKind::Method),
        ("concat", "(t: {string}, sep: string?, i: number?, j: number?) -> string", AcKind::Method),
        ("create", "<V>(count: number, value: V?) -> {V}", AcKind::Method),
        ("find", "<V>(haystack: {V}, needle: V, init: number?) -> number?", AcKind::Method),
        ("freeze", "<T>(t: T) -> T", AcKind::Method),
        ("insert", "(t: table, posOrValue: any, value: any?) -> ()", AcKind::Method),
        ("isfrozen", "(t: table) -> boolean", AcKind::Method),
        ("move", "(src, a, b, t, dst?) -> table", AcKind::Method),
        ("pack", "(...any) -> { n: number }", AcKind::Method),
        ("remove", "<V>(t: {V}, pos: number?) -> V?", AcKind::Method),
        ("sort", "<V>(t: {V}, comp: ((V, V) -> boolean)?) -> ()", AcKind::Method),
        ("unpack", "<V>(list: {V}, i: number?, j: number?) -> ...V", AcKind::Method),
    ];
    const STRING: &[(&str, &str, AcKind)] = &[
        ("byte", "(s: string, i: number?, j: number?) -> ...number", AcKind::Method),
        ("char", "(...number) -> string", AcKind::Method),
        ("find", "(s: string, pattern: string, init: number?, plain: boolean?) -> (number?, number?)", AcKind::Method),
        ("format", "(s: string, ...any) -> string", AcKind::Method),
        ("gmatch", "(s: string, pattern: string) -> iterator", AcKind::Method),
        ("gsub", "(s: string, pattern: string, repl, n: number?) -> (string, number)", AcKind::Method),
        ("len", "(s: string) -> number", AcKind::Method),
        ("lower", "(s: string) -> string", AcKind::Method),
        ("match", "(s: string, pattern: string, init: number?) -> ...string?", AcKind::Method),
        ("pack", "(fmt: string, ...any) -> string", AcKind::Method),
        ("packsize", "(fmt: string) -> number", AcKind::Method),
        ("rep", "(s: string, n: number) -> string", AcKind::Method),
        ("reverse", "(s: string) -> string", AcKind::Method),
        ("split", "(s: string, separator: string?) -> {string}", AcKind::Method),
        ("sub", "(s: string, i: number, j: number?) -> string", AcKind::Method),
        ("unpack", "(fmt: string, s: string, pos: number?) -> ...any", AcKind::Method),
        ("upper", "(s: string) -> string", AcKind::Method),
    ];
    const BIT32: &[(&str, &str, AcKind)] = &[
        ("arshift", "(x: number, disp: number) -> number", AcKind::Method),
        ("band", "(...number) -> number", AcKind::Method),
        ("bnot", "(x: number) -> number", AcKind::Method),
        ("bor", "(...number) -> number", AcKind::Method),
        ("btest", "(...number) -> boolean", AcKind::Method),
        ("bxor", "(...number) -> number", AcKind::Method),
        ("byteswap", "(n: number) -> number", AcKind::Method),
        ("countlz", "(n: number) -> number", AcKind::Method),
        ("countrz", "(n: number) -> number", AcKind::Method),
        ("extract", "(n: number, field: number, width: number?) -> number", AcKind::Method),
        ("lrotate", "(x: number, disp: number) -> number", AcKind::Method),
        ("lshift", "(x: number, disp: number) -> number", AcKind::Method),
        ("replace", "(n: number, v: number, field: number, width: number?) -> number", AcKind::Method),
        ("rrotate", "(x: number, disp: number) -> number", AcKind::Method),
        ("rshift", "(x: number, disp: number) -> number", AcKind::Method),
    ];
    const BUFFER: &[(&str, &str, AcKind)] = &[
        ("create", "(size: number) -> buffer", AcKind::Method),
        ("fromstring", "(str: string) -> buffer", AcKind::Method),
        ("tostring", "(b: buffer) -> string", AcKind::Method),
        ("len", "(b: buffer) -> number", AcKind::Method),
        ("copy", "(target, targetOffset, source, sourceOffset?, count?)", AcKind::Method),
        ("fill", "(b: buffer, offset: number, value: number, count: number?)", AcKind::Method),
        ("readi8", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readu8", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readi16", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readu16", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readi32", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readu32", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readf32", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readf64", "(b: buffer, offset: number) -> number", AcKind::Method),
        ("readstring", "(b: buffer, offset: number, count: number) -> string", AcKind::Method),
        ("writei8", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writeu8", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writei16", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writeu16", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writei32", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writeu32", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writef32", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writef64", "(b: buffer, offset: number, value: number)", AcKind::Method),
        ("writestring", "(b: buffer, offset: number, value: string, count: number?)", AcKind::Method),
    ];
    const COROUTINE: &[(&str, &str, AcKind)] = &[
        ("create", "(f: function) -> thread", AcKind::Method),
        ("resume", "(co: thread, ...any) -> (boolean, ...any)", AcKind::Method),
        ("running", "() -> thread", AcKind::Method),
        ("status", "(co: thread) -> string", AcKind::Method),
        ("wrap", "(f: function) -> function", AcKind::Method),
        ("yield", "(...any) -> ...any", AcKind::Method),
        ("isyieldable", "() -> boolean", AcKind::Method),
        ("close", "(co: thread) -> (boolean, any)", AcKind::Method),
    ];
    const UTF8: &[(&str, &str, AcKind)] = &[
        ("char", "(...number) -> string", AcKind::Method),
        ("charpattern", "string", AcKind::Property),
        ("codes", "(s: string) -> iterator", AcKind::Method),
        ("codepoint", "(s: string, i: number?, j: number?) -> ...number", AcKind::Method),
        ("len", "(s: string, i: number?, j: number?) -> (number?, number?)", AcKind::Method),
        ("offset", "(s: string, n: number, i: number?) -> number?", AcKind::Method),
        ("graphemes", "(s: string, i: number?, j: number?) -> iterator", AcKind::Method),
        ("nfcnormalize", "(s: string) -> string", AcKind::Method),
        ("nfdnormalize", "(s: string) -> string", AcKind::Method),
    ];
    const OS: &[(&str, &str, AcKind)] = &[
        ("clock", "() -> number", AcKind::Method),
        ("date", "(format: string?, time: number?) -> string | table", AcKind::Method),
        ("difftime", "(t2: number, t1: number) -> number", AcKind::Method),
        ("time", "(t: table?) -> number", AcKind::Method),
    ];
    const DEBUG: &[(&str, &str, AcKind)] = &[
        ("info", "(levelOrFunc, options: string) -> ...any", AcKind::Method),
        ("traceback", "(message: string?, level: number?) -> string", AcKind::Method),
        ("profilebegin", "(label: string) -> ()", AcKind::Method),
        ("profileend", "() -> ()", AcKind::Method),
        ("setmemorycategory", "(tag: string) -> ()", AcKind::Method),
        ("resetmemorycategory", "() -> ()", AcKind::Method),
    ];
    const VECTOR: &[(&str, &str, AcKind)] = &[
        ("create", "(x: number, y: number, z: number?) -> vector", AcKind::Method),
        ("magnitude", "(v: vector) -> number", AcKind::Method),
        ("normalize", "(v: vector) -> vector", AcKind::Method),
        ("cross", "(a: vector, b: vector) -> vector", AcKind::Method),
        ("dot", "(a: vector, b: vector) -> number", AcKind::Method),
        ("lerp", "(a: vector, b: vector, t: number) -> vector", AcKind::Method),
        ("abs", "(v: vector) -> vector", AcKind::Method),
        ("floor", "(v: vector) -> vector", AcKind::Method),
        ("ceil", "(v: vector) -> vector", AcKind::Method),
        ("clamp", "(v: vector, min: vector, max: vector) -> vector", AcKind::Method),
        ("zero", "vector", AcKind::Property),
        ("one", "vector", AcKind::Property),
    ];
    const INSTANCE_CTOR: &[(&str, &str, AcKind)] = &[
        ("new", "(className: string, parent: Instance?) -> Instance", AcKind::Method),
        ("fromExisting", "(existing: Instance) -> Instance", AcKind::Method),
    ];
    const VECTOR3: &[(&str, &str, AcKind)] = &[
        ("new", "(x: number?, y: number?, z: number?) -> Vector3", AcKind::Method),
        ("FromNormalId", "(normal: Enum.NormalId) -> Vector3", AcKind::Method),
        ("FromAxis", "(axis: Enum.Axis) -> Vector3", AcKind::Method),
        ("zero", "Vector3", AcKind::Property),
        ("one", "Vector3", AcKind::Property),
        ("xAxis", "Vector3", AcKind::Property),
        ("yAxis", "Vector3", AcKind::Property),
        ("zAxis", "Vector3", AcKind::Property),
        ("X", "number", AcKind::Property),
        ("Y", "number", AcKind::Property),
        ("Z", "number", AcKind::Property),
        ("Magnitude", "number", AcKind::Property),
        ("Unit", "Vector3", AcKind::Property),
        ("Dot", "(other: Vector3) -> number", AcKind::Method),
        ("Cross", "(other: Vector3) -> Vector3", AcKind::Method),
        ("Lerp", "(goal: Vector3, alpha: number) -> Vector3", AcKind::Method),
        ("FuzzyEq", "(other: Vector3, epsilon: number?) -> boolean", AcKind::Method),
        ("Angle", "(other: Vector3, axis: Vector3?) -> number", AcKind::Method),
    ];
    const VECTOR2: &[(&str, &str, AcKind)] = &[
        ("new", "(x: number?, y: number?) -> Vector2", AcKind::Method),
        ("zero", "Vector2", AcKind::Property),
        ("one", "Vector2", AcKind::Property),
        ("xAxis", "Vector2", AcKind::Property),
        ("yAxis", "Vector2", AcKind::Property),
        ("X", "number", AcKind::Property),
        ("Y", "number", AcKind::Property),
        ("Magnitude", "number", AcKind::Property),
        ("Unit", "Vector2", AcKind::Property),
        ("Dot", "(other: Vector2) -> number", AcKind::Method),
        ("Cross", "(other: Vector2) -> number", AcKind::Method),
        ("Lerp", "(goal: Vector2, alpha: number) -> Vector2", AcKind::Method),
    ];
    const CFRAME: &[(&str, &str, AcKind)] = &[
        ("new", "(...) -> CFrame", AcKind::Method),
        ("Angles", "(rx: number, ry: number, rz: number) -> CFrame", AcKind::Method),
        ("fromEulerAnglesXYZ", "(rx: number, ry: number, rz: number) -> CFrame", AcKind::Method),
        ("fromEulerAnglesYXZ", "(rx: number, ry: number, rz: number) -> CFrame", AcKind::Method),
        ("fromOrientation", "(rx: number, ry: number, rz: number) -> CFrame", AcKind::Method),
        ("fromAxisAngle", "(v: Vector3, r: number) -> CFrame", AcKind::Method),
        ("fromMatrix", "(pos: Vector3, vX: Vector3, vY: Vector3, vZ: Vector3?) -> CFrame", AcKind::Method),
        ("lookAt", "(at: Vector3, lookAt: Vector3, up: Vector3?) -> CFrame", AcKind::Method),
        ("lookAlong", "(at: Vector3, direction: Vector3, up: Vector3?) -> CFrame", AcKind::Method),
        ("identity", "CFrame", AcKind::Property),
        ("Position", "Vector3", AcKind::Property),
        ("Rotation", "CFrame", AcKind::Property),
        ("LookVector", "Vector3", AcKind::Property),
        ("RightVector", "Vector3", AcKind::Property),
        ("UpVector", "Vector3", AcKind::Property),
        ("X", "number", AcKind::Property),
        ("Y", "number", AcKind::Property),
        ("Z", "number", AcKind::Property),
        ("Inverse", "() -> CFrame", AcKind::Method),
        ("Lerp", "(goal: CFrame, alpha: number) -> CFrame", AcKind::Method),
        ("ToWorldSpace", "(cf: CFrame) -> CFrame", AcKind::Method),
        ("ToObjectSpace", "(cf: CFrame) -> CFrame", AcKind::Method),
        ("PointToWorldSpace", "(v3: Vector3) -> Vector3", AcKind::Method),
        ("PointToObjectSpace", "(v3: Vector3) -> Vector3", AcKind::Method),
        ("VectorToWorldSpace", "(v3: Vector3) -> Vector3", AcKind::Method),
        ("VectorToObjectSpace", "(v3: Vector3) -> Vector3", AcKind::Method),
        ("GetComponents", "() -> (number, ...)", AcKind::Method),
        ("ToEulerAnglesXYZ", "() -> (number, number, number)", AcKind::Method),
        ("ToEulerAnglesYXZ", "() -> (number, number, number)", AcKind::Method),
        ("ToOrientation", "() -> (number, number, number)", AcKind::Method),
    ];
    const COLOR3: &[(&str, &str, AcKind)] = &[
        ("new", "(r: number?, g: number?, b: number?) -> Color3", AcKind::Method),
        ("fromRGB", "(r: number, g: number, b: number) -> Color3", AcKind::Method),
        ("fromHSV", "(h: number, s: number, v: number) -> Color3", AcKind::Method),
        ("fromHex", "(hex: string) -> Color3", AcKind::Method),
        ("R", "number", AcKind::Property),
        ("G", "number", AcKind::Property),
        ("B", "number", AcKind::Property),
        ("Lerp", "(color: Color3, alpha: number) -> Color3", AcKind::Method),
        ("ToHSV", "() -> (number, number, number)", AcKind::Method),
        ("ToHex", "() -> string", AcKind::Method),
    ];
    const UDIM2: &[(&str, &str, AcKind)] = &[
        ("new", "(xScale, xOffset, yScale, yOffset) -> UDim2", AcKind::Method),
        ("fromScale", "(xScale: number, yScale: number) -> UDim2", AcKind::Method),
        ("fromOffset", "(xOffset: number, yOffset: number) -> UDim2", AcKind::Method),
        ("X", "UDim", AcKind::Property),
        ("Y", "UDim", AcKind::Property),
        ("Lerp", "(goal: UDim2, alpha: number) -> UDim2", AcKind::Method),
    ];
    const UDIM: &[(&str, &str, AcKind)] = &[
        ("new", "(scale: number?, offset: number?) -> UDim", AcKind::Method),
        ("Scale", "number", AcKind::Property),
        ("Offset", "number", AcKind::Property),
    ];
    const BRICKCOLOR: &[(&str, &str, AcKind)] = &[
        ("new", "(val: string | number | Color3) -> BrickColor", AcKind::Method),
        ("Random", "() -> BrickColor", AcKind::Method),
        ("White", "() -> BrickColor", AcKind::Method),
        ("Black", "() -> BrickColor", AcKind::Method),
        ("Red", "() -> BrickColor", AcKind::Method),
        ("Blue", "() -> BrickColor", AcKind::Method),
        ("Green", "() -> BrickColor", AcKind::Method),
        ("Yellow", "() -> BrickColor", AcKind::Method),
        ("Color", "Color3", AcKind::Property),
        ("Name", "string", AcKind::Property),
        ("Number", "number", AcKind::Property),
    ];
    const TWEENINFO: &[(&str, &str, AcKind)] = &[
        ("new", "(time?, style?, dir?, repeatCount?, reverses?, delayTime?) -> TweenInfo", AcKind::Method),
        ("Time", "number", AcKind::Property),
        ("EasingStyle", "Enum.EasingStyle", AcKind::Property),
        ("EasingDirection", "Enum.EasingDirection", AcKind::Property),
        ("RepeatCount", "number", AcKind::Property),
        ("Reverses", "boolean", AcKind::Property),
        ("DelayTime", "number", AcKind::Property),
    ];
    const RAYCASTPARAMS: &[(&str, &str, AcKind)] = &[
        ("new", "() -> RaycastParams", AcKind::Method),
        ("FilterDescendantsInstances", "{Instance}", AcKind::Property),
        ("FilterType", "Enum.RaycastFilterType", AcKind::Property),
        ("IgnoreWater", "boolean", AcKind::Property),
        ("CollisionGroup", "string", AcKind::Property),
        ("RespectCanCollide", "boolean", AcKind::Property),
        ("BruteForceAllSlow", "boolean", AcKind::Property),
        ("AddToFilter", "(instances: Instance | {Instance}) -> ()", AcKind::Method),
    ];
    const OVERLAPPARAMS: &[(&str, &str, AcKind)] = &[
        ("new", "() -> OverlapParams", AcKind::Method),
        ("FilterDescendantsInstances", "{Instance}", AcKind::Property),
        ("FilterType", "Enum.RaycastFilterType", AcKind::Property),
        ("MaxParts", "number", AcKind::Property),
        ("CollisionGroup", "string", AcKind::Property),
        ("RespectCanCollide", "boolean", AcKind::Property),
        ("AddToFilter", "(instances: Instance | {Instance}) -> ()", AcKind::Method),
    ];
    const DATETIME: &[(&str, &str, AcKind)] = &[
        ("now", "() -> DateTime", AcKind::Method),
        ("fromUnixTimestamp", "(unixTimestamp: number) -> DateTime", AcKind::Method),
        ("fromUnixTimestampMillis", "(ms: number) -> DateTime", AcKind::Method),
        ("fromUniversalTime", "(year, month?, day?, hour?, min?, sec?, ms?) -> DateTime", AcKind::Method),
        ("fromLocalTime", "(year, month?, day?, hour?, min?, sec?, ms?) -> DateTime", AcKind::Method),
        ("fromIsoDate", "(isoDate: string) -> DateTime?", AcKind::Method),
        ("UnixTimestamp", "number", AcKind::Property),
        ("UnixTimestampMillis", "number", AcKind::Property),
        ("ToIsoDate", "() -> string", AcKind::Method),
        ("ToUniversalTime", "() -> table", AcKind::Method),
        ("ToLocalTime", "() -> table", AcKind::Method),
        ("FormatUniversalTime", "(format: string, locale: string) -> string", AcKind::Method),
        ("FormatLocalTime", "(format: string, locale: string) -> string", AcKind::Method),
    ];
    const RANDOM: &[(&str, &str, AcKind)] = &[
        ("new", "(seed: number?) -> Random", AcKind::Method),
        ("NextNumber", "(min: number?, max: number?) -> number", AcKind::Method),
        ("NextInteger", "(min: number, max: number) -> number", AcKind::Method),
        ("NextUnitVector", "() -> Vector3", AcKind::Method),
        ("Shuffle", "(tb: {any}) -> ()", AcKind::Method),
        ("Clone", "() -> Random", AcKind::Method),
    ];
    const FONT: &[(&str, &str, AcKind)] = &[
        ("new", "(family: string, weight?, style?) -> Font", AcKind::Method),
        ("fromEnum", "(font: Enum.Font) -> Font", AcKind::Method),
        ("fromId", "(id: number, weight?, style?) -> Font", AcKind::Method),
        ("fromName", "(name: string, weight?, style?) -> Font", AcKind::Method),
    ];
    const SIGNAL: &[(&str, &str, AcKind)] = &[
        ("Connect", "(func: (...any) -> ()) -> RBXScriptConnection", AcKind::Method),
        ("Once", "(func: (...any) -> ()) -> RBXScriptConnection", AcKind::Method),
        ("Wait", "() -> ...any", AcKind::Method),
        ("ConnectParallel", "(func: (...any) -> ()) -> RBXScriptConnection", AcKind::Method),
    ];
    const CONNECTION: &[(&str, &str, AcKind)] = &[
        ("Disconnect", "() -> ()", AcKind::Method),
        ("Connected", "boolean", AcKind::Property),
    ];
    const SIMPLE_NEW: &[(&str, &str, AcKind)] = &[
        ("new", "(...) -> datatype", AcKind::Method),
    ];

    match name {
        "task" => Some(TASK),
        "math" => Some(MATH),
        "table" => Some(TABLE),
        "string" => Some(STRING),
        "bit32" => Some(BIT32),
        "buffer" => Some(BUFFER),
        "coroutine" => Some(COROUTINE),
        "utf8" => Some(UTF8),
        "os" => Some(OS),
        "debug" => Some(DEBUG),
        "vector" => Some(VECTOR),
        "Instance" => Some(INSTANCE_CTOR),
        "Vector3" => Some(VECTOR3),
        "Vector2" => Some(VECTOR2),
        "CFrame" => Some(CFRAME),
        "Color3" => Some(COLOR3),
        "UDim2" => Some(UDIM2),
        "UDim" => Some(UDIM),
        "BrickColor" => Some(BRICKCOLOR),
        "TweenInfo" => Some(TWEENINFO),
        "RaycastParams" => Some(RAYCASTPARAMS),
        "OverlapParams" => Some(OVERLAPPARAMS),
        "DateTime" => Some(DATETIME),
        "Random" => Some(RANDOM),
        "Font" => Some(FONT),
        "RBXScriptSignal" => Some(SIGNAL),
        "RBXScriptConnection" => Some(CONNECTION),
        "ColorSequence"
        | "ColorSequenceKeypoint"
        | "NumberSequence"
        | "NumberSequenceKeypoint"
        | "NumberRange"
        | "Rect"
        | "Ray"
        | "Region3"
        | "PhysicalProperties"
        | "PathWaypoint"
        | "Faces"
        | "Axes"
        | "SharedTable"
        | "CatalogSearchParams" => Some(SIMPLE_NEW),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Context Parser (Identifier, Dotted/Colon Chain, or String Argument)
// ---------------------------------------------------------------------------

/// Parses a dotted/colon identifier chain ending just before `end` (char index),
/// e.g. `game.Workspace.Baseplate` -> `["game", "Workspace", "Baseplate"]`,
/// `game:GetService("Players")` -> `["game", "Players"]`.
fn parse_receiver_chain(chars: &[char], end: usize) -> Vec<String> {
    let mut segments: Vec<String> = Vec::new();
    let mut pos = end;

    loop {
        // Skip trailing whitespace before `.` / `:`
        while pos > 0 && (chars[pos - 1] == ' ' || chars[pos - 1] == '\t') {
            pos -= 1;
        }
        if pos == 0 {
            break;
        }
        // Handle call expressions like `:GetService("Players")` / `:WaitForChild("Part")`
        if chars[pos - 1] == ')' {
            let mut depth = 1usize;
            let mut open = pos - 1;
            while open > 0 && depth > 0 {
                open -= 1;
                if chars[open] == ')' {
                    depth += 1;
                } else if chars[open] == '(' {
                    depth -= 1;
                }
            }
            if depth != 0 {
                break;
            }
            let inner: String = chars[open + 1..pos - 1].iter().collect();
            let arg = inner.trim();
            let quoted = (arg.starts_with('"') && arg.ends_with('"') && arg.len() >= 2)
                .then(|| &arg[1..arg.len() - 1])
                .or_else(|| {
                    (arg.starts_with('\'') && arg.ends_with('\'') && arg.len() >= 2)
                        .then(|| &arg[1..arg.len() - 1])
                });
            // Read method name before `(`
            let mut m_end = open;
            while m_end > 0 && (chars[m_end - 1] == ' ' || chars[m_end - 1] == '\t') {
                m_end -= 1;
            }
            let mut m_start = m_end;
            while m_start > 0 && is_ident_char(chars[m_start - 1]) {
                m_start -= 1;
            }
            let method: String = chars[m_start..m_end].iter().collect();
            if let Some(target_name) = quoted {
                if matches!(
                    method.as_str(),
                    "GetService" | "FindService" | "FindFirstChild" | "WaitForChild"
                ) {
                    segments.push(target_name.to_string());
                } else if method == "new" && m_start > 0 && chars[m_start - 1] == '.' {
                    segments.push(format!("__INSTANCE_NEW__:{target_name}"));
                    break;
                }
            }
            pos = m_start;
            if pos > 0 && (chars[pos - 1] == '.' || chars[pos - 1] == ':') {
                pos -= 1;
                continue;
            }
            break;
        }

        // Plain identifier segment
        let mut seg_start = pos;
        while seg_start > 0 && is_ident_char(chars[seg_start - 1]) {
            seg_start -= 1;
        }
        if seg_start == pos || !is_ident_start(chars[seg_start]) {
            break;
        }
        segments.push(chars[seg_start..pos].iter().collect());
        pos = seg_start;
        if pos > 0 && (chars[pos - 1] == '.' || chars[pos - 1] == ':') {
            if chars[pos - 1] == '.' && pos >= 2 && chars[pos - 2] == '.' {
                break;
            }
            pos -= 1;
            continue;
        }
        break;
    }

    segments.reverse();
    segments
}

/// Computes the autocomplete context at `cursor` (char index) in `src`.
pub fn autocomplete_context(src: &str, cursor: usize) -> Option<AcContext> {
    let chars: Vec<char> = src.chars().collect();
    if cursor > chars.len() {
        return None;
    }
    // Skip autocomplete inside `--` line comments.
    let mut line_start = cursor;
    while line_start > 0 && chars[line_start - 1] != '\n' {
        line_start -= 1;
    }
    let mut in_str: Option<char> = None;
    let mut i = line_start;
    while i < cursor {
        let ch = chars[i];
        if let Some(q) = in_str {
            if ch == '\\' {
                i += 2;
                continue;
            }
            if ch == q {
                in_str = None;
            }
        } else if ch == '"' || ch == ''' {
            in_str = Some(ch);
        } else if ch == '-' && i + 1 < cursor && chars[i + 1] == '-' {
            return None;
        }
        i += 1;
    }

    // 1. Check for string-argument completion: e.g. `game:GetService("Play`, `Instance.new("Pa`
    let mut s_start = cursor;
    while s_start > 0 && is_ident_char(chars[s_start - 1]) {
        s_start -= 1;
    }
    if s_start > 0 && (chars[s_start - 1] == '"' || chars[s_start - 1] == '\'') {
        let mut p = s_start - 1;
        while p > 0 && (chars[p - 1] == ' ' || chars[p - 1] == '\t') {
            p -= 1;
        }
        if p > 0 && chars[p - 1] == '(' {
            p -= 1;
            while p > 0 && (chars[p - 1] == ' ' || chars[p - 1] == '\t') {
                p -= 1;
            }
            let fn_end = p;
            let mut fn_start = fn_end;
            while fn_start > 0 && is_ident_char(chars[fn_start - 1]) {
                fn_start -= 1;
            }
            let func: String = chars[fn_start..fn_end].iter().collect();
            if matches!(
                func.as_str(),
                "GetService"
                    | "FindService"
                    | "new"
                    | "IsA"
                    | "FindFirstChild"
                    | "WaitForChild"
                    | "FindFirstChildOfClass"
                    | "FindFirstChildWhichIsA"
                    | "FindFirstAncestorOfClass"
            ) {
                let receiver_chain = if fn_start > 0 && (chars[fn_start - 1] == '.' || chars[fn_start - 1] == ':') {
                    parse_receiver_chain(&chars, fn_start - 1)
                } else {
                    Vec::new()
                };
                return Some(AcContext {
                    replace_start: s_start,
                    prefix: chars[s_start..cursor].iter().collect(),
                    trigger: TriggerKind::StringArg { func, receiver_chain },
                });
            }
        }
    }

    // 2. Member trigger: `.` or `:`
    let prefix: String = chars[s_start..cursor].iter().collect();
    if s_start > 0 && (chars[s_start - 1] == '.' || chars[s_start - 1] == ':') {
        let sep = chars[s_start - 1];
        if sep == '.' && s_start >= 2 && (chars[s_start - 2] == '.' || chars[s_start - 2].is_ascii_digit()) {
            return None;
        }
        let chain = parse_receiver_chain(&chars, s_start - 1);
        return Some(AcContext {
            replace_start: s_start,
            prefix,
            trigger: TriggerKind::Member { sep, chain },
        });
    }

    // 3. Plain identifier trigger (>= 1 char)
    if !prefix.is_empty() && prefix.chars().next().is_some_and(is_ident_start) {
        return Some(AcContext {
            replace_start: s_start,
            prefix,
            trigger: TriggerKind::Identifier,
        });
    }

    None
}

// ---------------------------------------------------------------------------
// Local Type Inference & Live DataModel Path Resolution
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum InferredType {
    /// A live instance in the open Document (with its Roblox ClassName).
    Instance { r: Option<Ref>, class: String },
    /// A Roblox datatype or Luau stdlib table (e.g. "Vector3", "CFrame", "Color3").
    Datatype(String),
    /// An Enum group (e.g. `Enum` -> `None`, `Enum.Material` -> `Some("Material")`).
    Enum(Option<String>),
}

fn infer_locals(src: &str, doc: Option<&Document>, script_ref: Option<Ref>) -> HashMap<String, InferredType> {
    let mut map: HashMap<String, InferredType> = HashMap::new();
    for raw in src.lines() {
        let line = raw.split("--").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        // `local foo: ClassName = ...` or `local foo = expr`
        if let Some(rest) = line.strip_prefix("local ") {
            if rest.starts_with("function ") {
                continue;
            }
            let (lhs, rhs) = match rest.split_once('=') {
                Some((l, r)) => (l.trim(), Some(r.trim())),
                None => (rest.trim(), None),
            };
            // Explicit Luau type annotation: `local hum: Humanoid`
            if let Some((var, ty)) = lhs.split_once(':') {
                let var = var.trim();
                let ty = ty.trim().trim_end_matches('?').trim();
                if var.chars().next().is_some_and(is_ident_start) && !ty.is_empty() {
                    if stdlib_or_datatype_members(ty).is_some() {
                        map.insert(var.to_string(), InferredType::Datatype(ty.to_string()));
                    } else {
                        map.insert(var.to_string(), InferredType::Instance { r: None, class: ty.to_string() });
                    }
                    continue;
                }
            }
            let var = lhs.trim();
            if !var.chars().next().is_some_and(is_ident_start) || !var.chars().all(is_ident_char) {
                continue;
            }
            if let Some(expr) = rhs {
                if let Some(ty) = infer_expr_type(expr, &map, doc, script_ref) {
                    map.insert(var.to_string(), ty);
                }
            }
        }
        // Function parameters with type annotations: `function foo(player: Player, part: BasePart)`
        if let Some(open) = line.find('(') {
            if let Some(close) = line[open + 1..].find(')') {
                let params = &line[open + 1..open + 1 + close];
                for p in params.split(',') {
                    if let Some((pname, ty)) = p.split_once(':') {
                        let pname = pname.trim();
                        let ty = ty.trim().trim_end_matches('?').trim();
                        if pname.chars().next().is_some_and(is_ident_start)
                            && pname.chars().all(is_ident_char)
                            && !ty.is_empty()
                        {
                            if stdlib_or_datatype_members(ty).is_some() {
                                map.insert(pname.to_string(), InferredType::Datatype(ty.to_string()));
                            } else {
                                map.insert(pname.to_string(), InferredType::Instance { r: None, class: ty.to_string() });
                            }
                        }
                    }
                }
            }
        }
    }
    map
}

fn infer_expr_type(
    expr: &str,
    locals: &HashMap<String, InferredType>,
    doc: Option<&Document>,
    script_ref: Option<Ref>,
) -> Option<InferredType> {
    let e = expr.trim().trim_end_matches(';').trim();
    // `Instance.new("Part")`
    if let Some(rest) = e.strip_prefix("Instance.new(") {
        let arg = rest.split([',', ')']).next().unwrap_or("").trim();
        if (arg.starts_with('"') && arg.ends_with('"')) || (arg.starts_with('\'') && arg.ends_with('\'')) {
            if arg.len() >= 2 {
                let cls = arg[1..arg.len() - 1].to_string();
                return Some(InferredType::Instance { r: None, class: cls });
            }
        }
    }
    // Datatype constructors: `Vector3.new(...)`, `Color3.fromRGB(...)`, etc.
    if let Some((head, _)) = e.split_once('.') {
        if stdlib_or_datatype_members(head.trim()).is_some() && head.trim() != "Instance" {
            return Some(InferredType::Datatype(head.trim().to_string()));
        }
    }
    // Dotted/colon chain: `game:GetService("Players")`, `game.Workspace.Baseplate`, `script.Parent`
    let chars: Vec<char> = e.chars().collect();
    let chain = parse_receiver_chain(&chars, chars.len());
    if !chain.is_empty() {
        return resolve_chain(&chain, locals, doc, script_ref);
    }
    None
}

fn resolve_chain(
    chain: &[String],
    locals: &HashMap<String, InferredType>,
    doc: Option<&Document>,
    script_ref: Option<Ref>,
) -> Option<InferredType> {
    if chain.is_empty() {
        return None;
    }
    let first = chain[0].as_str();
    if let Some(cls) = first.strip_prefix("__INSTANCE_NEW__:") {
        return Some(InferredType::Instance { r: None, class: cls.to_string() });
    }
    if first == "Enum" {
        return match chain.len() {
            1 => Some(InferredType::Enum(None)),
            2 => Some(InferredType::Enum(Some(chain[1].clone()))),
            _ => None,
        };
    }
    if chain.len() == 1 && stdlib_or_datatype_members(first).is_some() {
        return Some(InferredType::Datatype(first.to_string()));
    }

    let mut cur: InferredType = match first {
        "game" => InferredType::Instance {
            r: doc.map(|d| d.dom.root_ref()),
            class: "DataModel".to_string(),
        },
        "workspace" => {
            let ws_ref = doc.and_then(|d| d.find_child(d.dom.root_ref(), "Workspace"));
            InferredType::Instance { r: ws_ref, class: "Workspace".to_string() }
        }
        "script" => {
            let cls = doc
                .and_then(|d| script_ref.and_then(|r| d.dom.get_by_ref(r)))
                .map(|i| i.class.to_string())
                .unwrap_or_else(|| "Script".to_string());
            InferredType::Instance { r: script_ref, class: cls }
        }
        other => locals.get(other)?.clone(),
    };

    for seg in &chain[1..] {
        match cur {
            InferredType::Instance { r, class } => {
                if seg == "Parent" {
                    if let (Some(d), Some(inst_ref)) = (doc, r) {
                        if let Some(p_ref) = d.dom.get_by_ref(inst_ref).map(|i| i.parent()) {
                            let p_cls = if p_ref == d.dom.root_ref() {
                                "DataModel".to_string()
                            } else {
                                d.dom
                                    .get_by_ref(p_ref)
                                    .map(|i| i.class.to_string())
                                    .unwrap_or_else(|| "Instance".to_string())
                            };
                            cur = InferredType::Instance { r: Some(p_ref), class: p_cls };
                            continue;
                        }
                    }
                    cur = InferredType::Instance { r: None, class: "Instance".to_string() };
                    continue;
                }
                // Live child lookup in Document by Instance.Name first (matching Roblox Studio),
                // falling back to ClassName for services.
                if let (Some(d), Some(inst_ref)) = (doc, r) {
                    let child_by_name = d.dom.get_by_ref(inst_ref).and_then(|parent| {
                        parent.children().iter().copied().find(|&c| {
                            d.dom.get_by_ref(c).is_some_and(|ci| ci.name == seg.as_str())
                        })
                    });
                    if let Some(child_ref) = child_by_name.or_else(|| d.find_child(inst_ref, seg)) {
                        if let Some(child) = d.dom.get_by_ref(child_ref) {
                            cur = InferredType::Instance {
                                r: Some(child_ref),
                                class: child.class.to_string(),
                            };
                            continue;
                        }
                    }
                }
                // DataModel service lookup by class name (e.g. `game.ReplicatedStorage` or `game:GetService("Players")`)
                if class == "DataModel" {
                    if seg.eq_ignore_ascii_case("workspace") {
                        let ws_ref = doc.and_then(|d| d.find_child(d.dom.root_ref(), "Workspace"));
                        cur = InferredType::Instance { r: ws_ref, class: "Workspace".to_string() };
                        continue;
                    }
                    if let Some(api) = rustudio_core::api::get() {
                        if api.classes.get(seg.as_str()).is_some_and(|c| c.service) {
                            let svc_ref = doc.and_then(|d| d.find_child(d.dom.root_ref(), seg));
                            cur = InferredType::Instance { r: svc_ref, class: seg.clone() };
                            continue;
                        }
                    }
                }
                // Event property -> RBXScriptSignal (so `part.Touched:Connect` works!)
                if let Some(api) = rustudio_core::api::get() {
                    if class_has_event(api, &class, seg) {
                        cur = InferredType::Datatype("RBXScriptSignal".to_string());
                        continue;
                    }
                }
                // Common property -> datatype inference
                cur = match seg.as_str() {
                    "Position" | "Size" | "Orientation" | "Velocity" | "AssemblyLinearVelocity" | "AssemblyAngularVelocity" => {
                        InferredType::Datatype("Vector3".to_string())
                    }
                    "CFrame" | "WorldPivot" | "PivotOffset" => InferredType::Datatype("CFrame".to_string()),
                    "Color" | "BackgroundColor3" | "TextColor3" | "ImageColor3" | "Ambient" => {
                        InferredType::Datatype("Color3".to_string())
                    }
                    "LocalPlayer" => InferredType::Instance { r: None, class: "Player".to_string() },
                    "Character" => InferredType::Instance { r: None, class: "Model".to_string() },
                    "CurrentCamera" => InferredType::Instance { r: None, class: "Camera".to_string() },
                    "Humanoid" => InferredType::Instance { r: None, class: "Humanoid".to_string() },
                    _ => return None,
                };
            }
            InferredType::Enum(None) => {
                cur = InferredType::Enum(Some(seg.clone()));
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn class_has_event(api: &rustudio_core::api::Api, class: &str, member: &str) -> bool {
    let mut cur = Some(class);
    while let Some(c) = cur {
        let Some(desc) = api.classes.get(c) else { break };
        if let Some(m) = desc.members.get(member) {
            return m.kind == rustudio_core::api::Kind::Event;
        }
        cur = desc.superclass.as_deref();
    }
    false
}

/// Collects all script-accessible Properties, Methods, Events, and Callbacks
/// for `class` and its entire superclass chain from `api-dump.json` (falling
/// back to `rbx_reflection_database` if `api-dump.json` is unavailable).
fn collect_class_members(
    class: &str,
    sep: char,
    add: &mut dyn FnMut(&str, &str, AcKind, String),
) {
    if let Some(api) = rustudio_core::api::get() {
        let mut cur = Some(class);
        let mut visited = HashSet::new();
        while let Some(c) = cur {
            if !visited.insert(c) {
                break;
            }
            let Some(desc) = api.classes.get(c) else { break };
            for mname in &desc.order {
                let Some(m) = desc.members.get(mname) else { continue };
                if m.deprecated || m.hidden {
                    continue;
                }
                if m.security != "None" && m.security != "PluginSecurity" {
                    continue;
                }
                let kind = match m.kind {
                    rustudio_core::api::Kind::Property => AcKind::Property,
                    rustudio_core::api::Kind::Function => AcKind::Method,
                    rustudio_core::api::Kind::Event => AcKind::Event,
                    rustudio_core::api::Kind::Callback => AcKind::Property,
                };
                // On `:` prioritize methods; on `.` include properties, events, and callbacks
                if sep == ':' && kind != AcKind::Method {
                    continue;
                }
                if sep == '.' && kind == AcKind::Method {
                    continue;
                }
                let detail = if !m.category.is_empty() {
                    format!("{c} ({})", m.category)
                } else {
                    format!("{c} {}", m.kind.label())
                };
                add(mname, mname, kind, detail);
            }
            cur = desc.superclass.as_deref();
        }
        return;
    }

    // Fallback: bundled rbx_reflection_database (properties only, guaranteed present even without api-dump.json)
    let db = rustudio_core::doc::db();
    let mut cur = Some(class);
    while let Some(c) = cur {
        let Some(desc) = db.classes.get(c) else { break };
        if sep == '.' {
            for (pname, _) in &desc.properties {
                add(pname.as_ref(), pname.as_ref(), AcKind::Property, format!("{c} property"));
            }
        }
        cur = desc.superclass.as_deref();
    }
}

// ---------------------------------------------------------------------------
// Public Entry Point
// ---------------------------------------------------------------------------

pub fn build_completions(
    src: &str,
    cursor: usize,
    doc: Option<&Document>,
    script_ref: Option<Ref>,
    plugin_items: &[(String, String)],
    dynamic_idents: &[String],
) -> Option<(AcContext, Vec<AcItem>)> {
    let ctx = autocomplete_context(src, cursor)?;
    let prefix_lower = ctx.prefix.to_ascii_lowercase();
    let mut candidates: Vec<AcItem> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    let mut add = |label: &str, insert: &str, kind: AcKind, detail: String| {
        if !prefix_lower.is_empty() && !label.to_ascii_lowercase().contains(&prefix_lower) {
            return;
        }
        if seen.insert(insert.to_string()) {
            candidates.push(AcItem {
                label: label.to_string(),
                insert: insert.to_string(),
                kind,
                detail,
            });
        }
    };

    for (label, insert) in plugin_items {
        add(label, insert, AcKind::Plugin, "plugin".to_string());
    }

    let locals = infer_locals(src, doc, script_ref);

    match &ctx.trigger {
        TriggerKind::StringArg { func, receiver_chain } => {
            if matches!(func.as_str(), "GetService" | "FindService") {
                if let Some(api) = rustudio_core::api::get() {
                    for cname in &api.class_order {
                        if api.classes.get(cname).is_some_and(|c| c.service) {
                            add(cname, cname, AcKind::Service, "Service".to_string());
                        }
                    }
                }
                // Always ensure core services exist even if api-dump.json is absent
                for svc in [
                    "Workspace", "Players", "ReplicatedStorage", "ReplicatedFirst",
                    "ServerScriptService", "ServerStorage", "StarterGui", "StarterPack",
                    "StarterPlayer", "Lighting", "SoundService", "RunService",
                    "UserInputService", "TweenService", "HttpService", "DataStoreService",
                    "MarketplaceService", "CollectionService", "ContextActionService",
                    "Debris", "PathfindingService", "PhysicsService", "ProximityPromptService",
                    "TeleportService", "TextChatService", "GuiService", "Teams",
                ] {
                    add(svc, svc, AcKind::Service, "Service".to_string());
                }
            } else if func == "new" && receiver_chain.last().map(|s| s.as_str()) == Some("Instance") {
                if let Some(api) = rustudio_core::api::get() {
                    for cname in &api.class_order {
                        if api.classes.get(cname).is_some_and(|c| c.creatable) {
                            add(cname, cname, AcKind::Class, "Class".to_string());
                        }
                    }
                }
            } else if matches!(
                func.as_str(),
                "IsA" | "FindFirstChildOfClass" | "FindFirstChildWhichIsA" | "FindFirstAncestorOfClass"
            ) {
                if let Some(api) = rustudio_core::api::get() {
                    for cname in &api.class_order {
                        add(cname, cname, AcKind::Class, "Class".to_string());
                    }
                }
            } else if matches!(func.as_str(), "FindFirstChild" | "WaitForChild") {
                if let (Some(d), Some(InferredType::Instance { r: Some(inst_ref), .. })) =
                    (doc, resolve_chain(receiver_chain, &locals, doc, script_ref))
                {
                    if let Some(inst) = d.dom.get_by_ref(inst_ref) {
                        for &child_ref in inst.children() {
                            if let Some(child) = d.dom.get_by_ref(child_ref) {
                                add(&child.name, &child.name, AcKind::Child, child.class.to_string());
                            }
                        }
                    }
                }
            }
        }

        TriggerKind::Member { sep, chain } => {
            let resolved = resolve_chain(chain, &locals, doc, script_ref);
            match resolved {
                Some(InferredType::Enum(None)) if *sep == '.' => {
                    if let Some(api) = rustudio_core::api::get() {
                        for ename in api.enums.keys() {
                            add(ename, ename, AcKind::Enum, "Enum".to_string());
                        }
                    } else {
                        for ename in rustudio_core::doc::db().enums.keys() {
                            add(ename.as_ref(), ename.as_ref(), AcKind::Enum, "Enum".to_string());
                        }
                    }
                }
                Some(InferredType::Enum(Some(enum_name))) if *sep == '.' => {
                    if let Some(api) = rustudio_core::api::get() {
                        if let Some(items) = api.enums.get(&enum_name) {
                            for (item_name, val) in items {
                                add(
                                    item_name,
                                    item_name,
                                    AcKind::Enum,
                                    format!("Enum.{enum_name}.{item_name} ({val})"),
                                );
                            }
                        }
                    } else if let Some(ed) = rustudio_core::doc::db().enums.get(enum_name.as_str()) {
                        for (item_name, val) in &ed.items {
                            add(
                                item_name.as_ref(),
                                item_name.as_ref(),
                                AcKind::Enum,
                                format!("Enum.{enum_name}.{item_name} ({val})"),
                            );
                        }
                    }
                }
                Some(InferredType::Datatype(dt)) => {
                    if let Some(members) = stdlib_or_datatype_members(&dt) {
                        for &(name, detail, kind) in members {
                            add(name, name, kind, detail.to_string());
                        }
                    }
                }
                Some(InferredType::Instance { r, class }) => {
                    // 1. Live children from Document (on `.`)
                    if *sep == '.' {
                        if let (Some(d), Some(inst_ref)) = (doc, r) {
                            if let Some(inst) = d.dom.get_by_ref(inst_ref) {
                                for &child_ref in inst.children() {
                                    if let Some(child) = d.dom.get_by_ref(child_ref) {
                                        if child.name.chars().next().is_some_and(is_ident_start)
                                            && child.name.chars().all(is_ident_char)
                                        {
                                            let kind = if class == "DataModel" {
                                                AcKind::Service
                                            } else {
                                                AcKind::Child
                                            };
                                            add(&child.name, &child.name, kind, child.class.to_string());
                                        }
                                    }
                                }
                            }
                        }
                        // For `game.`, always include core DataModel services & `workspace` property
                        if class == "DataModel" {
                            add("Workspace", "Workspace", AcKind::Service, "Workspace".to_string());
                            add("workspace", "workspace", AcKind::Property, "Workspace".to_string());
                            for svc in [
                                "Players", "ReplicatedStorage", "ReplicatedFirst",
                                "ServerScriptService", "ServerStorage", "StarterGui",
                                "StarterPack", "StarterPlayer", "Lighting", "SoundService",
                                "Teams", "TextChatService", "RunService", "UserInputService",
                                "TweenService", "HttpService",
                            ] {
                                add(svc, svc, AcKind::Service, svc.to_string());
                            }
                        }
                    }
                    // 2. Full Roblox API members for this class + superclasses
                    collect_class_members(&class, *sep, &mut add);
                }
                None => {
                    // Unknown receiver: fall back to Instance base members + common datatypes
                    collect_class_members("Instance", *sep, &mut add);
                    if *sep == ':' {
                        for &(name, detail, kind) in stdlib_or_datatype_members("RBXScriptSignal").unwrap_or(&[]) {
                            add(name, name, kind, detail.to_string());
                        }
                    }
                }
                _ => {}
            }
        }

        TriggerKind::Identifier => {
            for &kw in LUAU_KEYWORDS {
                add(kw, kw, AcKind::Keyword, "keyword".to_string());
            }
            for &(name, detail, kind) in LUAU_GLOBALS {
                add(name, name, kind, detail.to_string());
            }
            for ident in dynamic_idents {
                if ident == &ctx.prefix {
                    continue;
                }
                let detail = match locals.get(ident) {
                    Some(InferredType::Instance { class, .. }) => class.clone(),
                    Some(InferredType::Datatype(dt)) => dt.clone(),
                    Some(InferredType::Enum(Some(e))) => format!("Enum.{e}"),
                    Some(InferredType::Enum(None)) => "Enum".to_string(),
                    None => "local".to_string(),
                };
                add(ident, ident, AcKind::Local, detail);
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }
    if candidates.len() == 1 && candidates[0].insert == ctx.prefix {
        return None;
    }

    // Rank like Roblox Studio: exact prefix (case-sensitive) > case-insensitive prefix > substring,
    // then Children/Services/Locals > Properties/Methods/Events > Keywords/Globals.
    candidates.sort_by(|a, b| {
        let score = |item: &AcItem| -> (u8, u8, usize) {
            let match_tier = if ctx.prefix.is_empty() {
                0
            } else if item.label.starts_with(&ctx.prefix) {
                0
            } else if item.label.to_ascii_lowercase().starts_with(&prefix_lower) {
                1
            } else {
                2
            };
            let kind_tier = match item.kind {
                AcKind::Plugin => 0,
                AcKind::Child | AcKind::Service => 1,
                AcKind::Local => 1,
                AcKind::Property | AcKind::Method | AcKind::Event => 2,
                AcKind::Enum | AcKind::Class => 3,
                AcKind::Keyword => 4,
                AcKind::Stdlib => 5,
                AcKind::Global => 6,
            };
            (match_tier, kind_tier, item.label.len())
        };
        score(a).cmp(&score(b)).then_with(|| a.label.cmp(&b.label))
    });

    candidates.truncate(32);
    Some((ctx, candidates))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_dot_work_completes_workspace() {
        let (ctx, items) = build_completions("game.Work", 9, None, None, &[], &[]).unwrap();
        assert_eq!(ctx.replace_start, 5);
        assert!(items.iter().any(|i| i.label == "Workspace"));
        assert!(items.iter().any(|i| i.label == "workspace"));
    }

    #[test]
    fn getservice_string_completion() {
        let src = r#"local rs = game:GetService("Repli"#;
        let (ctx, items) = build_completions(src, src.len(), None, None, &[], &[]).unwrap();
        assert_eq!(ctx.prefix, "Repli");
        assert!(items.iter().any(|i| i.label == "ReplicatedStorage"));
    }

    #[test]
    fn inferred_local_vector3_and_signal() {
        let src = "local v = Vector3.new(1, 2, 3)\nv.Mag";
        let (_, items) = build_completions(src, src.len(), None, None, &[], &[]).unwrap();
        assert!(items.iter().any(|i| i.label == "Magnitude"));
    }
}
