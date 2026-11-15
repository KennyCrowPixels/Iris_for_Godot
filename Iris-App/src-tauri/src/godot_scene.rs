//! Structural editing of Godot 4 text scenes (.tscn) for the agent's godot_scene tool. Lines an
//! edit does not touch are kept byte-for-byte, so properties Iris does not understand survive.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

const SECTION_KINDS: [&str; 7] = ["gd_scene", "gd_resource", "ext_resource", "sub_resource", "node", "connection", "editable"];

#[derive(Clone, Debug)]
pub struct Section {
    pub header: String,
    pub body: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NodeInfo {
    pub index: usize,
    pub name: String,
    pub parent: Option<String>,
    pub path: String,
    pub type_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Scene {
    preamble: Vec<String>,
    pub sections: Vec<Section>,
    crlf: bool,
}

fn section_kind(header: &str) -> Option<&'static str> {
    let rest = header.trim_end_matches('\r').strip_prefix('[')?;
    let word = rest.split([' ', ']']).next()?;
    SECTION_KINDS.iter().copied().find(|kind| *kind == word)
}

/// Whether a scan position is inside a string literal or an unfinished bracketed value, so a
/// multi-line value (a built-in script, a mesh array) is never mistaken for a header or property.
#[derive(Default, Clone, Copy)]
struct ValueState {
    depth: i32,
    in_string: bool,
}

impl ValueState {
    fn clean(&self) -> bool {
        self.depth <= 0 && !self.in_string
    }

    fn scan(&mut self, line: &str) {
        let mut escaped = false;
        for character in line.chars() {
            if self.in_string {
                if escaped { escaped = false; } else if character == '\\' { escaped = true; } else if character == '"' { self.in_string = false; }
            } else {
                match character {
                    '"' => self.in_string = true,
                    '(' | '[' | '{' => self.depth += 1,
                    ')' | ']' | '}' => self.depth -= 1,
                    _ => {}
                }
            }
        }
    }
}

/// key=value pairs of a section header; values stay raw (quoted strings keep their quotes).
fn header_attrs(header: &str) -> Vec<(String, String)> {
    let inner = header.trim_end_matches('\r').trim();
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner);
    let chars: Vec<char> = inner.chars().collect();
    let mut index = chars.iter().position(|character| character.is_whitespace()).unwrap_or(chars.len());
    let mut attrs = Vec::new();
    while index < chars.len() {
        while index < chars.len() && chars[index].is_whitespace() { index += 1; }
        let key_start = index;
        while index < chars.len() && chars[index] != '=' && !chars[index].is_whitespace() { index += 1; }
        let key: String = chars[key_start..index].iter().collect();
        if index >= chars.len() || chars[index] != '=' {
            if !key.is_empty() { attrs.push((key, String::new())); }
            continue;
        }
        index += 1;
        let value_start = index;
        let mut state = ValueState::default();
        let mut escaped = false;
        while index < chars.len() {
            let character = chars[index];
            if state.in_string {
                if escaped { escaped = false; } else if character == '\\' { escaped = true; } else if character == '"' { state.in_string = false; }
            } else if character == '"' {
                state.in_string = true;
            } else if "([{".contains(character) {
                state.depth += 1;
            } else if ")]}".contains(character) {
                state.depth -= 1;
            } else if character.is_whitespace() && state.depth <= 0 {
                break;
            }
            index += 1;
        }
        attrs.push((key, chars[value_start..index].iter().collect()));
    }
    attrs
}

fn unquote(raw: &str) -> String {
    match raw.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
        Some(inner) => inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => raw.to_string(),
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn attr(header: &str, key: &str) -> Option<String> {
    header_attrs(header).into_iter().find(|(name, _)| name == key).map(|(_, value)| unquote(&value))
}

fn build_header(kind: &str, attrs: &[(String, String)], crlf: bool) -> String {
    let mut header = format!("[{kind}");
    for (key, value) in attrs {
        header.push(' ');
        header.push_str(key);
        if !value.is_empty() {
            header.push('=');
            header.push_str(value);
        }
    }
    header.push(']');
    if crlf { header.push('\r'); }
    header
}

fn with_attr(header: &str, key: &str, raw: &str) -> String {
    let mut attrs = header_attrs(header);
    match attrs.iter_mut().find(|(name, _)| name == key) {
        Some(entry) => entry.1 = raw.to_string(),
        None => attrs.push((key.to_string(), raw.to_string())),
    }
    build_header(section_kind(header).unwrap_or("node"), &attrs, header.ends_with('\r'))
}

/// (key, first line, end line exclusive) of each property in a section body.
fn properties(body: &[String]) -> Vec<(String, usize, usize)> {
    let mut found: Vec<(String, usize, usize)> = Vec::new();
    let mut state = ValueState::default();
    for (index, line) in body.iter().enumerate() {
        if state.clean() {
            if let Some((key, _)) = line.split_once(" = ") {
                if !key.is_empty() && key.chars().all(|character| character.is_ascii_alphanumeric() || "_/:.-".contains(character)) {
                    found.push((key.to_string(), index, index + 1));
                }
            }
        } else if let Some(last) = found.last_mut() {
            last.2 = index + 1;
        }
        state.scan(line);
    }
    found
}

impl Section {
    fn property(&self, key: &str) -> Option<String> {
        let (_, start, end) = properties(&self.body).into_iter().find(|(name, _, _)| name == key)?;
        let joined = self.body[start..end].iter().map(|line| line.trim_end_matches('\r')).collect::<Vec<_>>().join("\n");
        joined.split_once(" = ").map(|(_, value)| value.to_string())
    }

    fn set_property(&mut self, key: &str, raw: &str, crlf: bool) {
        let line = format!("{key} = {raw}{}", if crlf { "\r" } else { "" });
        let props = properties(&self.body);
        if let Some((_, start, end)) = props.iter().find(|(name, _, _)| name == key) {
            self.body.splice(*start..*end, [line]);
            return;
        }
        let at = props.last().map(|(_, _, end)| *end).unwrap_or_else(|| {
            let mut at = self.body.len();
            while at > 0 && self.body[at - 1].trim().is_empty() { at -= 1; }
            at
        });
        self.body.insert(at, line);
    }

    fn remove_property(&mut self, key: &str) {
        if let Some((_, start, end)) = properties(&self.body).into_iter().find(|(name, _, _)| name == key) {
            self.body.drain(start..end);
        }
    }
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn random_suffix() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos() as u64).unwrap_or(0);
    let mut seed = nanos ^ ID_COUNTER.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    (0..5).map(|_| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        char::from_digit(((seed >> 33) % 36) as u32, 36).unwrap_or('a')
    }).collect()
}

