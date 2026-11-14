//! The signed-in person's avatar for Play: rig type, body colours, scales,
//! classic clothing, face, accessories and layered clothing, from the
//! session's own avatar description (avatar.roblox.com/v1/avatar) and asset
//! delivery with the session. Signed out, the default body is used.

use rbx_dom_weak::types::{Content, ContentId, Enum, Ref, Variant};
use rbx_dom_weak::{ustr, InstanceBuilder, WeakDom};
use rustudio_account::avatar::{parse_avatar, AvatarInfo};

use crate::app::Editor;
use crate::assets::SharedClient;
use crate::doc;
use crate::rig::{BodyColors, Scales};

/// What a character should look like.
#[derive(Debug, Clone)]
pub struct AvatarLook {
    pub user_id: u64,
    pub r15: bool,
    pub colors: BodyColors,
    pub scales: Scales,
    /// BodyProportionScale / BodyTypeScale (R15 part proportions).
    pub proportion: f32,
    pub body_type: f32,
    /// Clothing template / graphic content ids.
    pub shirt: Option<String>,
    pub pants: Option<String>,
    pub tshirt: Option<String>,
    pub face: Option<String>,
    /// Accessory models (.rbxm bytes).
    pub accessories: Vec<Vec<u8>>,
    /// Body part packages (Torso / arms / legs: .rbxm bytes with R6
    /// CharacterMeshes and R15 MeshParts) and the head (a SpecialMesh).
    pub body_parts: Vec<Vec<u8>>,
    pub head: Option<Vec<u8>>,
    /// Animation packages (Enum.AssetType name such as "IdleAnimation",
    /// asset id, .rbxm bytes): an `R15Anim` folder of StringValues named
    /// like the Animate script's slots (idle, walk, run, jump, fall, climb,
    /// swim, swimidle, mood) holding the Animations.
    pub animations: Vec<(String, u64, Vec<u8>)>,
    /// Equipped emotes: (name, emote asset id, wheel slot 1–8).
    pub emotes: Vec<(String, u64, u32)>,
    /// Worn items rustudio can't show yet (emotes…).
    pub skipped: Vec<String>,
}

/// A content property, under its classic name or the newer `…Content` name
/// files are migrated to (e.g. ShirtTemplate → ShirtTemplateContent).
fn content_of(dom: &WeakDom, class: &str, prop: &str) -> Option<String> {
    let i = dom.descendants().find(|i| i.class == class)?;
    [prop.to_string(), format!("{prop}Content")]
        .iter()
        .find_map(|name| match i.properties.get(&ustr(name)) {
            Some(Variant::ContentId(c)) => Some(c.as_str().to_string()),
            Some(Variant::Content(c)) => c.as_uri().map(str::to_string),
            Some(Variant::String(s)) => Some(s.clone()),
            _ => None,
        })
        .filter(|s| !s.is_empty())
}

fn decode_model(bytes: &[u8]) -> Result<WeakDom, String> {
    doc::Document::decode(bytes)
}

/// Enum.AssetType name for an id (from the bundled reflection database).
fn asset_type_name(info_name: &str, id: u32) -> String {
    if !info_name.is_empty() {
        return info_name.to_string();
    }
    doc::db()
        .enums
        .get("AssetType")
        .and_then(|e| {
            e.items
                .iter()
                .find(|(_, v)| **v == id)
                .map(|(k, _)| k.to_string())
        })
        .unwrap_or_default()
}

/// Body colours from avatar.roblox.com/v1/avatar's `bodyColors`, which
/// gives BrickColor numbers (headColorId …) rather than hex.
fn v1_colors(v: &serde_json::Value) -> Option<[[u8; 3]; 6]> {
    let c = v.get("bodyColors")?;
    let col = |k: &str| -> Option<[u8; 3]> {
        let n = c.get(k)?.as_u64()? as u16;
        let b = rbx_dom_weak::types::BrickColor::from_number(n)?.to_color3uint8();
        Some([b.r, b.g, b.b])
    };
    Some([
        col("headColorId")?,
        col("torsoColorId")?,
        col("leftArmColorId")?,
        col("rightArmColorId")?,
        col("leftLegColorId")?,
        col("rightLegColorId")?,
    ])
}

/// Enum.AssetType names of body part packages.
fn is_body_part(kind: &str) -> bool {
    matches!(kind, "Torso" | "LeftArm" | "RightArm" | "LeftLeg" | "RightLeg")
}

/// Enum.AssetType names of the animation packages an avatar wears (the
/// HumanoidDescription's …Animation properties).
pub fn is_animation_pack(kind: &str) -> bool {
    matches!(
        kind,
        "IdleAnimation"
            | "WalkAnimation"
            | "RunAnimation"
            | "JumpAnimation"
            | "FallAnimation"
            | "ClimbAnimation"
            | "SwimAnimation"
            | "MoodAnimation"
    )
}

