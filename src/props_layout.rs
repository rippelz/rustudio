//! Which rows Studio's Properties panel shows for a class, in which
//! category and order (checks/ui/studio/props-*.png: a Part, a Model, a
//! Script, a ScreenGui and Lighting in an offline Studio).
//!
//! From Studio's own data: each property's category (the API dump), hidden
//! ones (tags Hidden / NotBrowsable / Deprecated, ReflectionMetadata
//! Browsable = false, read security beyond Plugin), and the order within a
//! category: ReflectionMetadata PropertyOrder first (Archivable 990,
//! Anchored 900, CanCollide / CanQuery / CanTouch 1000 / 1001 / 1002,
//! AssemblyLinear / AngularVelocity 1000 / 1010…), then the rest A–Z
//! ignoring case (ResizeableFaces before ResizeIncrement).
//!
//! From the captures (Studio's Properties is a Luau plugin whose rules
//! aren't in its data), as noted per item below: the category order,
//! Archivable under Data, the rows it leaves out, the Transform / Pivot
//! composite rows, CanQuery hidden while CanCollide is on, Permissions only
//! for Models and scripts, and Surface starting collapsed.

/// Studio's category order (captured); others follow A–Z, Permissions last.
const CATEGORY_ORDER: &[&str] = &[
    "Appearance", "Data", "Transform", "Pivot", "Behavior", "Collision", "Part", "Assembly", "Surface Inputs",
    "Surface",
];

/// Left out of Studio's panel although its data lists them (captured).
fn left_out(class: &str, prop: &str) -> bool {
    let is = |b: &str| rustudio_core::doc::is_a(class, b);
    matches!(prop, "PredictionMode" | "Capabilities" | "Sandboxed" | "ExtendLightRangeTo120" | "ExtentsCFrame" | "ExtentsSize" | "Pivot Offset")
        || (prop == "Rotation" && is("BasePart"))
        || (prop == "Disabled" && is("BaseScript"))
        || (prop == "Source" && is("LuaSourceContainer"))
        || (prop.starts_with("SelectionBehavior") && is("LayerCollector"))
        || (prop == "ModelStreamingMode" && is("Model"))
        // Studio 0.748 shows Asset -> AudioContent (with Preview child) instead
        // of legacy SoundId / Asset in Properties.
        || (prop == "SoundId" && is("Sound"))
        || (prop == "Asset" && is("AudioPlayer"))
        // shown as composite rows instead
        || (matches!(prop, "CFrame" | "PivotOffset" | "Origin" | "Position" | "Orientation") && is("BasePart"))
        || (matches!(prop, "Origin" | "WorldPivot") && is("Model"))
}

/// One row of the panel.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// A property: (name, read-only).
    Prop(String, bool),
    /// A CFrame shown as Position / Orientation children: (label, which).
    Frame(&'static str, Frame),
}

/// Where a composite Transform / Pivot row reads and writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// BasePart.CFrame.
    PartCFrame,
    /// A part's pivot (CFrame × PivotOffset); setting it moves the part.
    PartOrigin,
    /// BasePart.PivotOffset.
    PivotOffset,
    /// A Model's pivot; setting it moves the model.
    ModelOrigin,
    /// Model.WorldPivotData (the pivot alone).
    WorldPivot,
    /// Attachment.CFrame: its frame in the parent part (or Bone).
    AttachmentCFrame,
    /// Attachment.WorldCFrame, computed from the part and ancestor Bones;
    /// setting it writes the local CFrame that puts the attachment there.
    AttachmentWorldCFrame,
}

#[derive(Clone, Debug)]
pub struct Category {
    pub name: String,
    pub rows: Vec<Row>,
    /// Studio opens Surface collapsed (captured).
    pub collapsed: bool,
}

fn category_rank(c: &str) -> (u8, String) {
    match CATEGORY_ORDER.iter().position(|x| *x == c) {
        Some(i) => (0, format!("{i:03}")),
        None if c == "Permissions" => (2, String::new()),
        None => (1, c.to_lowercase()),
    }
}