/// (call, id) pairs such as ("SubResource", "SphereMesh_1") referenced in some text.
fn resource_refs(text: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for call in ["SubResource", "ExtResource"] {
        let opener = format!("{call}(\"");
        let mut rest = text;
        while let Some(start) = rest.find(&opener) {
            rest = &rest[start + opener.len()..];
            if let Some(end) = rest.find('"') {
                found.push((call.to_string(), rest[..end].to_string()));
                rest = &rest[end..];
            }
        }
    }
    found
}

fn call_id(raw: &str, call: &str) -> Option<String> {
    resource_refs(raw).into_iter().find(|(kind, _)| kind == call).map(|(_, id)| id)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.trim() != name || name.chars().any(|character| ".:@/\"%\\\r\n".contains(character)) {
        return Err(format!("'{name}' is not a valid node name (no . : @ / % or quotes)."));
    }
    Ok(())
}

fn validate_type(type_name: &str) -> Result<(), String> {
    let valid = type_name.chars().next().is_some_and(|first| first.is_ascii_uppercase())
        && type_name.chars().all(|character| character.is_ascii_alphanumeric() || character == '_');
    if valid { Ok(()) } else { Err(format!("'{type_name}' is not a Godot class name (for example Node3D, MeshInstance3D, Sprite2D).")) }
}

pub fn mesh_type(name: &str) -> Result<&'static str, String> {
    let key = name.trim().to_lowercase().replace([' ', '_', '-'], "");
    let key = key.trim_end_matches("mesh").trim_end_matches("3d");
    Ok(match key {
        "box" | "cube" | "block" => "BoxMesh",
        "sphere" | "ball" | "orb" => "SphereMesh",
        "cylinder" | "tube" | "pillar" => "CylinderMesh",
        "capsule" | "pill" => "CapsuleMesh",
        "plane" | "floor" | "ground" => "PlaneMesh",
        "prism" | "pyramid" | "wedge" | "triangle" => "PrismMesh",
        "torus" | "donut" | "ring" => "TorusMesh",
        "quad" => "QuadMesh",
        _ => return Err(format!("Unknown mesh '{name}'. Use BoxMesh, SphereMesh, CylinderMesh, CapsuleMesh, PlaneMesh, PrismMesh, TorusMesh or QuadMesh.")),
    })
}

/// Godot's float formatting: integers without a decimal point, otherwise up to 6 decimals.
fn num(value: f64) -> String {
    let rounded = (value * 1_000_000.0).round() / 1_000_000.0;
    if rounded == 0.0 { return "0".into(); }
    if rounded.fract() == 0.0 { return format!("{}", rounded as i64); }
    format!("{rounded:.6}").trim_end_matches('0').trim_end_matches('.').to_string()
}

pub fn parse_color(input: &str) -> Result<(f64, f64, f64, f64), String> {
    let text = input.trim().to_lowercase();
    let compact = text.replace([' ', '_', '-'], "");
    let named: &[(&str, (f64, f64, f64))] = &[
        ("red", (1.0, 0.0, 0.0)), ("green", (0.0, 1.0, 0.0)), ("blue", (0.0, 0.0, 1.0)), ("yellow", (1.0, 1.0, 0.0)),
        ("orange", (1.0, 0.647, 0.0)), ("purple", (0.627, 0.125, 0.941)), ("violet", (0.933, 0.51, 0.933)),
        ("pink", (1.0, 0.753, 0.796)), ("white", (1.0, 1.0, 1.0)), ("black", (0.0, 0.0, 0.0)), ("gray", (0.5, 0.5, 0.5)),
        ("grey", (0.5, 0.5, 0.5)), ("cyan", (0.0, 1.0, 1.0)), ("teal", (0.0, 0.5, 0.5)), ("magenta", (1.0, 0.0, 1.0)),
        ("brown", (0.647, 0.165, 0.165)), ("gold", (1.0, 0.843, 0.0)), ("silver", (0.753, 0.753, 0.753)),
        ("navy", (0.0, 0.0, 0.5)), ("maroon", (0.5, 0.0, 0.0)), ("lime", (0.75, 1.0, 0.0)), ("lightblue", (0.678, 0.847, 0.902)),
        ("darkblue", (0.0, 0.0, 0.545)), ("lightgreen", (0.565, 0.933, 0.565)), ("darkgreen", (0.0, 0.392, 0.0)),
        ("darkred", (0.545, 0.0, 0.0)), ("turquoise", (0.251, 0.878, 0.816)), ("indigo", (0.294, 0.0, 0.51)),
        ("crimson", (0.863, 0.078, 0.235)), ("beige", (0.961, 0.961, 0.863)), ("skyblue", (0.529, 0.808, 0.922)),
    ];
    if let Some((_, (r, g, b))) = named.iter().find(|(name, _)| *name == compact) { return Ok((*r, *g, *b, 1.0)); }
    if let Some(hex) = compact.strip_prefix('#').or_else(|| (compact.len() == 6 && compact.chars().all(|c| c.is_ascii_hexdigit())).then_some(compact.as_str())) {
        let expanded: String = if hex.len() == 3 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.to_string() };
        if matches!(expanded.len(), 6 | 8) && expanded.chars().all(|c| c.is_ascii_hexdigit()) {
            let channel = |index: usize| u8::from_str_radix(&expanded[index..index + 2], 16).map(|value| value as f64 / 255.0).unwrap_or(0.0);
            return Ok((channel(0), channel(2), channel(4), if expanded.len() == 8 { channel(6) } else { 1.0 }));
        }
    }
    let numbers: Vec<f64> = text.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')).filter(|part| !part.is_empty()).filter_map(|part| part.parse().ok()).collect();
    if matches!(numbers.len(), 3 | 4) {
        let scale = if numbers[..3].iter().any(|value| *value > 1.0) { 255.0 } else { 1.0 };
        let channel = |value: f64| (value / scale).clamp(0.0, 1.0);
        let alpha = numbers.get(3).map(|value| if *value > 1.0 { value / 255.0 } else { *value }).unwrap_or(1.0).clamp(0.0, 1.0);
        return Ok((channel(numbers[0]), channel(numbers[1]), channel(numbers[2]), alpha));
    }
    Err(format!("Could not read the color '{input}'. Use a name like red, a hex code like #ff8800, or r,g,b values."))
}