/// Fetches and resolves the signed-in person's avatar (runs on a worker
/// thread): the session's own description, avatar.roblox.com/v1/avatar.
pub fn load(client: &SharedClient, log: &mut dyn FnMut(String)) -> Result<AvatarLook, String> {
    // As Studio does (sub_Studio): the avatar-fetch description for this
    // place, whose resolvedAvatarType is the rig Roblox resolved. A local
    // file has no place id, so place 0. Falls back to the session's own
    // avatar.roblox.com/v1/avatar.
    let (user_id, json) = {
        let guard = client.lock().map_err(|_| "account busy".to_string())?;
        let c = guard.as_ref().ok_or("signed out")?;
        let id = c.user_id();
        let fetched = c
            .session
            .get(&rustudio_account::avatar::avatar_fetch_url(0, id))
            .ok()
            .filter(|(status, _)| *status == 200)
            .and_then(|(_, text)| serde_json::from_str::<serde_json::Value>(&text).ok())
            .filter(|v| v.get("resolvedAvatarType").is_some());
        match fetched {
            Some(v) => (id, v),
            None => (id, c.avatar_json().map_err(|e| e.to_string())?),
        }
    };
    let info = info_from_json(user_id, &json);
    log(format!(
        "Avatar of user {user_id}: {} with {} worn items",
        if info.r15 { "R15" } else { "R6" },
        info.assets.len()
    ));
    let mut look = look_from_info(client, user_id, &info, log)?;
    look.emotes = emotes_of(&json);
    if !look.emotes.is_empty() {
        log(format!(
            "Emotes: {}",
            look.emotes.iter().map(|e| e.0.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(look)
}

/// Equipped emotes from an avatar reply's `emotes`
/// ([{assetId, assetName, position}]).
pub fn emotes_of(json: &serde_json::Value) -> Vec<(String, u64, u32)> {
    json.get("emotes")
        .and_then(|e| e.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            Some((
                e.get("assetName")?.as_str()?.to_string(),
                e.get("assetId")?.as_u64()?,
                e.get("position").and_then(|p| p.as_u64()).unwrap_or(0) as u32,
            ))
        })
        .collect()
}

/// The HumanoidDescription a spawned character's Humanoid carries for its
/// emotes (HumanoidDescription:GetEmotes / GetEquippedEmotes storage,
/// domains/avatar_desc.rs).
pub fn emote_description(look: &AvatarLook) -> InstanceBuilder {
    let all = serde_json::Value::Array(
        look.emotes.iter().map(|(n, id, _)| serde_json::json!([n, [id]])).collect(),
    );
    let equipped = serde_json::Value::Array(
        look.emotes.iter().filter(|e| e.2 > 0).map(|(n, _, slot)| serde_json::json!([slot, n])).collect(),
    );
    InstanceBuilder::new("HumanoidDescription")
        .with_name("HumanoidDescription")
        .with_property("EmotesDataInternal", Variant::String(all.to_string()))
        .with_property("EquippedEmotesDataInternal", Variant::String(equipped.to_string()))
}

/// An avatar description from any of the avatar endpoints' JSON (BrickColor
/// ids win over hex colours when both are given, as before).
pub fn info_from_json(user_id: u64, json: &serde_json::Value) -> AvatarInfo {
    let mut info: AvatarInfo = parse_avatar(user_id, json);
    if let Some(c) = v1_colors(json) {
        info.colors = c;
    }
    info
}

/// Downloads a description's worn items into a look (worker thread).
pub fn look_from_info(
    client: &SharedClient,
    user_id: u64,
    info: &AvatarInfo,
    log: &mut dyn FnMut(String),
) -> Result<AvatarLook, String> {
    let c = info.colors;
    let mut look = AvatarLook {
        user_id,
        r15: info.r15,
        colors: BodyColors {
            head: c[0],
            torso: c[1],
            left_arm: c[2],
            right_arm: c[3],
            left_leg: c[4],
            right_leg: c[5],
        },
        scales: Scales {
            height: info.height,
            width: info.width,
            depth: info.depth,
            head: info.head,
            body_type: info.body_type,
            proportion: info.proportion,
        },
        proportion: info.proportion,
        body_type: info.body_type,
        shirt: None,
        pants: None,
        tshirt: None,
        face: None,
        accessories: Vec::new(),
        body_parts: Vec::new(),
        head: None,
        animations: Vec::new(),
        emotes: Vec::new(),
        skipped: Vec::new(),
    };
    for a in &info.assets {
        let kind = asset_type_name(&a.type_name, a.type_id);
        // Layered clothing (Jacket, Sweater, Shorts, DressSkirt, TShirt /
        // Shirt / Pants accessories, shoes) is an Accessory with a WrapLayer:
        // wrap.rs fits it onto R15 bodies.
        let wanted = matches!(
            kind.as_str(),
            "Shirt" | "Pants" | "TShirt" | "Face" | "Hat" | "Head" | "DynamicHead"
        ) || is_body_part(&kind)
            || kind.ends_with("Accessory")
            || is_animation_pack(&kind);
        if !wanted {
            look.skipped.push(format!("{} ({kind})", a.name));
            continue;
        }
        let bytes = {
            let mut guard = client.lock().map_err(|_| "account busy".to_string())?;
            let c = guard.as_mut().ok_or("signed out")?;
            match c.download_asset(a.id, None) {
                Ok(b) => b,
                Err(e) => {
                    log(format!(
                        "Avatar item {} ({kind}) not downloaded: {e}",
                        a.name
                    ));
                    continue;
                }
            }
        };
        let dom = match decode_model(&bytes) {
            Ok(d) => d,
            Err(e) => {
                log(format!(
                    "Avatar item {} ({kind}) could not be read: {e}",
                    a.name
                ));
                continue;
            }
        };
        match kind.as_str() {
            "Shirt" => look.shirt = content_of(&dom, "Shirt", "ShirtTemplate"),
            "Pants" => look.pants = content_of(&dom, "Pants", "PantsTemplate"),
            "TShirt" => look.tshirt = content_of(&dom, "ShirtGraphic", "Graphic"),
            "Face" => look.face = content_of(&dom, "Decal", "Texture"),
            "Head" | "DynamicHead" => look.head = Some(head_for_rig(client, a.id, look.r15, bytes)),
            k if is_body_part(k) => look.body_parts.push(bytes),
            k if is_animation_pack(k) => look.animations.push((kind.clone(), a.id, bytes)),
            _ => look.accessories.push(bytes),
        }
    }
    if !look.skipped.is_empty() {
        log(format!("Not shown yet: {}", look.skipped.join(", ")));
    }
    Ok(look)
}

impl Editor {
    /// Puts the avatar's clothing, face and accessories on a new character
    /// (server DataModel; they replicate like any other instances).
    pub fn dress_character(&mut self, model: Ref) {
        let Some(look) = self.account.avatar().cloned() else {
            return;
        };
        dress(&mut self.doc, model, &look);
    }
}

/// Puts a look's clothing, face and accessories on a character model.
pub fn dress(doc: &mut doc::Document, model: Ref, look: &AvatarLook) {
    let no_face = put_on_body(doc, model, look);
    {
        let add = |doc: &mut doc::Document, class: &str, prop: &str, id: &Option<String>| {
            if let Some(id) = id {
                doc.dom.insert(
                    model,
                    InstanceBuilder::new(class)
                        .with_name(class)
                        .with_property(prop, ContentId::from(id.clone())),
                );
            }
        };
        add(doc, "Shirt", "ShirtTemplate", &look.shirt);
        add(doc, "Pants", "PantsTemplate", &look.pants);
        add(doc, "ShirtGraphic", "Graphic", &look.tshirt);
        let face = look.face.as_ref().filter(|_| !no_face);
        if let (Some(face), Some(head)) = (face, doc.find_named(model, "Head")) {
            // A face the rig already has (Roblox's default) is replaced.
            if let Some(old) = doc.find_named(head, "face") {
                doc.dom.destroy(old);
            }
            doc.dom.insert(
                head,
                InstanceBuilder::new("Decal")
                    .with_name("face")
                    .with_property("Texture", Content::from(face.clone()))
                    .with_property("Face", Enum::from_u32(5)),
            );
        }
        for bytes in &look.accessories {
            let Ok(mut tmp) = decode_model(bytes) else {
                continue;
            };
            let roots: Vec<Ref> = tmp.root().children().to_vec();
            for r in roots {
                let is_acc = tmp
                    .get_by_ref(r)
                    .is_some_and(|i| doc::is_a(&i.class, "Accoutrement"));
                if !is_acc {
                    continue;
                }
                // Handles ride along with the body; they don't collide.
                let parts: Vec<Ref> = tmp
                    .descendants_of(r)
                    .filter(|i| doc::is_a(&i.class, "BasePart"))
                    .map(|i| i.referent())
                    .collect();
                for p in parts {
                    if let Some(i) = tmp.get_by_ref_mut(p) {
                        i.properties.insert(ustr("Anchored"), Variant::Bool(false));
                        i.properties
                            .insert(ustr("CanCollide"), Variant::Bool(false));
                    }
                }
                tmp.transfer(r, &mut doc.dom, model);
                weld_accessory(&mut doc.dom, model, r);
            }
        }
        doc.mark_tree(model);
    }
}

/// Whether the place lets players bring their own animation packs, as Game
/// Settings → Avatar → Animation sets it: AvatarSettings' animation rules
/// (AnimationPacksMode PlayerChoice 0, StandardR15 1, StandardR6 2) and the
/// legacy StarterPlayer.AllowCustomAnimations ("Standard" = false).
pub fn place_allows_animation_packs(doc: &doc::Document) -> bool {
    let root = doc.dom.root_ref();
    if let Some(sp) = doc.find_child(root, "StarterPlayer") {
        if matches!(doc.prop(sp, "AllowCustomAnimations"), Some(Variant::Bool(false))) {
            return false;
        }
    }
    let rules = doc.find_child(root, "AvatarSettings").and_then(|s| {
        doc.dom
            .descendants_of(s)
            .find(|i| i.class == "AvatarAnimationRules")
            .map(|i| i.referent())
    });
    match rules {
        Some(r) => crate::scene::enum_u32(doc.prop(r, "AnimationPacksMode")).unwrap_or(0) == 0,
        None => true,
    }
}

/// Puts the avatar's animation packages into the character's Animate
/// script, as Roblox's HumanoidDescription apply does: each package
/// StringValue (idle, walk, run, jump, fall, climb, swim, swimidle, mood)
/// replaces the children of the Animate StringValue of the same name with
/// its own Animations (and their Weight values). Slots the Animate script
/// doesn't have are left out, so a place's own Animate keeps its shape.
/// Returns the slots changed.
pub fn put_on_animations(doc: &mut doc::Document, model: Ref, look: &AvatarLook) -> Vec<String> {
    put_on_animation_packs(doc, model, &look.animations)
}

/// [`put_on_animations`] for any list of (AssetType name, id, .rbxm) packages.
pub fn put_on_animation_packs(doc: &mut doc::Document, model: Ref, packs: &[(String, u64, Vec<u8>)]) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(animate) = child_named(doc, model, "Animate") else {
        return changed;
    };
    for (_, _, bytes) in packs {
        let Ok(mut pkg) = decode_model(bytes) else {
            continue;
        };
        let slots: Vec<(Ref, String)> = pkg
            .descendants()
            .filter(|i| i.class == "StringValue")
            .map(|i| (i.referent(), i.name.to_string()))
            .collect();
        for (sv, slot) in slots {
            let Some(dst) = child_named(doc, animate, &slot)
                .filter(|r| doc.dom.get_by_ref(*r).is_some_and(|i| i.class == "StringValue"))
            else {
                continue;
            };
            let anims: Vec<Ref> = pkg
                .get_by_ref(sv)
                .map(|i| i.children().to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|c| pkg.get_by_ref(*c).is_some_and(|i| i.class == "Animation"))
                .collect();
            if anims.is_empty() {
                continue;
            }
            let old: Vec<Ref> = doc
                .dom
                .get_by_ref(dst)
                .map(|i| i.children().to_vec())
                .unwrap_or_default();
            for o in old {
                doc.dom.destroy(o);
            }
            for a in anims {
                pkg.transfer(a, &mut doc.dom, dst);
            }
            changed.push(slot);
        }
    }
    if !changed.is_empty() {
        doc.mark_tree(animate);
    }
    changed
}

/// A game's CustomClips: each listed Animate slot holds just these
/// animations (several idles share the slot, the first weighted 9 and the
/// alternates 1, like Animate's own idle). Returns the slots changed.
pub fn put_on_custom_clips(doc: &mut doc::Document, model: Ref, clips: &[(&str, Vec<u64>)]) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(animate) = child_named(doc, model, "Animate") else {
        return changed;
    };
    for (slot, ids) in clips {
        let Some(sv) = child_named(doc, animate, slot) else { continue };
        let old: Vec<Ref> = doc.dom.get_by_ref(sv).map(|i| i.children().to_vec()).unwrap_or_default();
        // Keep the slot's own Animation names (WalkAnim, Animation1 …).
        let names: Vec<String> = old
            .iter()
            .filter_map(|o| doc.dom.get_by_ref(*o).filter(|i| i.class == "Animation").map(|i| i.name.to_string()))
            .collect();
        for o in old {
            doc.dom.destroy(o);
        }
        for (n, id) in ids.iter().enumerate() {
            let name = names.get(n).cloned().unwrap_or_else(|| format!("Animation{}", n + 1));
            let mut b = InstanceBuilder::new("Animation")
                .with_name(name)
                .with_property("AnimationId", ContentId::from(format!("rbxassetid://{id}")));
            if *slot == "idle" {
                b = b.with_child(
                    InstanceBuilder::new("NumberValue")
                        .with_name("Weight")
                        .with_property("Value", if n == 0 { 9.0f64 } else { 1.0f64 }),
                );
            }
            doc.dom.insert(sv, b);
        }
        changed.push(slot.to_string());
    }
    if !changed.is_empty() {
        doc.mark_tree(animate);
    }
    changed
}

/// Welds an accessory to the body as Roblox's Humanoid:AddAccessory does:
/// a Weld "AccessoryWeld" in the Handle (Part0 Handle, Part1 the body part,
/// C0 / C1 the matching attachments' CFrames), the Handle moved into
/// place. Models made by CreateHumanoidModelFromDescription carry these,
/// and display copies of them (Place A's lobby crew) are posed from their
/// joints alone. Legacy hats with no matching attachment go on the Head by
/// AttachmentPoint. A Handle that already has an AccessoryWeld is left be.
pub fn weld_accessory(dom: &mut WeakDom, model: Ref, acc: Ref) {
    use crate::math::Xform;
    let ident = Xform {
        pos: glam::Vec3::ZERO,
        rot: glam::Mat3::IDENTITY,
    };
    let compose = |a: &Xform, b: &Xform| Xform {
        pos: a.apply(b.pos),
        rot: a.rot * b.rot,
    };
    let kids = |dom: &WeakDom, r: Ref| -> Vec<Ref> {
        dom.get_by_ref(r).map(|i| i.children().to_vec()).unwrap_or_default()
    };
    let named = |dom: &WeakDom, r: Ref, name: &str| {
        kids(dom, r)
            .into_iter()
            .find(|c| dom.get_by_ref(*c).is_some_and(|i| i.name == name))
    };
    let cf = |dom: &WeakDom, r: Ref, k: &str| match dom.get_by_ref(r).and_then(|i| i.properties.get(&ustr(k))) {
        Some(Variant::CFrame(c)) => Xform::from_cframe(c),
        _ => ident,
    };
    let Some(handle) = named(dom, acc, "Handle")
        .filter(|h| dom.get_by_ref(*h).is_some_and(|i| doc::is_a(&i.class, "BasePart")))
    else {
        return;
    };
    if named(dom, handle, "AccessoryWeld").is_some() {
        return;
    }
    let body: Vec<Ref> = kids(dom, model)
        .into_iter()
        .filter(|c| dom.get_by_ref(*c).is_some_and(|i| doc::is_a(&i.class, "BasePart")))
        .collect();
    let att_named = |dom: &WeakDom, part: Ref, name: &str| {
        kids(dom, part).into_iter().find(|c| {
            dom.get_by_ref(*c)
                .is_some_and(|i| i.class == "Attachment" && i.name == name)
        })
    };
    let found = kids(dom, handle)
        .into_iter()
        .filter(|a| dom.get_by_ref(*a).is_some_and(|i| i.class == "Attachment"))
        .find_map(|a| {
            let name = dom.get_by_ref(a)?.name.clone();
            body.iter()
                .find_map(|p| att_named(dom, *p, &name).map(|b| (*p, cf(dom, a, "CFrame"), cf(dom, b, "CFrame"))))
        });
    let found = found.or_else(|| {
        let head = body
            .iter()
            .copied()
            .find(|p| dom.get_by_ref(*p).is_some_and(|i| i.name == "Head"))?;
        let half = match dom.get_by_ref(head)?.properties.get(&ustr("Size")) {
            Some(Variant::Vector3(s)) => s.y / 2.0,
            _ => 0.5,
        };
        let c1 = Xform {
            pos: glam::Vec3::Y * half,
            rot: glam::Mat3::IDENTITY,
        };
        Some((head, cf(dom, acc, "AttachmentPoint"), c1))
    });
    let Some((part, c0, c1)) = found else {
        return;
    };
    let part_cf = cf(dom, part, "CFrame");
    let world = compose(&compose(&part_cf, &c1), &c0.inverse());
    if let Some(i) = dom.get_by_ref_mut(handle) {
        i.properties
            .insert(ustr("CFrame"), Variant::CFrame(world.to_cframe()));
    }
    dom.insert(
        handle,
        InstanceBuilder::new("Weld")
            .with_name("AccessoryWeld")
            .with_property("Part0", Variant::Ref(handle))
            .with_property("Part1", Variant::Ref(part))
            .with_property("C0", Variant::CFrame(c0.to_cframe()))
            .with_property("C1", Variant::CFrame(c1.to_cframe())),
    );
}

/// A direct child by name.
fn child_named(doc: &doc::Document, parent: Ref, name: &str) -> Option<Ref> {
    doc.dom
        .get_by_ref(parent)?
        .children()
        .iter()
        .copied()
        .find(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.name == name))
}