/// The property order from ReflectionMetadata (nearest class in the chain).
fn property_order(chain: &[String], prop: &str) -> Option<i32> {
    let meta = crate::studio_panels::member_meta();
    chain.iter().find_map(|c| meta.get(&(c.clone(), prop.to_string())).and_then(|m| m.0))
}

fn browsable(chain: &[String], prop: &str) -> bool {
    let meta = crate::studio_panels::member_meta();
    chain
        .iter()
        .find_map(|c| meta.get(&(c.clone(), prop.to_string())).map(|m| m.1))
        .unwrap_or(true)
}

/// The panel's categories and rows for `class` (`can_collide`: whether the
/// selected part has CanCollide on, which hides CanQuery).
pub fn rows(class: &str, can_collide: bool) -> Vec<Category> {
    let Some(api) = rustudio_core::api::get() else {
        return Vec::new();
    };
    let mut chain: Vec<String> = Vec::new();
    let mut cur = Some(class.to_string());
    while let Some(c) = cur {
        let next = api.classes.get(&c).and_then(|k| k.superclass.clone());
        chain.push(c);
        cur = next;
    }
    let is = |b: &str| rustudio_core::doc::is_a(class, b);
    let mut found: Vec<(String, String, Option<i32>, bool)> = Vec::new();
    for c in &chain {
        let Some(k) = api.classes.get(c) else { continue };
        for name in &k.order {
            let m = &k.members[name];
            if m.kind != rustudio_core::api::Kind::Property || found.iter().any(|f| f.1 == *name) {
                continue;
            }
            let shown_security = matches!(m.security.as_str(), "None" | "PluginSecurity") || name == "UniqueId";
            if m.hidden || m.deprecated || !shown_security || !browsable(&chain, name) || left_out(class, name) {
                continue;
            }
            if name == "CanQuery" && can_collide {
                continue;
            }
            // Studio lists Archivable under Data (captured; the dump says Behavior).
            let mut category = if name == "Archivable" { "Data".to_string() } else { m.category.clone() };
            if category.is_empty() {
                category = "Data".into();
            }
            if category == "Permissions" && !(is("Model") || is("LuaSourceContainer")) {
                continue;
            }
            found.push((category, name.clone(), property_order(&chain, name), m.read_only));
        }
    }
    let mut cats: Vec<Category> = Vec::new();
    let mut names: Vec<String> = found.iter().map(|f| f.0.clone()).collect();
    if is("BasePart") || is("Model") {
        names.push("Transform".into());
        names.push("Pivot".into());
    }
    names.sort_by_key(|c| category_rank(c));
    names.dedup();
    for cat in names {
        let mut props: Vec<&(String, String, Option<i32>, bool)> = found.iter().filter(|f| f.0 == cat).collect();
        props.sort_by(|a, b| {
            a.2.unwrap_or(i32::MAX).cmp(&b.2.unwrap_or(i32::MAX)).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        let mut rows: Vec<Row> = props.iter().map(|f| Row::Prop(f.1.clone(), f.3)).collect();
        // Composite rows (captured): a part's Transform is Size, CFrame and
        // Origin; its Pivot is PivotOffset. A Model's Transform is Origin;
        // its Pivot is PrimaryPart, Scale and WorldPivot.
        if is("BasePart") && cat == "Transform" {
            rows = vec![Row::Prop("Size".into(), false), Row::Frame("CFrame", Frame::PartCFrame), Row::Frame("Origin", Frame::PartOrigin)];
        }
        if is("BasePart") && cat == "Pivot" {
            rows = vec![Row::Frame("PivotOffset", Frame::PivotOffset)];
        }
        if is("Model") && cat == "Transform" {
            rows = vec![Row::Frame("Origin", Frame::ModelOrigin)];
        }
        if is("Model") && cat == "Pivot" {
            rows.retain(|r| matches!(r, Row::Prop(n, _) if n == "PrimaryPart" || n == "Scale"));
            rows.push(Row::Frame("WorldPivot", Frame::WorldPivot));
        }
        // An Attachment's CFrame and WorldCFrame are edited as Position /
        // Orientation children, like a part's frame; WorldCFrame is worked
        // out from the parent part (and Bones) rather than stored.
        if is("Attachment") {
            for row in &mut rows {
                if let Row::Prop(name, _) = row {
                    match name.as_str() {
                        "CFrame" if cat == "Transform" => *row = Row::Frame("CFrame", Frame::AttachmentCFrame),
                        "WorldCFrame" => *row = Row::Frame("WorldCFrame", Frame::AttachmentWorldCFrame),
                        _ => {}
                    }
                }
            }
        }
        if rows.is_empty() {
            continue;
        }
        // Studio starts "Surface" and "Surface Inputs" collapsed, the rest
        // open, unless toggled before (,
        // the set built).
        cats.push(Category { collapsed: matches!(cat.as_str(), "Surface" | "Surface Inputs"), name: cat, rows });
    }
    cats
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(class: &str) -> Vec<(String, Vec<String>)> {
        rows(class, true)
            .into_iter()
            .map(|c| {
                let r = c
                    .rows
                    .iter()
                    .map(|r| match r {
                        Row::Prop(n, _) => n.clone(),
                        Row::Frame(l, _) => l.to_string(),
                    })
                    .collect();
                (c.name, r)
            })
            .collect()
    }

    fn want(spec: &[(&str, &[&str])]) -> Vec<(String, Vec<String>)> {
        spec.iter().map(|(c, r)| (c.to_string(), r.iter().map(|s| s.to_string()).collect())).collect()
    }

    /// The five classes captured from Studio's Properties panel.
    #[test]
    fn matches_studio_captures() {
        if crate::studio_panels::member_meta().is_empty() {
            return; // (no Studio install to read ReflectionMetadata from)
        }
        assert_eq!(names("Part"), want(&[
            ("Appearance", &["BrickColor", "CastShadow", "Color", "Material", "MaterialVariant", "Reflectance", "Transparency"]),
            ("Data", &["Archivable", "ClassName", "Locked", "Name", "Parent", "ResizeableFaces", "ResizeIncrement", "UniqueId"]),
            ("Transform", &["Size", "CFrame", "Origin"]),
            ("Pivot", &["PivotOffset"]),
            ("Behavior", &["EnableFluidForces"]),
            ("Collision", &["CanCollide", "CanTouch", "AudioCanCollide", "CollisionGroup"]),
            ("Part", &["Anchored", "CenterOfMass", "CurrentPhysicalProperties", "CustomPhysicalProperties", "Mass", "Massless", "RootPriority", "Shape"]),
            ("Assembly", &["AssemblyLinearVelocity", "AssemblyAngularVelocity", "AssemblyCenterOfMass", "AssemblyMass", "AssemblyRootPart"]),
            ("Surface", &["BackSurface", "BottomSurface", "FrontSurface", "LeftSurface", "RightSurface", "TopSurface"]),
        ]));
        assert_eq!(names("Model"), want(&[
            ("Appearance", &["LevelOfDetail"]),
            ("Data", &["Archivable", "ClassName", "Name", "Parent", "UniqueId"]),
            ("Transform", &["Origin"]),
            ("Pivot", &["PrimaryPart", "Scale", "WorldPivot"]),
            ("Permissions", &["IsInSandbox"]),
        ]));
        assert_eq!(names("Script"), want(&[
            ("Data", &["Archivable", "ClassName", "Name", "Parent", "UniqueId"]),
            ("Behavior", &["Enabled", "RunContext"]),
            ("Permissions", &["IsInSandbox"]),
        ]));
        assert_eq!(names("ScreenGui"), want(&[
            ("Appearance", &["ClipToDeviceSafeArea", "SafeAreaCompatibility", "ScreenInsets"]),
            ("Data", &["Archivable", "AbsolutePosition", "AbsoluteRotation", "AbsoluteSize", "ClassName", "DisplayOrder", "Enabled", "IgnoreGuiInset", "Name", "Parent", "ResetOnSpawn", "UniqueId", "ZIndexBehavior"]),
            ("Localization", &["AutoLocalize", "RootLocalizationTable"]),
            ("Selection", &["SelectionGroup"]),
        ]));
        assert_eq!(names("Lighting"), want(&[
            ("Appearance", &["Ambient", "Brightness", "ColorShift_Bottom", "ColorShift_Top", "EnvironmentDiffuseScale", "EnvironmentSpecularScale", "GlobalShadows", "LightingStyle", "OutdoorAmbient", "PrioritizeLightingQuality", "ShadowSoftness"]),
            ("Data", &["Archivable", "ClassName", "ClockTime", "GeographicLatitude", "Name", "Parent", "TimeOfDay", "UniqueId"]),
            ("Exposure", &["ExposureCompensation"]),
            ("Fog", &["FogColor", "FogEnd", "FogStart"]),
        ]));
    }

    /// An Attachment's CFrame and WorldCFrame are Position / Orientation
    /// frame rows (WorldCFrame is computed; editing it writes the local
    /// CFrame), and Axis / SecondaryAxis stay plain Vector3 rows.
    #[test]
    fn attachment_frames_are_composite_rows() {
        let cats = rows("Attachment", true);
        let rows_of = |name: &str| {
            cats.iter()
                .find(|c| c.name == name)
                .map(|c| c.rows.clone())
                .unwrap_or_default()
        };
        assert!(rows_of("Transform").contains(&Row::Frame("CFrame", Frame::AttachmentCFrame)));
        assert!(rows_of("Derived World Data").contains(&Row::Frame("WorldCFrame", Frame::AttachmentWorldCFrame)));
        assert!(rows_of("Derived Data").contains(&Row::Prop("Axis".into(), false)));
        assert!(rows_of("Derived Data").contains(&Row::Prop("SecondaryAxis".into(), false)));
        assert!(rows_of("Derived World Data").contains(&Row::Prop("WorldAxis".into(), false)));
        assert!(rows_of("Derived World Data").contains(&Row::Prop("WorldSecondaryAxis".into(), false)));
    }

    #[test]
    fn gui_classes_match_studio_category_order() {
        if crate::studio_panels::member_meta().is_empty() {
            return;
        }
        let cat_names = |cls: &str| rows(cls, true).into_iter().map(|c| c.name).collect::<Vec<_>>();
        assert_eq!(cat_names("Frame"), vec!["Appearance", "Data", "Behavior", "Localization", "Selection", "State"]);
        assert_eq!(cat_names("TextLabel"), vec!["Appearance", "Data", "Behavior", "Localization", "Selection", "State", "Text"]);
        assert_eq!(cat_names("ImageLabel"), vec!["Appearance", "Data", "Behavior", "Image", "Localization", "Selection", "State"]);
        assert_eq!(cat_names("UIGradient"), vec!["Appearance", "Data"]);
        // Key GUI properties appear in their expected categories.
        let tl = names("TextLabel");
        let text_rows = &tl.iter().find(|(c, _)| c == "Text").unwrap().1;
        assert!(text_rows.contains(&"FontFace".to_string()));
        assert!(text_rows.contains(&"Text".to_string()));
        assert!(!text_rows.contains(&"Font".to_string()), "deprecated legacy Font should stay hidden");
        let il = names("ImageLabel");
        let img_rows = &il.iter().find(|(c, _)| c == "Image").unwrap().1;
        assert!(img_rows.contains(&"SliceCenter".to_string()));
        assert!(img_rows.contains(&"Image".to_string()));
    }
}