type Basis = [[f64; 3]; 3];

/// Godot's Node3D basis from YXZ Euler angles (radians) and scale: R = Ry * Rx * Rz, B = R * diag(scale).
fn compose_basis(rotation: [f64; 3], scale: [f64; 3]) -> Basis {
    let (sx, cx) = rotation[0].sin_cos();
    let (sy, cy) = rotation[1].sin_cos();
    let (sz, cz) = rotation[2].sin_cos();
    let rows = [
        [cy * cz + sy * sx * sz, cz * sy * sx - cy * sz, cx * sy],
        [cx * sz, cx * cz, -sx],
        [cy * sx * sz - cz * sy, cy * cz * sx + sy * sz, cy * cx],
    ];
    let mut basis = rows;
    for row in basis.iter_mut() { for (column, value) in row.iter_mut().enumerate() { *value *= scale[column]; } }
    basis
}

/// Inverse of compose_basis, following Godot's Basis::get_scale and get_euler(YXZ).
fn decompose_basis(basis: &Basis) -> ([f64; 3], [f64; 3]) {
    let determinant = basis[0][0] * (basis[1][1] * basis[2][2] - basis[1][2] * basis[2][1])
        - basis[0][1] * (basis[1][0] * basis[2][2] - basis[1][2] * basis[2][0])
        + basis[0][2] * (basis[1][0] * basis[2][1] - basis[1][1] * basis[2][0]);
    let sign = if determinant < 0.0 { -1.0 } else { 1.0 };
    let mut scale = [0.0; 3];
    for (column, value) in scale.iter_mut().enumerate() {
        *value = sign * (0..3).map(|row| basis[row][column].powi(2)).sum::<f64>().sqrt();
    }
    let mut rows = [[0.0; 3]; 3];
    for row in 0..3 { for column in 0..3 { rows[row][column] = if scale[column] == 0.0 { 0.0 } else { basis[row][column] / scale[column] }; } }
    let m12 = rows[1][2];
    let rotation = if m12 < 1.0 - 1e-6 {
        if m12 > -(1.0 - 1e-6) {
            [(-m12).asin(), rows[0][2].atan2(rows[2][2]), rows[1][0].atan2(rows[1][1])]
        } else {
            [std::f64::consts::FRAC_PI_2, rows[0][1].atan2(rows[0][0]), 0.0]
        }
    } else {
        [-std::f64::consts::FRAC_PI_2, -rows[0][1].atan2(rows[0][0]), 0.0]
    };
    (rotation, scale)
}

fn parse_numbers(raw: &str, constructor: &str) -> Option<Vec<f64>> {
    let inner = raw.trim().strip_prefix(constructor)?.trim().strip_prefix('(')?.strip_suffix(')')?;
    inner.split(',').map(|part| part.trim().parse::<f64>().ok()).collect()
}