/// The head package for the avatar's rig. A head asset as served plainly is
/// its legacy R6 form (a SpecialMesh, "NoFace" on a dynamic head); R15
/// characters wear its MeshPart head, which asset delivery hands out for
/// `assetFormat: avatar_meshpart_head` (as Studio's avatar loader asks):
/// a MeshPart "Head" with its mesh, texture, FaceControls and cage. Falls
/// back to the legacy bytes when that form isn't available.
fn head_for_rig(client: &SharedClient, id: u64, r15: bool, legacy: Vec<u8>) -> Vec<u8> {
    if !r15 {
        return legacy;
    }
    let cache = crate::assets::cache_dir().join(format!("assets/{id}_avatar_meshpart_head.bin"));
    if let Ok(b) = std::fs::read(&cache) {
        if !b.is_empty() {
            return b;
        }
    }
    let fetched = (|| {
        let guard = client.lock().ok()?;
        let s = &guard.as_ref()?.session;
        let body = serde_json::json!([{
            "assetId": id, "requestId": "0", "assetFormat": "avatar_meshpart_head"
        }])
        .to_string();
        let (status, text) = s
            .post_csrf(
                "https://assetdelivery.roblox.com/v1/assets/batch",
                "application/json",
                body.as_bytes(),
            )
            .ok()?;
        if status != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let loc = v.pointer("/0/location")?.as_str()?.to_string();
        let (status, bytes) = s.fetch_bytes(&loc, false).ok()?;
        (status == 200 && decode_model(&bytes).is_ok()).then_some(bytes)
    })();
    match fetched {
        Some(b) => {
            if let Some(dir) = cache.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&cache, &b);
            b
        }
        None => legacy,
    }
}

