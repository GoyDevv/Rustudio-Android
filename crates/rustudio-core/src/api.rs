//! Studio's own API dump (app/api-dump.json, written by Studio
//! 0.744.0.7441077's `-API`: 942 classes, 3796 properties, 3374 functions,
//! 849 events, 652 enums), indexed for the gap telemetry and the
//! coverage table: which members exist, where they are declared, and what a
//! game script may touch (Security None).

use std::collections::HashMap;
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Property,
    Function,
    Event,
    Callback,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Property => "property",
            Kind::Function => "function",
            Kind::Event => "event",
            Kind::Callback => "callback",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Member {
    pub kind: Kind,
    /// Security for reading / calling ("None" = any game script).
    pub security: String,
    /// Saved in place files (Serialization.CanLoad): the value a script reads
    /// is the stored one. Not saved = the engine works it out at run time.
    pub can_load: bool,
    pub read_only: bool,
    pub deprecated: bool,
    /// The Properties panel's category ("Appearance", "Data"...).
    pub category: String,
    /// Tagged Hidden or NotBrowsable: never listed in Properties.
    pub hidden: bool,
}

#[derive(Debug, Default)]
pub struct Class {
    pub superclass: Option<String>,
    pub members: HashMap<String, Member>,
    /// Declaration order, for stable listings.
    pub order: Vec<String>,
    pub creatable: bool,
    pub service: bool,
}

#[derive(Debug, Default)]
pub struct Api {
    pub classes: HashMap<String, Class>,
    pub class_order: Vec<String>,
    /// (enum, legacy item name) → the item's name now: the dump's
    /// LegacyNames (Enum.ContextActionPriority.Default is Medium).
    pub legacy_enum_items: HashMap<(String, String), String>,
    /// Every enum of the dump: name → (item, value). Enums Studio has that
    /// the reflection database lacks (ProjectionType, OrthographicMode…)
    /// come from here (ltypes::LEnumItem).
    pub enums: HashMap<String, Vec<(String, u32)>>,
}

fn dump_path() -> std::path::PathBuf {
    std::env::var_os("RUSTUDIO_API_DUMP")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::paths::resource("api-dump.json"))
}

/// The dump, parsed once when available. Never permanently latches `None` if
/// called before Android finishes unpacking `assets/api-dump.json`.
pub fn get() -> Option<&'static Api> {
    static API: OnceLock<Api> = OnceLock::new();
    static INIT_LOCK: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    if let Some(api) = API.get() {
        return Some(api);
    }
    let mut last_fail = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(api) = API.get() {
        return Some(api);
    }
    if last_fail.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(3)) {
        return None;
    }
    let path = dump_path();
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            log::error!("api: failed to open {}: {e}", path.display());
            *last_fail = Some(std::time::Instant::now());
            return None;
        }
    };
    let reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let Some(parsed) = parse_reader(reader) else {
        log::error!("api: failed to parse {}", path.display());
        *last_fail = Some(std::time::Instant::now());
        return None;
    };
    let _ = API.set(parsed);
    API.get()
}

#[derive(serde::Deserialize)]
struct RawDump {
    #[serde(rename = "Classes", default)]
    classes: Vec<RawClass>,
    #[serde(rename = "Enums", default)]
    enums: Vec<RawEnum>,
}

#[derive(serde::Deserialize)]
struct RawClass {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Superclass", default)]
    superclass: Option<String>,
    #[serde(rename = "Tags", default)]
    tags: Vec<serde_json::Value>,
    #[serde(rename = "Members", default)]
    members: Vec<RawMember>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RawSecurity {
    Str(String),
    Obj {
        #[serde(rename = "Read", default)]
        read: Option<String>,
    },
}

#[derive(serde::Deserialize, Default)]
struct RawSerialization {
    #[serde(rename = "CanLoad", default)]
    can_load: bool,
}

#[derive(serde::Deserialize)]
struct RawMember {
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "MemberType", default)]
    member_type: Option<String>,
    #[serde(rename = "Security", default)]
    security: Option<RawSecurity>,
    #[serde(rename = "Tags", default)]
    tags: Vec<serde_json::Value>,
    #[serde(rename = "Serialization", default)]
    serialization: RawSerialization,
    #[serde(rename = "Category", default)]
    category: Option<String>,
}

#[derive(serde::Deserialize)]
struct RawEnum {
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "Items", default)]
    items: Vec<RawEnumItem>,
}

#[derive(serde::Deserialize)]
struct RawEnumItem {
    #[serde(rename = "Name", default)]
    name: Option<String>,
    #[serde(rename = "Value", default)]
    value: Option<u64>,
    #[serde(rename = "LegacyNames", default)]
    legacy_names: Vec<String>,
}

fn has_tag(tags: &[serde_json::Value], needle: &str) -> bool {
    tags.iter().any(|t| t.as_str() == Some(needle))
}