/// A property value written as a Godot literal; bare text becomes a quoted string.
pub fn godot_literal(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() { return Ok("\"\"".into()); }
    let quoted = (trimmed.starts_with('"') || trimmed.starts_with("&\"") || trimmed.starts_with("^\"")) && trimmed.ends_with('"') && trimmed.len() > 1;
    if quoted { return Ok(trimmed.to_string()); }
    if trimmed.contains(['\n', '\r']) { return Ok(quote(trimmed)); }
    let literal = trimmed.parse::<f64>().is_ok()
        || matches!(trimmed, "true" | "false" | "null")
        || (trimmed.chars().next().is_some_and(|first| first.is_ascii_uppercase()) && trimmed.contains('(') && trimmed.ends_with(')'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('{') && trimmed.ends_with('}'));
    Ok(if literal { trimmed.to_string() } else { quote(trimmed) })
}

pub fn new_scene_text(root_type: &str, root_name: &str) -> Result<String, String> {
    validate_type(root_type)?;
    validate_name(root_name)?;
    Ok(format!("[gd_scene format=3]\n\n[node name={} type={}]\n", quote(root_name), quote(root_type)))
}

impl Scene {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut scene = Scene { preamble: Vec::new(), sections: Vec::new(), crlf: false };
        let mut state = ValueState::default();
        for line in text.split('\n') {
            if state.clean() && section_kind(line).is_some() {
                state = ValueState::default();
                scene.sections.push(Section { header: line.to_string(), body: Vec::new() });
                continue;
            }
            state.scan(line);
            match scene.sections.last_mut() {
                Some(section) => section.body.push(line.to_string()),
                None => scene.preamble.push(line.to_string()),
            }
        }
        if scene.sections.first().and_then(|section| section_kind(&section.header)) != Some("gd_scene") {
            return Err("This is not a Godot text scene: a .tscn file starts with [gd_scene ...].".into());
        }
        scene.crlf = scene.sections[0].header.ends_with('\r');
        Ok(scene)
    }

    pub fn to_text(&self) -> String {
        let mut lines: Vec<&str> = self.preamble.iter().map(String::as_str).collect();
        for section in &self.sections {
            lines.push(&section.header);
            lines.extend(section.body.iter().map(String::as_str));
        }
        lines.join("\n")
    }

    fn line(&self, text: String) -> String {
        if self.crlf { format!("{text}\r") } else { text }
    }

    fn blank(&self) -> String {
        self.line(String::new())
    }

    pub fn nodes(&self) -> Vec<NodeInfo> {
        self.sections.iter().enumerate().filter(|(_, section)| section_kind(&section.header) == Some("node")).map(|(index, section)| {
            let name = attr(&section.header, "name").unwrap_or_default();
            let parent = attr(&section.header, "parent");
            let path = match parent.as_deref() {
                None => ".".to_string(),
                Some(".") => name.clone(),
                Some(parent) => format!("{parent}/{name}"),
            };
            NodeInfo { index, name, parent, path, type_name: attr(&section.header, "type") }
        }).collect()
    }

    /// A node by path ("Body/Mesh"), by name when unique, or the root via "." or its name.
    pub fn find_node(&self, reference: &str) -> Result<NodeInfo, String> {
        let nodes = self.nodes();
        let root_name = nodes.first().map(|node| node.name.clone()).unwrap_or_default();
        let mut wanted = reference.trim().trim_matches('"').trim_start_matches("./").to_string();
        if wanted.is_empty() || wanted == "." || wanted == root_name { wanted = ".".into(); }
        if let Some(rest) = wanted.strip_prefix(&format!("{root_name}/")) { wanted = rest.to_string(); }
        if let Some(found) = nodes.iter().find(|node| node.path == wanted) { return Ok(found.clone()); }
        let matches: Vec<&NodeInfo> = nodes.iter().filter(|node| node.name.eq_ignore_ascii_case(&wanted) || node.path.eq_ignore_ascii_case(&wanted)).collect();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(format!("No node '{reference}' in this scene. Its nodes are: {}.", nodes.iter().map(|node| if node.path == "." { format!("{} (root)", node.name) } else { node.path.clone() }).collect::<Vec<_>>().join(", "))),
            _ => Err(format!("'{reference}' matches several nodes ({}); give the full path.", matches.iter().map(|node| node.path.as_str()).collect::<Vec<_>>().join(", "))),
        }
    }

    fn is_descendant(node: &NodeInfo, ancestor: &NodeInfo) -> bool {
        if ancestor.parent.is_none() { return node.parent.is_some(); }
        node.parent.as_deref().is_some_and(|parent| parent == ancestor.path || parent.starts_with(&format!("{}/", ancestor.path)))
    }

    fn sub_resource_index(&self, id: &str) -> Option<usize> {
        self.sections.iter().position(|section| section_kind(&section.header) == Some("sub_resource") && attr(&section.header, "id").as_deref() == Some(id))
    }

    fn resource_type(&self, call: &str, id: &str) -> Option<String> {
        let kind = if call == "SubResource" { "sub_resource" } else { "ext_resource" };
        self.sections.iter().find(|section| section_kind(&section.header) == Some(kind) && attr(&section.header, "id").as_deref() == Some(id))
            .and_then(|section| if kind == "ext_resource" { attr(&section.header, "path") } else { attr(&section.header, "type") })
    }

    fn references(&self, call: &str, id: &str) -> usize {
        let needle = format!("{call}(\"{id}\")");
        self.sections.iter().map(|section| section.header.matches(&needle).count() + section.body.iter().map(|line| line.matches(&needle).count()).sum::<usize>()).sum()
    }

    fn insert_section(&mut self, at: usize, header: String, properties: Vec<String>) {
        let blank = self.blank();
        let joins_ext_block = section_kind(&header) == Some("ext_resource") && at > 0 && section_kind(&self.sections[at - 1].header) == Some("ext_resource");
        let mut body = properties;
        if joins_ext_block {
            // Godot lists external resources on consecutive lines; keep that layout.
            body.append(&mut self.sections[at - 1].body);
        } else {
            if at > 0 && self.sections[at - 1].body.last().is_none_or(|line| !line.trim().is_empty()) {
                self.sections[at - 1].body.push(blank.clone());
            }
            body.push(blank);
        }
        self.sections.insert(at, Section { header, body });
    }

    fn last_index_of(&self, kinds: &[&str]) -> Option<usize> {
        self.sections.iter().rposition(|section| section_kind(&section.header).is_some_and(|kind| kinds.contains(&kind)))
    }

    fn fresh_id(&self, prefix: &str) -> String {
        loop {
            let id = format!("{prefix}_{}", random_suffix());
            if self.references("SubResource", &id) == 0 && self.references("ExtResource", &id) == 0 && self.sub_resource_index(&id).is_none()
                && self.resource_type("ExtResource", &id).is_none() { return id; }
        }
    }

    fn refresh_load_steps(&mut self) {
        if attr(&self.sections[0].header, "load_steps").is_none() { return; }
        let count = self.sections.iter().filter(|section| matches!(section_kind(&section.header), Some("sub_resource" | "ext_resource"))).count() + 1;
        self.sections[0].header = with_attr(&self.sections[0].header, "load_steps", &count.to_string());
    }

    fn add_sub_resource(&mut self, type_name: &str, properties: Vec<String>) -> String {
        let id = self.fresh_id(type_name);
        let at = self.last_index_of(&["sub_resource", "ext_resource"]).map(|index| index + 1).unwrap_or(1);
        let header = self.line(format!("[sub_resource type={} id={}]", quote(type_name), quote(&id)));
        self.insert_section(at, header, properties);
        self.refresh_load_steps();
        id
    }

    /// Remove resources an edit orphaned (and anything only they referenced).
    fn prune(&mut self, mut candidates: Vec<(String, String)>) {
        while let Some((call, id)) = candidates.pop() {
            if self.references(&call, &id) > 0 { continue; }
            let kind = if call == "SubResource" { "sub_resource" } else { "ext_resource" };
            if let Some(index) = self.sections.iter().position(|section| section_kind(&section.header) == Some(kind) && attr(&section.header, "id").as_deref() == Some(id.as_str())) {
                let removed = self.sections.remove(index);
                candidates.extend(resource_refs(&removed.body.join("\n")));
            }
        }
        self.refresh_load_steps();
    }

    fn set_node_property(&mut self, path: &str, key: &str, raw: &str) -> Result<Option<String>, String> {
        let node = self.find_node(path)?;
        let crlf = self.crlf;
        let section = &mut self.sections[node.index];
        let previous = section.property(key);
        section.set_property(key, raw, crlf);
        Ok(previous)
    }

    fn describe_resource(&self, raw: &str) -> String {
        resource_refs(raw).into_iter().next()
            .map(|(call, id)| self.resource_type(&call, &id).unwrap_or_else(|| format!("{call} {id}")))
            .unwrap_or_else(|| raw.to_string())
    }

    pub fn tree(&self) -> Value {
        let nodes = self.nodes().into_iter().map(|node| {
            let section = &self.sections[node.index];
            let mut entry = json!({"path": if node.path == "." { format!("{} (root)", node.name) } else { node.path.clone() },
                "type": node.type_name.clone().unwrap_or_else(|| "instanced scene".into())});
            if let Some(mesh) = section.property("mesh") { entry["mesh"] = json!(self.describe_resource(&mesh)); }
            for key in ["material_override", "material"] {
                if let Some(material) = section.property(key) {
                    let color = call_id(&material, "SubResource").and_then(|id| self.sub_resource_index(&id)).and_then(|index| self.sections[index].property("albedo_color"));
                    entry["material"] = json!(match color { Some(color) => format!("{} {color}", self.describe_resource(&material)), None => self.describe_resource(&material) });
                }
            }
            if let Some(script) = section.property("script") { entry["script"] = json!(self.describe_resource(&script)); }
            if let Some(origin) = section.property("transform").and_then(|raw| parse_numbers(&raw, "Transform3D")).filter(|values| values.len() == 12) {
                entry["position"] = json!(format!("({}, {}, {})", num(origin[9]), num(origin[10]), num(origin[11])));
            } else if let Some(position) = section.property("position") {
                entry["position"] = json!(position);
            }
            entry
        }).collect::<Vec<_>>();
        json!({"nodeCount": nodes.len(), "nodes": nodes})
    }

    fn is_3d(&self, node: &NodeInfo) -> Option<bool> {
        let section = &self.sections[node.index];
        if let Some(type_name) = node.type_name.as_deref() {
            if type_name.ends_with("3D") { return Some(true); }
            if type_name.ends_with("2D") || matches!(type_name, "Control" | "Label" | "Button" | "ColorRect" | "Panel" | "TextureRect" | "RichTextLabel" | "LineEdit" | "TextEdit" | "Container" | "HBoxContainer" | "VBoxContainer" | "MarginContainer" | "CenterContainer") { return Some(false); }
            return None;
        }
        if section.property("transform").is_some() { return Some(true); }
        if section.property("position").is_some() { return Some(false); }
        self.nodes().first().and_then(|root| root.type_name.as_deref().map(|root_type| root_type.ends_with("3D")))
    }

    pub fn set_mesh(&mut self, reference: &str, mesh: &str, new_name: Option<&str>) -> Result<String, String> {
        let node = self.find_node(reference)?;
        let mesh_type = mesh_type(mesh)?;
        if node.type_name.as_deref() != Some("MeshInstance3D") {
            return Err(format!("set_mesh works on MeshInstance3D nodes; '{}' is {}. Use add_node with type MeshInstance3D and a mesh instead.", node.path, node.type_name.as_deref().unwrap_or("an instanced scene")));
        }
        let previous = self.sections[node.index].property("mesh");
        let previous_ref = previous.as_deref().and_then(|raw| resource_refs(raw).into_iter().next());
        let previous_type = previous.as_deref().map(|raw| self.describe_resource(raw));
        // Keep the primitive's own material so a colored sphere becomes a colored cube.
        let material = previous_ref.as_ref().filter(|(call, _)| call == "SubResource").and_then(|(_, id)| self.sub_resource_index(id)).and_then(|index| self.sections[index].property("material"));
        let properties = material.map(|raw| vec![self.line(format!("material = {raw}"))]).unwrap_or_default();
        let id = self.add_sub_resource(mesh_type, properties);
        self.set_node_property(&node.path, "mesh", &format!("SubResource(\"{id}\")"))?;
        if let Some(previous_ref) = previous_ref { self.prune(vec![previous_ref]); }
        let mut summary = format!("{} now uses a {}{}.", node.path, mesh_type, previous_type.map(|kind| format!(" (was {kind})")).unwrap_or_default());
        if let Some(new_name) = new_name.map(str::trim).filter(|name| !name.is_empty() && *name != node.name) {
            summary.push(' ');
            summary.push_str(&self.rename_node(&node.path, new_name)?);
        }
        Ok(summary)
    }

    pub fn set_color(&mut self, reference: &str, color: &str) -> Result<String, String> {
        let node = self.find_node(reference)?;
        let (r, g, b, a) = parse_color(color)?;
        let literal = format!("Color({}, {}, {}, {})", num(r), num(g), num(b), num(a));
        let type_name = node.type_name.clone().unwrap_or_default();
        let material_key = if type_name == "MeshInstance3D" || type_name == "MultiMeshInstance3D" { Some("material_override") }
            else if type_name.starts_with("CSG") { Some("material") } else { None };
        if let Some(key) = material_key {
            let current = self.sections[node.index].property(key);
            let reusable = current.as_deref().and_then(|raw| call_id(raw, "SubResource"))
                .filter(|id| matches!(self.resource_type("SubResource", id).as_deref(), Some("StandardMaterial3D" | "ORMMaterial3D")) && self.references("SubResource", id) == 1);
            let crlf = self.crlf;
            let index = match reusable {
                Some(id) => self.sub_resource_index(&id).ok_or("Material resource disappeared")?,
                None => {
                    let id = self.add_sub_resource("StandardMaterial3D", Vec::new());
                    let previous = self.set_node_property(&node.path, key, &format!("SubResource(\"{id}\")"))?;
                    if let Some(previous_ref) = previous.as_deref().and_then(|raw| resource_refs(raw).into_iter().next()) { self.prune(vec![previous_ref]); }
                    self.sub_resource_index(&id).ok_or("Material resource disappeared")?
                }
            };
            self.sections[index].set_property("albedo_color", &literal, crlf);
            if a < 1.0 { self.sections[index].set_property("transparency", "1", crlf); }
            return Ok(format!("{} is now colored {literal} through its {key}.", node.path));
        }
        let key = if type_name == "ColorRect" { "color" }
            else if type_name.ends_with("Light3D") { "light_color" }
            else if type_name.ends_with("Light2D") { "color" }
            else if type_name.ends_with("2D") || type_name.ends_with("3D") && (type_name.starts_with("Sprite") || type_name == "Label3D") || self.is_3d(&node) == Some(false) { "modulate" }
            else { return Err(format!("'{}' ({}) has no color of its own. Color a MeshInstance3D, CSG, sprite, light or 2D/UI node instead.", node.path, if type_name.is_empty() { "instanced scene" } else { &type_name })); };
        self.set_node_property(&node.path, key, &literal)?;
        Ok(format!("{} {key} is now {literal}.", node.path))
    }

    pub fn set_transform(&mut self, reference: &str, position: Option<&[f64]>, rotation_degrees: Option<&[f64]>, scale: Option<&[f64]>) -> Result<String, String> {
        let node = self.find_node(reference)?;
        if position.is_none() && rotation_degrees.is_none() && scale.is_none() { return Err("Give position, rotation_degrees and/or scale.".into()); }
        match self.is_3d(&node) {
            Some(true) => {
                let current = self.sections[node.index].property("transform").and_then(|raw| parse_numbers(&raw, "Transform3D")).filter(|values| values.len() == 12);
                let (basis, origin) = match &current {
                    Some(values) => ([[values[0], values[1], values[2]], [values[3], values[4], values[5]], [values[6], values[7], values[8]]], [values[9], values[10], values[11]]),
                    None => ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], [0.0; 3]),
                };
                let (current_rotation, current_scale) = decompose_basis(&basis);
                let triple = |values: &[f64], fallback: [f64; 3]| -> Result<[f64; 3], String> {
                    match values.len() { 3 => Ok([values[0], values[1], values[2]]), 1 => Ok([values[0]; 3]), _ => { let _ = fallback; Err("3D values need three numbers [x, y, z].".to_string()) } }
                };
                let origin = match position { Some(values) => triple(values, origin)?, None => origin };
                let rotation = match rotation_degrees { Some(values) => { let degrees = triple(values, [0.0; 3])?; [degrees[0].to_radians(), degrees[1].to_radians(), degrees[2].to_radians()] }, None => current_rotation };
                let scale = match scale { Some(values) => triple(values, current_scale)?, None => current_scale };
                let basis = if rotation_degrees.is_none() && scale.is_none_or(|_| false) && position.is_some() { basis } else { compose_basis(rotation, scale) };
                let values = [basis[0][0], basis[0][1], basis[0][2], basis[1][0], basis[1][1], basis[1][2], basis[2][0], basis[2][1], basis[2][2], origin[0], origin[1], origin[2]];
                let literal = format!("Transform3D({})", values.iter().map(|value| num(*value)).collect::<Vec<_>>().join(", "));
                self.set_node_property(&node.path, "transform", &literal)?;
                let degrees = rotation.map(f64::to_degrees);
                Ok(format!("{} is now at position ({}, {}, {}), rotation_degrees ({}, {}, {}), scale ({}, {}, {}).", node.path,
                    num(origin[0]), num(origin[1]), num(origin[2]), num(degrees[0]), num(degrees[1]), num(degrees[2]), num(scale[0]), num(scale[1]), num(scale[2])))
            }
            Some(false) => {
                let mut changes = Vec::new();
                if let Some(values) = position {
                    if values.len() < 2 { return Err("2D position needs [x, y].".into()); }
                    let literal = format!("Vector2({}, {})", num(values[0]), num(values[1]));
                    self.set_node_property(&node.path, "position", &literal)?;
                    changes.push(format!("position {literal}"));
                }
                if let Some(values) = rotation_degrees {
                    let degrees = *values.last().ok_or("rotation_degrees is empty")?;
                    let crlf = self.crlf;
                    let index = self.find_node(&node.path)?.index;
                    if degrees == 0.0 { self.sections[index].remove_property("rotation"); } else { self.sections[index].set_property("rotation", &num(degrees.to_radians()), crlf); }
                    changes.push(format!("rotation {} degrees", num(degrees)));
                }
                if let Some(values) = scale {
                    let (x, y) = match values.len() { 1 => (values[0], values[0]), 0 => return Err("scale is empty".into()), _ => (values[0], values[1]) };
                    let literal = format!("Vector2({}, {})", num(x), num(y));
                    self.set_node_property(&node.path, "scale", &literal)?;
                    changes.push(format!("scale {literal}"));
                }
                Ok(format!("{}: {}.", node.path, changes.join(", ")))
            }
            None => Err(format!("'{}' ({}) has no position. Move a Node3D or Node2D-based node.", node.path, node.type_name.as_deref().unwrap_or("instanced scene"))),
        }
    }

    pub fn rename_node(&mut self, reference: &str, new_name: &str) -> Result<String, String> {
        let node = self.find_node(reference)?;
        validate_name(new_name)?;
        let nodes = self.nodes();
        if nodes.iter().any(|other| other.index != node.index && other.parent == node.parent && other.name == new_name) {
            return Err(format!("A sibling node is already named '{new_name}'."));
        }
        self.sections[node.index].header = with_attr(&self.sections[node.index].header, "name", &quote(new_name));
        if node.parent.is_some() {
            let old_path = node.path.clone();
            let new_path = match node.parent.as_deref() { Some(".") => new_name.to_string(), Some(parent) => format!("{parent}/{new_name}"), None => unreachable!() };
            let moved = |value: &str| -> Option<String> {
                if value == old_path { Some(new_path.clone()) } else { value.strip_prefix(&format!("{old_path}/")).map(|rest| format!("{new_path}/{rest}")) }
            };
            for section in self.sections.iter_mut() {
                let keys: &[&str] = match section_kind(&section.header) { Some("node") => &["parent"], Some("connection") => &["from", "to"], Some("editable") => &["path"], _ => &[] };
                for key in keys {
                    if let Some(updated) = attr(&section.header, key).and_then(|value| moved(&value)) {
                        section.header = with_attr(&section.header, key, &quote(&updated));
                    }
                }
            }
        }
        Ok(format!("Renamed {} to {new_name}.", if node.path == "." { node.name.as_str() } else { node.path.as_str() }))
    }

    pub fn add_node(&mut self, parent_reference: &str, type_name: &str, name: &str) -> Result<String, String> {
        validate_type(type_name)?;
        validate_name(name)?;
        let parent = self.find_node(if parent_reference.trim().is_empty() { "." } else { parent_reference })?;
        let nodes = self.nodes();
        if nodes.iter().any(|node| node.parent.as_deref() == Some(parent.path.as_str()) && node.name == name) {
            return Err(format!("'{}' already has a child named '{name}'.", parent.path));
        }
        let at = nodes.iter().filter(|node| node.index == parent.index || Self::is_descendant(node, &parent)).map(|node| node.index).max().unwrap_or(parent.index) + 1;
        let mut attrs = vec![("name".to_string(), quote(name)), ("type".to_string(), quote(type_name)), ("parent".to_string(), quote(&parent.path))];
        if self.sections.iter().any(|section| section_kind(&section.header) == Some("node") && attr(&section.header, "unique_id").is_some()) {
            let used: Vec<String> = self.sections.iter().filter_map(|section| attr(&section.header, "unique_id")).collect();
            let id = loop {
                let candidate = (u64::from_str_radix(&random_suffix(), 36).unwrap_or(7) * 2_654_435_761 % 2_000_000_000 + 1).to_string();
                if !used.contains(&candidate) { break candidate; }
            };
            attrs.push(("unique_id".to_string(), id));
        }
        let header = build_header("node", &attrs, self.crlf);
        self.insert_section(at, header, Vec::new());
        Ok(if parent.path == "." { name.to_string() } else { format!("{}/{name}", parent.path) })
    }

    pub fn delete_node(&mut self, reference: &str) -> Result<String, String> {
        let node = self.find_node(reference)?;
        if node.parent.is_none() { return Err("The root node cannot be deleted; delete or replace the scene file instead.".into()); }
        let nodes = self.nodes();
        let mut doomed: Vec<usize> = nodes.iter().filter(|other| other.index == node.index || Self::is_descendant(other, &node)).map(|other| other.index).collect();
        let affected = |value: &str| value == node.path || value.starts_with(&format!("{}/", node.path));
        doomed.extend(self.sections.iter().enumerate().filter(|(_, section)| section_kind(&section.header) == Some("connection")
            && ["from", "to"].iter().any(|key| attr(&section.header, key).is_some_and(|value| affected(&value)))).map(|(index, _)| index));
        doomed.sort_unstable();
        doomed.dedup();
        let mut candidates = Vec::new();
        for index in doomed.iter().rev() {
            let removed = self.sections.remove(*index);
            candidates.extend(resource_refs(&format!("{}\n{}", removed.header, removed.body.join("\n"))));
        }
        self.prune(candidates);
        let removed_nodes = nodes.iter().filter(|other| other.index == node.index || Self::is_descendant(other, &node)).count();
        Ok(format!("Deleted {}{}.", node.path, if removed_nodes > 1 { format!(" and {} child node(s)", removed_nodes - 1) } else { String::new() }))
    }

    pub fn attach_script(&mut self, reference: &str, res_path: &str) -> Result<String, String> {
        let node = self.find_node(reference)?;
        let existing = self.sections.iter().find(|section| section_kind(&section.header) == Some("ext_resource") && attr(&section.header, "path").as_deref() == Some(res_path))
            .and_then(|section| attr(&section.header, "id"));
        let id = match existing {
            Some(id) => id,
            None => {
                let count = self.sections.iter().filter(|section| section_kind(&section.header) == Some("ext_resource")).count();
                let id = self.fresh_id(&(count + 1).to_string());
                let at = self.last_index_of(&["ext_resource"]).map(|index| index + 1).unwrap_or(1);
                let header = self.line(format!("[ext_resource type=\"Script\" path={} id={}]", quote(res_path), quote(&id)));
                self.insert_section(at, header, Vec::new());
                self.refresh_load_steps();
                id
            }
        };
        let previous = self.set_node_property(&node.path, "script", &format!("ExtResource(\"{id}\")"))?;
        if let Some(previous_ref) = previous.as_deref().and_then(|raw| resource_refs(raw).into_iter().next()).filter(|(_, previous_id)| *previous_id != id) {
            self.prune(vec![previous_ref]);
        }
        Ok(format!("Attached {res_path} to {}.", if node.path == "." { node.name.as_str() } else { node.path.as_str() }))
    }

    pub fn set_property(&mut self, reference: &str, property: &str, value: &str) -> Result<String, String> {
        if property.is_empty() || !property.chars().all(|character| character.is_ascii_alphanumeric() || "_/:".contains(character)) {
            return Err(format!("'{property}' is not a property name."));
        }
        if property == "name" { return self.rename_node(reference, value.trim().trim_matches('"')); }
        let node = self.find_node(reference)?;
        let literal = godot_literal(value)?;
        self.set_node_property(&node.path, property, &literal)?;
        Ok(format!("{} {property} = {literal}.", node.path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPHERE: &str = "[gd_scene format=3 uid=\"uid://dnj1g5x6k7uq6\"]\n\n[sub_resource type=\"SphereMesh\" id=\"SphereMesh_1\"]\nradius = 1.0\n\n[node name=\"SphereScene\" type=\"Node3D\" unique_id=158776782]\n\n[node name=\"Sphere\" type=\"MeshInstance3D\" parent=\".\" unique_id=2100579543]\nmesh = SubResource(\"SphereMesh_1\")\n";

    fn close(left: f64, right: f64) -> bool { (left - right).abs() < 1e-6 }

    #[test]
    fn untouched_scenes_round_trip_byte_for_byte() {
        for text in [SPHERE, "[gd_scene load_steps=2 format=3]\r\n\r\n[node name=\"A\" type=\"Node2D\"]\r\n", "[gd_scene format=3]\n\n[sub_resource type=\"GDScript\" id=\"s\"]\nscript/source = \"extends Node\n[node name=\\\"Fake\\\"]\nvar x = [1,\n2]\n\"\n\n[node name=\"Root\" type=\"Node\"]\nscript = SubResource(\"s\")"] {
            let scene = Scene::parse(text).unwrap();
            assert_eq!(scene.to_text(), text);
        }
        let embedded = Scene::parse("[gd_scene format=3]\n\n[sub_resource type=\"GDScript\" id=\"s\"]\nscript/source = \"extends Node\n[node name=\\\"Fake\\\"]\n\"\n\n[node name=\"Root\" type=\"Node\"]\n").unwrap();
        assert_eq!(embedded.nodes().len(), 1, "a header-like line inside a string literal is not a node");
    }

    #[test]
    fn set_mesh_swaps_the_primitive_renames_and_drops_the_orphan() {
        let mut scene = Scene::parse(SPHERE).unwrap();
        let summary = scene.set_mesh("Sphere", "cube", Some("Cube")).unwrap();
        assert!(summary.contains("BoxMesh") && summary.contains("was SphereMesh") && summary.contains("Renamed Sphere to Cube"), "{summary}");
        let text = scene.to_text();
        assert!(!text.contains("SphereMesh"), "orphaned sphere mesh removed:\n{text}");
        assert!(text.contains("[sub_resource type=\"BoxMesh\" id=\"BoxMesh_"), "{text}");
        assert!(text.contains("[node name=\"Cube\" type=\"MeshInstance3D\" parent=\".\" unique_id=2100579543]"), "{text}");
        let reparsed = Scene::parse(&text).unwrap();
        let tree = reparsed.tree();
        assert_eq!(tree["nodes"][1]["mesh"], "BoxMesh");
        assert!(text.starts_with("[gd_scene format=3 uid=\"uid://dnj1g5x6k7uq6\"]\n\n[sub_resource"), "{text}");
    }

    #[test]
    fn set_color_creates_then_reuses_a_material() {
        let mut scene = Scene::parse(SPHERE).unwrap();
        scene.set_color("Sphere", "red").unwrap();
        scene.set_color("Sphere", "#00ff00").unwrap();
        let text = scene.to_text();
        assert_eq!(text.matches("StandardMaterial3D").count(), 1, "{text}");
        assert!(text.contains("albedo_color = Color(0, 1, 0, 1)"), "{text}");
        assert!(text.contains("material_override = SubResource(\"StandardMaterial3D_"), "{text}");
        assert_eq!(parse_color("255, 128, 0").unwrap().1, 128.0 / 255.0);
        assert!(parse_color("not a color").is_err());
    }

    #[test]
    fn transforms_follow_godot_row_major_yxz_layout() {
        let mut scene = Scene::parse(SPHERE).unwrap();
        scene.set_transform("Sphere", Some(&[1.0, 2.0, 3.0]), Some(&[0.0, 90.0, 0.0]), None).unwrap();
        let raw = scene.sections[scene.find_node("Sphere").unwrap().index].property("transform").unwrap();
        assert_eq!(raw, "Transform3D(0, 0, 1, 0, 1, 0, -1, 0, 0, 1, 2, 3)");
        let (rotation, scale) = decompose_basis(&compose_basis([0.3, -1.1, 0.7], [2.0, 1.0, 0.5]));
        assert!(close(rotation[0], 0.3) && close(rotation[1], -1.1) && close(rotation[2], 0.7), "{rotation:?}");
        assert!(close(scale[0], 2.0) && close(scale[1], 1.0) && close(scale[2], 0.5), "{scale:?}");
        scene.set_transform("Sphere", Some(&[5.0, 0.0, 0.0]), None, None).unwrap();
        let moved = scene.sections[scene.find_node("Sphere").unwrap().index].property("transform").unwrap();
        assert_eq!(moved, "Transform3D(0, 0, 1, 0, 1, 0, -1, 0, 0, 5, 0, 0)", "moving keeps the rotation");
    }

    #[test]
    fn add_rename_and_delete_keep_paths_consistent() {
        let mut scene = Scene::parse(SPHERE).unwrap();
        assert_eq!(scene.add_node(".", "Node3D", "Body").unwrap(), "Body");
        assert_eq!(scene.add_node("Body", "MeshInstance3D", "Wheel").unwrap(), "Body/Wheel");
        scene.set_mesh("Wheel", "cylinder", None).unwrap();
        scene.rename_node("Body", "Car").unwrap();
        let text = scene.to_text();
        assert!(text.contains("[node name=\"Wheel\" type=\"MeshInstance3D\" parent=\"Car\""), "{text}");
        assert!(scene.find_node("Car/Wheel").is_ok());
        let summary = scene.delete_node("Car").unwrap();
        assert!(summary.contains("1 child"), "{summary}");
        let text = scene.to_text();
        assert!(!text.contains("CylinderMesh") && !text.contains("Wheel"), "{text}");
        assert_eq!(Scene::parse(&text).unwrap().nodes().len(), 2);
        assert!(scene.delete_node(".").is_err(), "root cannot be deleted");
    }

    #[test]
    fn attach_script_adds_one_ext_resource_and_updates_load_steps() {
        let mut scene = Scene::parse("[gd_scene load_steps=2 format=3]\n\n[sub_resource type=\"BoxMesh\" id=\"b\"]\n\n[node name=\"Root\" type=\"Node3D\"]\n").unwrap();
        scene.attach_script("Root", "res://scripts/spin.gd").unwrap();
        scene.attach_script(".", "res://scripts/spin.gd").unwrap();
        let text = scene.to_text();
        assert_eq!(text.matches("[ext_resource").count(), 1, "{text}");
        assert!(text.starts_with("[gd_scene load_steps=3 format=3]"), "{text}");
        assert!(text.contains("script = ExtResource(\"1_"), "{text}");
    }

    #[test]
    fn literals_and_scene_creation_are_safe() {
        assert_eq!(godot_literal("Hello").unwrap(), "\"Hello\"");
        assert_eq!(godot_literal("false").unwrap(), "false");
        assert_eq!(godot_literal("Vector3(1, 2, 3)").unwrap(), "Vector3(1, 2, 3)");
        assert_eq!(godot_literal("two\nlines").unwrap(), "\"two\nlines\"");
        assert!(validate_name("bad/name").is_err());
        let created = new_scene_text("Node3D", "Level").unwrap();
        assert!(Scene::parse(&created).unwrap().find_node(".").is_ok());
        assert!(new_scene_text("node3d; rm", "x").is_err());
    }
}