/// A MeshPart head package (the `avatar_meshpart_head` form) onto an R15
/// character's Head: its mesh, texture, size, attachments and cage, and its
/// FaceControls. True when the head paints its own face (a dynamic head,
/// one with FaceControls), so no classic face decal goes on it.
fn put_on_r15_head(
    doc: &mut doc::Document,
    pkg: &mut WeakDom,
    src: Ref,
    head: Ref,
    look: &AvatarLook,
) -> bool {
    put_on_r15_part(doc, pkg, src, head, look);
    let face_controls: Option<Ref> = pkg.get_by_ref(src).and_then(|i| {
        i.children()
            .iter()
            .copied()
            .find(|c| pkg.get_by_ref(*c).is_some_and(|i| i.class == "FaceControls"))
    });
    // The head's own children that give way: the old FaceControls, and a
    // face decal when the new head has its own face.
    let old: Vec<Ref> = doc
        .dom
        .get_by_ref(head)
        .map(|h| h.children().to_vec())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| {
            doc.dom.get_by_ref(*c).is_some_and(|i| {
                i.class == "FaceControls"
                    || (face_controls.is_some() && i.class == "Decal" && i.name == "face")
            })
        })
        .collect();
    for o in old {
        doc.dom.destroy(o);
    }
    if let Some(fc) = face_controls {
        pkg.transfer(fc, &mut doc.dom, head);
    }
    face_controls.is_some()
}