fn build_api(raw: RawDump) -> Option<Api> {
    if raw.classes.is_empty() {
        return None;
    }
    let mut api = Api::default();
    api.classes.reserve(raw.classes.len());
    api.class_order.reserve(raw.classes.len());
    for c in raw.classes {
        let mut class = Class {
            superclass: c.superclass.filter(|s| s != "<<<ROOT>>>"),
            creatable: !has_tag(&c.tags, "NotCreatable") && !has_tag(&c.tags, "Service"),
            service: has_tag(&c.tags, "Service"),
            ..Default::default()
        };
        for m in c.members {
            let Some(mname) = m.name else {
                continue;
            };
            let kind = match m.member_type.as_deref() {
                Some("Property") => Kind::Property,
                Some("Function") => Kind::Function,
                Some("Event") => Kind::Event,
                Some("Callback") => Kind::Callback,
                _ => continue,
            };
            let security = match m.security {
                Some(RawSecurity::Str(s)) => s,
                Some(RawSecurity::Obj { read }) => read.unwrap_or_else(|| "None".into()),
                None => "None".into(),
            };
            let read_only = has_tag(&m.tags, "ReadOnly");
            let deprecated = has_tag(&m.tags, "Deprecated");
            let hidden = has_tag(&m.tags, "Hidden") || has_tag(&m.tags, "NotBrowsable");
            class.order.push(mname.clone());
            class.members.insert(
                mname,
                Member {
                    kind,
                    security,
                    can_load: m.serialization.can_load,
                    read_only,
                    deprecated,
                    category: m.category.unwrap_or_default(),
                    hidden,
                },
            );
        }
        api.class_order.push(c.name.clone());
        api.classes.insert(c.name, class);
    }
    for e in raw.enums {
        let Some(en) = e.name else {
            continue;
        };
        // An enum with no items still exists (AudioCaptureMode in 0.744).
        let entry = api.enums.entry(en.clone()).or_default();
        for it in e.items {
            let Some(name) = it.name else {
                continue;
            };
            if let Some(v) = it.value {
                entry.push((name.clone(), v as u32));
            }
            for old in it.legacy_names {
                api.legacy_enum_items.insert((en.clone(), old), name.clone());
            }
        }
    }
    Some(api)
}

fn parse_reader<R: std::io::Read>(reader: R) -> Option<Api> {
    let raw: RawDump = serde_json::from_reader(reader).ok()?;
    build_api(raw)
}

pub fn parse(text: &str) -> Option<Api> {
    let raw: RawDump = serde_json::from_str(text).ok()?;
    build_api(raw)
}

impl Api {
    /// The member `name` of `class` or a superclass: (declaring class, member).
    pub fn lookup<'a>(&'a self, class: &str, name: &str) -> Option<(&'a str, &'a Member)> {
        let mut cur = Some(class);
        let mut guard = 0;
        while let Some(c) = cur {
            let (k, info) = self.classes.get_key_value(c)?;
            if let Some(m) = info.members.get(name) {
                return Some((k.as_str(), m));
            }
            cur = info.superclass.as_deref();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        None
    }

    /// Like `lookup`, with the dump's own strings: (declaring class, member
    /// name), borrowed from the parsed dump (no allocation on hot paths).
    pub fn lookup_names<'a>(&'a self, class: &str, name: &str) -> Option<(&'a str, &'a str)> {
        let mut cur = Some(class);
        let mut guard = 0;
        while let Some(c) = cur {
            let (k, info) = self.classes.get_key_value(c)?;
            if let Some((m, _)) = info.members.get_key_value(name) {
                return Some((k.as_str(), m.as_str()));
            }
            cur = info.superclass.as_deref();
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        None
    }

    /// Whether `class` is `base` or derives from it.
    pub fn is_a(&self, class: &str, base: &str) -> bool {
        let mut cur = Some(class);
        let mut guard = 0;
        while let Some(c) = cur {
            if c == base {
                return true;
            }
            cur = self.classes.get(c).and_then(|i| i.superclass.as_deref());
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        false
    }

    pub fn count(&self, kind: Kind, game_only: bool) -> usize {
        self.classes
            .values()
            .flat_map(|c| c.members.values())
            .filter(|m| m.kind == kind && (!game_only || m.security == "None"))
            .count()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dump_counts_match_studio() {
        let Some(api) = super::get() else { return };
        use super::Kind::*;
        // Studio 0.744.0.7441077's own `-API` dump.
        assert_eq!(api.classes.len(), 942);
        // (+1: DataModel.ServerRegion, from Studio's current dump.)
        assert_eq!(api.count(Property, false), 3797);
        assert_eq!(api.count(Function, false), 3374);
        assert_eq!(api.count(Event, false), 849);
        let (decl, m) = api.lookup("Part", "Anchored").unwrap();
        assert_eq!(decl, "BasePart");
        assert_eq!(m.kind, Property);
        assert!(api.lookup("Part", "Destroy").is_some());
        assert_eq!(api.enums.len(), 652);
        assert!(api.lookup("UserInputService", "IsWindowFocused").is_none());
    }
}