/// Swaps the character's body parts for the avatar's body part packages
/// and head, as Roblox does: R6 gets each package's CharacterMesh (and the
/// head's SpecialMesh); R15 parts take the package MeshPart's mesh, size,
/// attachments and cage, then the joints are re-seated. True when the head
/// carries no face of its own (a dynamic head's "NoFace" tag).
fn put_on_body(doc: &mut doc::Document, model: Ref, look: &AvatarLook) -> bool {
    let r6 = child_named(doc, model, "Torso").is_some();
    let r15 = child_named(doc, model, "UpperTorso").is_some();
    let mut no_face = false;
    let mut changed_r15 = false;
    let packages = look.body_parts.iter().chain(look.head.iter());
    for bytes in packages {
        let Ok(mut pkg) = decode_model(bytes) else {
            continue;
        };
        let root = pkg.root_ref();
        let folder = |pkg: &WeakDom, name: &str| {
            pkg.get_by_ref(root)?
                .children()
                .iter()
                .copied()
                .find(|c| pkg.get_by_ref(*c).is_some_and(|i| i.name == name && i.class == "Folder"))
        };
        if r6 {
            // Body parts: the R6 folder's CharacterMeshes, replacing any
            // the character has for the same BodyPart.
            if let Some(f) = folder(&pkg, "R6") {
                let meshes: Vec<Ref> = pkg
                    .get_by_ref(f)
                    .map(|i| i.children().to_vec())
                    .unwrap_or_default();
                for cm in meshes {
                    let Some(part) = pkg
                        .get_by_ref(cm)
                        .filter(|i| i.class == "CharacterMesh")
                        .map(|i| i.properties.get(&ustr("BodyPart")).cloned())
                    else {
                        continue;
                    };
                    let old: Vec<Ref> = doc
                        .dom
                        .get_by_ref(model)
                        .map(|m| m.children().to_vec())
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|c| {
                            doc.dom.get_by_ref(*c).is_some_and(|i| {
                                i.class == "CharacterMesh"
                                    && i.properties.get(&ustr("BodyPart")).cloned() == part
                            })
                        })
                        .collect();
                    for o in old {
                        doc.dom.destroy(o);
                    }
                    pkg.transfer(cm, &mut doc.dom, model);
                }
            }
            // A head: its SpecialMesh replaces the Head's mesh.
            let special = pkg.get_by_ref(root).and_then(|i| {
                i.children()
                    .iter()
                    .copied()
                    .find(|c| pkg.get_by_ref(*c).is_some_and(|i| i.class == "SpecialMesh"))
            });
            if let (Some(sm), Some(head)) = (special, child_named(doc, model, "Head")) {
                let old: Vec<Ref> = doc
                    .dom
                    .get_by_ref(head)
                    .map(|h| h.children().to_vec())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| {
                        doc.dom
                            .get_by_ref(*c)
                            .is_some_and(|i| doc::is_a(&i.class, "DataModelMesh"))
                    })
                    .collect();
                for o in old {
                    doc.dom.destroy(o);
                }
                let tagged_no_face = pkg.get_by_ref(sm).is_some_and(|i| {
                    matches!(i.properties.get(&ustr("Tags")),
                        Some(Variant::Tags(t)) if t.iter().any(|t| t == "NoFace"))
                });
                if tagged_no_face {
                    no_face = true;
                    if let Some(face) = child_named(doc, head, "face") {
                        doc.dom.destroy(face);
                    }
                }
                pkg.transfer(sm, &mut doc.dom, head);
            }
        }
        if r15 {
            // A MeshPart head package: a "Head" MeshPart at the root.
            let mesh_head = pkg.get_by_ref(root).and_then(|i| {
                i.children().iter().copied().find(|c| {
                    pkg.get_by_ref(*c)
                        .is_some_and(|i| i.class == "MeshPart" && i.name == "Head")
                })
            });
            if let (Some(src), Some(head)) = (mesh_head, child_named(doc, model, "Head")) {
                no_face |= put_on_r15_head(doc, &mut pkg, src, head, look);
                changed_r15 = true;
                continue;
            }
            // Studio's order (avatar_apply.luau, measured): artist intent first.
            let Some(f) = ["R15ArtistIntent", "R15Fixed", "R15"]
                .iter()
                .find_map(|n| folder(&pkg, n))
            else {
                continue;
            };
            let parts: Vec<Ref> = pkg
                .get_by_ref(f)
                .map(|i| i.children().to_vec())
                .unwrap_or_default();
            for p in parts {
                let Some(src) = pkg.get_by_ref(p).filter(|i| i.class == "MeshPart") else {
                    continue;
                };
                let Some(dst) = child_named(doc, model, &src.name) else {
                    continue;
                };
                put_on_r15_part(doc, &mut pkg, p, dst, look);
                changed_r15 = true;
            }
        }
    }
    // Parts the packages didn't replace are Classic: the character was
    // built with the body type's proportions already (rig::apply_r15_scales,
    // from look.scales).
    if changed_r15 {
        crate::rig::reseat_r15_joints(&mut doc.dom, model);
    }
    no_face
}

use crate::rig::{part_kind, proportion_target, PROPORTIONS_NORMAL as NORMAL, PROPORTIONS_SLENDER as SLENDER};

/// A package part's size factor: the target proportions over the part's
/// own scale type's, times width / height / depth (head scale for the head).
fn part_factor(name: &str, scale_type: &str, look: &AvatarLook) -> glam::Vec3 {
    let target = proportion_target(name, look.proportion, look.body_type);
    let native = match (part_kind(name), scale_type) {
        (Some(k), "ProportionsNormal") => glam::Vec3::from(NORMAL[k]),
        (Some(k), "ProportionsSlender") => glam::Vec3::from(SLENDER[k]),
        _ => glam::Vec3::ONE,
    };
    let s = &look.scales;
    let hwd = if name == "Head" { glam::Vec3::splat(s.head) } else { glam::Vec3::new(s.width, s.height, s.depth) };
    target / native * hwd
}

/// One R15 package MeshPart onto the character's part of the same name.
fn put_on_r15_part(
    doc: &mut doc::Document,
    pkg: &mut WeakDom,
    src: Ref,
    dst: Ref,
    look: &AvatarLook,
) {
    use glam::Vec3;
    let Some(s) = pkg.get_by_ref(src) else { return };
    let name = s.name.to_string();
    // The package's own scale type: its sizes are native to it.
    let scale_type = s
        .children()
        .iter()
        .find_map(|c| {
            let i = pkg.get_by_ref(*c)?;
            (i.name == "AvatarPartScaleType").then(|| match i.properties.get(&ustr("Value")) {
                Some(Variant::String(v)) => v.clone(),
                _ => String::new(),
            })
        })
        .unwrap_or_default();
    let factor = part_factor(&name, &scale_type, look);
    let scale = |v: rbx_dom_weak::types::Vector3| {
        let v = Vec3::new(v.x, v.y, v.z) * factor;
        rbx_dom_weak::types::Vector3::new(v.x, v.y, v.z)
    };
    let native_size = match s.properties.get(&ustr("Size")) {
        Some(Variant::Vector3(v)) => Some(*v),
        _ => None,
    };
    let props: Vec<(&str, Option<Variant>)> = [
        "MeshId",
        "MeshContent",
        "TextureID",
        "TextureContent",
        "InitialSize",
    ]
    .iter()
    .map(|k| (*k, s.properties.get(&ustr(k)).cloned()))
    .collect();
    let size = match s.properties.get(&ustr("Size")) {
        Some(Variant::Vector3(v)) => Some(scale(*v)),
        _ => None,
    };
    let kids: Vec<Ref> = s.children().to_vec();
    if let Some(d) = doc.dom.get_by_ref_mut(dst) {
        for (k, v) in props {
            match v {
                Some(v) => d.properties.insert(ustr(k), v),
                None => d.properties.remove(&ustr(k)),
            };
        }
        if let Some(size) = size {
            d.properties.insert(ustr("Size"), Variant::Vector3(size));
        }
    }
    // OriginalSize / OriginalPosition keep the unscaled asset values (what
    // the scale values multiply), and the part keeps its scale type.
    if let (Some(native), Some(o)) = (native_size, child_named(doc, dst, "OriginalSize")) {
        if let Some(i) = doc.dom.get_by_ref_mut(o) {
            i.properties.insert(ustr("Value"), Variant::Vector3(native));
        }
    }
    if let Some(old) = child_named(doc, dst, "AvatarPartScaleType") {
        doc.dom.destroy(old);
    }
    if !scale_type.is_empty() {
        doc.dom.insert(
            dst,
            InstanceBuilder::new("StringValue")
                .with_name("AvatarPartScaleType")
                .with_property("Value", Variant::String(scale_type.clone())),
        );
    }
    for k in kids {
        let Some(ki) = pkg.get_by_ref(k) else { continue };
        match ki.class.as_str() {
            // The package's attachment frames (rig joints, accessories).
            "Attachment" => {
                let Some(Variant::CFrame(mut cf)) = ki.properties.get(&ustr("CFrame")).cloned()
                else {
                    continue;
                };
                let native_pos = cf.position;
                cf.position = scale(cf.position);
                let Some(a) = child_named(doc, dst, &ki.name) else {
                    pkg.transfer(k, &mut doc.dom, dst);
                    if let Some(i) = doc.dom.get_by_ref_mut(k) {
                        i.properties.insert(ustr("CFrame"), Variant::CFrame(cf));
                    }
                    continue;
                };
                if let Some(i) = doc.dom.get_by_ref_mut(a) {
                    i.properties.insert(ustr("CFrame"), Variant::CFrame(cf));
                }
                if let Some(o) = child_named(doc, a, "OriginalPosition") {
                    if let Some(i) = doc.dom.get_by_ref_mut(o) {
                        i.properties.insert(ustr("Value"), Variant::Vector3(native_pos));
                    }
                }
            }
            // The cage layered clothing fits to.
            "WrapTarget" => {
                let old: Vec<Ref> = doc
                    .dom
                    .get_by_ref(dst)
                    .map(|i| i.children().to_vec())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|c| doc.dom.get_by_ref(*c).is_some_and(|i| i.class == "WrapTarget"))
                    .collect();
                for o in old {
                    doc.dom.destroy(o);
                }
                pkg.transfer(k, &mut doc.dom, dst);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_clothing_template_from_a_model() {
        let dom = WeakDom::new(
            InstanceBuilder::new("DataModel").with_child(
                InstanceBuilder::new("Shirt")
                    .with_name("Shirt")
                    .with_property(
                        "ShirtTemplate",
                        ContentId::from("http://www.roblox.com/asset/?id=123"),
                    ),
            ),
        );
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, &dom, dom.root().children()).unwrap();
        let back = decode_model(&bytes).unwrap();
        assert_eq!(
            content_of(&back, "Shirt", "ShirtTemplate").as_deref(),
            Some("http://www.roblox.com/asset/?id=123")
        );
    }

    #[test]
    fn animation_pack_replaces_animate_slot() {
        // A package as the catalog serves it: R15Anim/idle/Animation1+2.
        let pkg = WeakDom::new(
            InstanceBuilder::new("DataModel").with_child(
                InstanceBuilder::new("Folder").with_name("R15Anim").with_child(
                    InstanceBuilder::new("StringValue")
                        .with_name("idle")
                        .with_child(
                            InstanceBuilder::new("Animation")
                                .with_name("Animation1")
                                .with_property("AnimationId", ContentId::from("rbxassetid://11"))
                                .with_child(
                                    InstanceBuilder::new("NumberValue")
                                        .with_name("Weight")
                                        .with_property("Value", 9.0f64),
                                ),
                        )
                        .with_child(
                            InstanceBuilder::new("Animation")
                                .with_name("Animation2")
                                .with_property("AnimationId", ContentId::from("rbxassetid://12")),
                        ),
                ),
            ),
        );
        let mut bytes = Vec::new();
        rbx_binary::to_writer(&mut bytes, &pkg, pkg.root().children()).unwrap();
        let mut d = doc::Document::empty();
        let root = d.dom.root_ref();
        let ch = d.dom.insert(root, InstanceBuilder::new("Model").with_name("Player"));
        let animate = d.dom.insert(ch, InstanceBuilder::new("LocalScript").with_name("Animate"));
        let idle = d.dom.insert(animate, InstanceBuilder::new("StringValue").with_name("idle"));
        d.dom.insert(idle, InstanceBuilder::new("Animation").with_name("Animation1"));
        let walk = d.dom.insert(animate, InstanceBuilder::new("StringValue").with_name("walk"));
        d.dom.insert(walk, InstanceBuilder::new("Animation").with_name("WalkAnim"));
        let look = AvatarLook {
            user_id: 1,
            r15: true,
            colors: BodyColors::default(),
            scales: Scales::default(),
            proportion: 0.0,
            body_type: 0.0,
            shirt: None,
            pants: None,
            tshirt: None,
            face: None,
            accessories: Vec::new(),
            body_parts: Vec::new(),
            head: None,
            animations: vec![("IdleAnimation".into(), 5, bytes)],
            emotes: Vec::new(),
            skipped: Vec::new(),
        };
        assert_eq!(put_on_animations(&mut d, ch, &look), vec!["idle".to_string()]);
        let names = |r: Ref| -> Vec<String> {
            d.dom.get_by_ref(r).unwrap().children().iter().map(|c| d.dom.get_by_ref(*c).unwrap().name.to_string()).collect()
        };
        assert_eq!(names(idle), vec!["Animation1", "Animation2"]);
        assert_eq!(names(walk), vec!["WalkAnim"]);
        let a1 = d.dom.get_by_ref(idle).unwrap().children()[0];
        assert_eq!(names(a1), vec!["Weight"]);
    }

}
