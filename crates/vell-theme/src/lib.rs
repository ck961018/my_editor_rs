//! Data-only theme loading and resolution.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use vell_protocol::content_query::{
    Appearance, Color, FaceName, FacePatch, FaceValue, ThemeName, UnderlineStyle,
};

const BUILTIN_THEMES: &[&str] = &[
    include_str!("../../../runtime/themes/terminal-default.toml"),
    include_str!("../../../runtime/themes/catppuccin-base.toml"),
    include_str!("../../../runtime/themes/catppuccin-latte.toml"),
    include_str!("../../../runtime/themes/catppuccin-frappe.toml"),
    include_str!("../../../runtime/themes/catppuccin-macchiato.toml"),
    include_str!("../../../runtime/themes/catppuccin-mocha.toml"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum ColorDefinition {
    Literal(Color),
    Palette(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct FacePatchDefinition {
    foreground: FaceValue<ColorDefinition>,
    background: FaceValue<ColorDefinition>,
    bold: FaceValue<bool>,
    dim: FaceValue<bool>,
    italic: FaceValue<bool>,
    underline: FaceValue<bool>,
    underline_style: FaceValue<UnderlineStyle>,
    strikethrough: FaceValue<bool>,
}

impl FacePatchDefinition {
    fn overlay(&mut self, patch: &Self) {
        self.foreground.overlay(&patch.foreground);
        self.background.overlay(&patch.background);
        self.bold.overlay(&patch.bold);
        self.dim.overlay(&patch.dim);
        self.italic.overlay(&patch.italic);
        self.underline.overlay(&patch.underline);
        self.underline_style.overlay(&patch.underline_style);
        if matches!(patch.underline_style, FaceValue::Value(_))
            && matches!(patch.underline, FaceValue::Unspecified)
        {
            self.underline = FaceValue::Value(true);
        }
        self.strikethrough.overlay(&patch.strikethrough);
    }

    fn resolve(
        &self,
        palette: &HashMap<String, Color>,
    ) -> Result<FacePatch, ThemeError> {
        Ok(FacePatch {
            foreground: resolve_color_value(&self.foreground, palette)?,
            background: resolve_color_value(&self.background, palette)?,
            bold: self.bold,
            dim: self.dim,
            italic: self.italic,
            underline: self.underline,
            underline_style: self.underline_style,
            strikethrough: self.strikethrough,
        })
    }
}

fn resolve_color_value(
    value: &FaceValue<ColorDefinition>,
    palette: &HashMap<String, Color>,
) -> Result<FaceValue<Color>, ThemeError> {
    match value {
        FaceValue::Unspecified => Ok(FaceValue::Unspecified),
        FaceValue::Reset => Ok(FaceValue::Reset),
        FaceValue::Value(ColorDefinition::Literal(color)) => {
            Ok(FaceValue::Value(*color))
        }
        FaceValue::Value(ColorDefinition::Palette(name)) => palette
            .get(name)
            .copied()
            .map(FaceValue::Value)
            .ok_or_else(|| ThemeError::UnknownPaletteEntry(name.clone())),
    }
}

#[derive(Clone, Debug)]
struct ThemeDefinition {
    name: ThemeName,
    appearance: Appearance,
    inherits: Option<ThemeName>,
    selectable: bool,
    palette: HashMap<String, Color>,
    faces: HashMap<FaceName, FacePatchDefinition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTheme {
    name: ThemeName,
    appearance: Appearance,
    faces: HashMap<FaceName, FacePatch>,
}

impl ResolvedTheme {
    pub fn name(&self) -> &ThemeName {
        &self.name
    }

    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    pub fn face(&self, name: &FaceName) -> Option<&FacePatch> {
        let mut candidate = name.as_str();
        loop {
            if let Some(face) = self.faces.get(&FaceName::new(candidate)) {
                return Some(face);
            }
            let Some((parent, _)) = candidate.rsplit_once('.') else {
                return None;
            };
            candidate = parent;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThemeError {
    InvalidSyntax { line: usize, message: String },
    UnsupportedSchema(u32),
    MissingField(&'static str),
    DuplicateTheme(String),
    ThemeNotFound(String),
    ThemeInheritanceCycle(Vec<String>),
    InvalidColor(String),
    UnknownPaletteEntry(String),
    MissingRequiredFace { theme: String, face: String },
}

impl fmt::Display for ThemeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSyntax { line, message } => {
                write!(formatter, "invalid theme syntax on line {line}: {message}")
            }
            Self::UnsupportedSchema(schema) => {
                write!(formatter, "unsupported theme schema {schema}")
            }
            Self::MissingField(field) => write!(formatter, "missing theme field '{field}'"),
            Self::DuplicateTheme(theme) => write!(formatter, "duplicate theme '{theme}'"),
            Self::ThemeNotFound(theme) => write!(formatter, "theme '{theme}' was not found"),
            Self::ThemeInheritanceCycle(path) => {
                write!(formatter, "theme inheritance cycle: {}", path.join(" -> "))
            }
            Self::InvalidColor(color) => write!(formatter, "invalid color '{color}'"),
            Self::UnknownPaletteEntry(name) => {
                write!(formatter, "unknown palette entry '{name}'")
            }
            Self::MissingRequiredFace { theme, face } => {
                write!(formatter, "selectable theme '{theme}' is missing '{face}'")
            }
        }
    }
}

impl std::error::Error for ThemeError {}

#[derive(Default)]
pub struct ThemeRegistry {
    definitions: HashMap<ThemeName, ThemeDefinition>,
}

impl ThemeRegistry {
    pub fn with_builtins() -> Result<Self, ThemeError> {
        let mut registry = Self::default();
        for source in BUILTIN_THEMES {
            registry.register_toml(source)?;
        }
        Ok(registry)
    }

    pub fn register_toml(&mut self, source: &str) -> Result<(), ThemeError> {
        let definition = parse_theme(source)?;
        let name = definition.name.clone();
        if self.definitions.contains_key(&name) {
            return Err(ThemeError::DuplicateTheme(name.as_str().to_owned()));
        }
        self.definitions.insert(name, definition);
        Ok(())
    }

    pub fn selectable_names(&self) -> Vec<ThemeName> {
        let mut names = self
            .definitions
            .values()
            .filter(|definition| definition.selectable)
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>();
        names.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        names
    }

    pub fn resolve(&self, name: &ThemeName) -> Result<Arc<ResolvedTheme>, ThemeError> {
        let mut visiting = Vec::new();
        let mut visited = HashSet::new();
        let merged = self.merge_definition(name, &mut visiting, &mut visited)?;
        if merged.selectable
            && !merged.faces.contains_key(&FaceName::new("ui.editor"))
        {
            return Err(ThemeError::MissingRequiredFace {
                theme: name.as_str().to_owned(),
                face: "ui.editor".to_owned(),
            });
        }
        let faces = merged
            .faces
            .into_iter()
            .map(|(name, face)| Ok((name, face.resolve(&merged.palette)?)))
            .collect::<Result<_, ThemeError>>()?;
        Ok(Arc::new(ResolvedTheme {
            name: merged.name,
            appearance: merged.appearance,
            faces,
        }))
    }

    fn merge_definition(
        &self,
        name: &ThemeName,
        visiting: &mut Vec<ThemeName>,
        visited: &mut HashSet<ThemeName>,
    ) -> Result<ThemeDefinition, ThemeError> {
        if let Some(position) = visiting.iter().position(|item| item == name) {
            let mut path = visiting[position..]
                .iter()
                .map(|item| item.as_str().to_owned())
                .collect::<Vec<_>>();
            path.push(name.as_str().to_owned());
            return Err(ThemeError::ThemeInheritanceCycle(path));
        }
        let definition = self
            .definitions
            .get(name)
            .ok_or_else(|| ThemeError::ThemeNotFound(name.as_str().to_owned()))?;
        if !visited.insert(name.clone()) || definition.inherits.is_none() {
            return Ok(definition.clone());
        }
        visiting.push(name.clone());
        let parent = self.merge_definition(
            definition.inherits.as_ref().expect("parent checked"),
            visiting,
            visited,
        )?;
        visiting.pop();
        let mut merged = parent;
        merged.name = definition.name.clone();
        merged.appearance = definition.appearance;
        merged.inherits = definition.inherits.clone();
        merged.selectable = definition.selectable;
        merged.palette.extend(definition.palette.clone());
        for (name, patch) in &definition.faces {
            merged
                .faces
                .entry(name.clone())
                .or_default()
                .overlay(patch);
        }
        Ok(merged)
    }
}

#[derive(Clone, Copy)]
enum Section {
    Root,
    Palette,
    Faces,
}

fn parse_theme(source: &str) -> Result<ThemeDefinition, ThemeError> {
    let mut schema = None;
    let mut name = None;
    let mut appearance = None;
    let mut inherits = None;
    let mut selectable = None;
    let mut palette = HashMap::new();
    let mut faces = HashMap::new();
    let mut section = Section::Root;
    let mut seen_palette = false;
    let mut seen_faces = false;
    for (index, raw) in source.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        section = match line {
            "[palette]" => {
                if seen_palette {
                    return syntax(line_number, "duplicate palette section");
                }
                seen_palette = true;
                Section::Palette
            }
            "[faces]" => {
                if seen_faces {
                    return syntax(line_number, "duplicate faces section");
                }
                seen_faces = true;
                Section::Faces
            }
            _ if line.starts_with('[') => {
                return syntax(line_number, "unknown section");
            }
            _ => {
                let (key, value) = split_assignment(line)
                    .ok_or_else(|| ThemeError::InvalidSyntax {
                        line: line_number,
                        message: "expected key = value".to_owned(),
                    })?;
                match section {
                    Section::Root => match key {
                        "schema" if schema.is_none() => {
                            schema = Some(parse_u32(value, line_number)?)
                        }
                        "name" if name.is_none() => {
                            name = Some(ThemeName::new(parse_string(value, line_number)?))
                        }
                        "appearance" => {
                            if appearance.is_some() {
                                return syntax(line_number, "duplicate root property");
                            }
                            appearance = Some(match parse_string(value, line_number)?.as_str() {
                                "light" => Appearance::Light,
                                "dark" => Appearance::Dark,
                                _ => return syntax(line_number, "appearance must be light or dark"),
                            });
                        }
                        "inherits" if inherits.is_none() => {
                            inherits = Some(ThemeName::new(parse_string(value, line_number)?));
                        }
                        "selectable" if selectable.is_none() => {
                            selectable = Some(parse_bool(value, line_number)?)
                        }
                        "schema" | "name" | "inherits" | "selectable" => {
                            return syntax(line_number, "duplicate root property");
                        }
                        _ => return syntax(line_number, "unknown root property"),
                    },
                    Section::Palette => {
                        let key = parse_key(key, line_number)?;
                        let value = parse_string(value, line_number)?;
                        if palette.insert(key, parse_hex_color(&value)?).is_some() {
                            return syntax(line_number, "duplicate palette entry");
                        }
                    }
                    Section::Faces => {
                        let key = FaceName::new(parse_key(key, line_number)?);
                        if faces.insert(key, parse_face(value, line_number)?).is_some() {
                            return syntax(line_number, "duplicate face");
                        }
                    }
                }
                section
            }
        };
    }
    let schema = schema.ok_or(ThemeError::MissingField("schema"))?;
    if schema != 1 {
        return Err(ThemeError::UnsupportedSchema(schema));
    }
    Ok(ThemeDefinition {
        name: name.ok_or(ThemeError::MissingField("name"))?,
        appearance: appearance.ok_or(ThemeError::MissingField("appearance"))?,
        inherits,
        selectable: selectable.unwrap_or(true),
        palette,
        faces,
    })
}

fn parse_face(value: &str, line: usize) -> Result<FacePatchDefinition, ThemeError> {
    let value = value.trim();
    if !value.starts_with('{') || !value.ends_with('}') {
        return syntax(line, "face must be an inline table");
    }
    let mut face = FacePatchDefinition::default();
    let mut attributes = HashSet::new();
    for member in split_members(&value[1..value.len() - 1]) {
        if member.trim().is_empty() {
            continue;
        }
        let (key, value) = split_assignment(member)
            .ok_or_else(|| ThemeError::InvalidSyntax {
                line,
                message: "invalid face attribute".to_owned(),
            })?;
        if !attributes.insert(key) {
            return syntax(line, "duplicate face attribute");
        }
        match key {
            "foreground" => face.foreground = parse_color_definition(value, line)?,
            "background" => face.background = parse_color_definition(value, line)?,
            "bold" => face.bold = parse_face_bool(value, line)?,
            "dim" => face.dim = parse_face_bool(value, line)?,
            "italic" => face.italic = parse_face_bool(value, line)?,
            "underline" => face.underline = parse_face_bool(value, line)?,
            "underline-style" => {
                face.underline_style = parse_underline_style(value, line)?
            }
            "strikethrough" => face.strikethrough = parse_face_bool(value, line)?,
            _ => return syntax(line, "unknown face attribute"),
        }
    }
    if matches!(face.underline_style, FaceValue::Value(_))
        && matches!(face.underline, FaceValue::Unspecified)
    {
        face.underline = FaceValue::Value(true);
    }
    Ok(face)
}

fn parse_underline_style(
    value: &str,
    line: usize,
) -> Result<FaceValue<UnderlineStyle>, ThemeError> {
    if is_reset(value) {
        return Ok(FaceValue::Reset);
    }
    let style = match parse_string(value, line)?.as_str() {
        "line" => UnderlineStyle::Line,
        "double" => UnderlineStyle::Double,
        "curl" => UnderlineStyle::Curl,
        "dotted" => UnderlineStyle::Dotted,
        "dashed" => UnderlineStyle::Dashed,
        _ => return syntax(line, "invalid underline style"),
    };
    Ok(FaceValue::Value(style))
}

fn parse_color_definition(
    value: &str,
    line: usize,
) -> Result<FaceValue<ColorDefinition>, ThemeError> {
    if is_reset(value) {
        return Ok(FaceValue::Reset);
    }
    if let Ok(index) = value.trim().parse::<u16>() {
        let index = u8::try_from(index)
            .map_err(|_| ThemeError::InvalidColor(value.trim().to_owned()))?;
        return Ok(FaceValue::Value(ColorDefinition::Literal(Color::Ansi(index))));
    }
    let name = parse_string(value, line)?;
    if name.starts_with('#') {
        Ok(FaceValue::Value(ColorDefinition::Literal(
            parse_hex_color(&name)?,
        )))
    } else {
        Ok(FaceValue::Value(ColorDefinition::Palette(name)))
    }
}

fn parse_face_bool(value: &str, line: usize) -> Result<FaceValue<bool>, ThemeError> {
    if is_reset(value) {
        Ok(FaceValue::Reset)
    } else {
        Ok(FaceValue::Value(parse_bool(value, line)?))
    }
}

fn is_reset(value: &str) -> bool {
    value.split_whitespace().collect::<String>() == "{reset=true}"
}

fn parse_hex_color(value: &str) -> Result<Color, ThemeError> {
    let Some(hex) = value.strip_prefix('#') else {
        return Err(ThemeError::InvalidColor(value.to_owned()));
    };
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ThemeError::InvalidColor(value.to_owned()));
    }
    let red = u8::from_str_radix(&hex[0..2], 16)
        .map_err(|_| ThemeError::InvalidColor(value.to_owned()))?;
    let green = u8::from_str_radix(&hex[2..4], 16)
        .map_err(|_| ThemeError::InvalidColor(value.to_owned()))?;
    let blue = u8::from_str_radix(&hex[4..6], 16)
        .map_err(|_| ThemeError::InvalidColor(value.to_owned()))?;
    Ok(Color::Rgb { red, green, blue })
}

fn parse_key(value: &str, line: usize) -> Result<String, ThemeError> {
    let value = value.trim();
    if value.starts_with('"') {
        parse_string(value, line)
    } else if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Ok(value.to_owned())
    } else {
        syntax(line, "invalid key")
    }
}

fn parse_string(value: &str, line: usize) -> Result<String, ThemeError> {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        if !inner.chars().any(|character| matches!(character, '"' | '\\')) {
            return Ok(inner.to_owned());
        }
    }
    syntax(line, "expected a simple quoted string")
}

fn parse_bool(value: &str, line: usize) -> Result<bool, ThemeError> {
    match value.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => syntax(line, "expected a boolean"),
    }
}

fn parse_u32(value: &str, line: usize) -> Result<u32, ThemeError> {
    value
        .trim()
        .parse()
        .map_err(|_| ThemeError::InvalidSyntax {
            line,
            message: "expected an unsigned integer".to_owned(),
        })
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let mut quoted = false;
    let mut depth = 0usize;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '{' if !quoted => depth += 1,
            '}' if !quoted => depth = depth.saturating_sub(1),
            '=' if !quoted && depth == 0 => {
                return Some((line[..index].trim(), line[index + 1..].trim()));
            }
            _ => {}
        }
    }
    None
}

fn split_members(value: &str) -> Vec<&str> {
    let mut members = Vec::new();
    let mut quoted = false;
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, character) in value.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '{' if !quoted => depth += 1,
            '}' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                members.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    members.push(&value[start..]);
    members
}

fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (index, character) in line.char_indices() {
        match character {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..index],
            _ => {}
        }
    }
    line
}

fn syntax<T>(line: usize, message: &str) -> Result<T, ThemeError> {
    Err(ThemeError::InvalidSyntax {
        line,
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_resolve_and_hide_abstract_fragment() {
        let registry = ThemeRegistry::with_builtins().unwrap();
        let names = registry
            .selectable_names()
            .into_iter()
            .map(|name| name.as_str().to_owned())
            .collect::<Vec<_>>();
        assert!(names.contains(&"catppuccin-mocha".to_owned()));
        assert!(!names.contains(&"catppuccin-base".to_owned()));
    }

    #[test]
    fn child_palette_resolves_parent_face() {
        let registry = ThemeRegistry::with_builtins().unwrap();
        let theme = registry
            .resolve(&ThemeName::new("catppuccin-mocha"))
            .unwrap();
        assert_eq!(
            theme.face(&FaceName::new("ui.editor")).unwrap().background,
            FaceValue::Value(Color::Rgb {
                red: 0x1e,
                green: 0x1e,
                blue: 0x2e,
            })
        );
    }

    #[test]
    fn dotted_lookup_uses_nearest_parent_without_merging_parents() {
        let registry = ThemeRegistry::with_builtins().unwrap();
        let theme = registry
            .resolve(&ThemeName::new("catppuccin-mocha"))
            .unwrap();
        assert_eq!(
            theme
                .face(&FaceName::new("syntax.function.macro.rust"))
                .unwrap(),
            theme
                .face(&FaceName::new("syntax.function.macro"))
                .unwrap()
        );
    }

    #[test]
    fn terminal_theme_keeps_non_hierarchical_capture_aliases() {
        let registry = ThemeRegistry::with_builtins().unwrap();
        let theme = registry
            .resolve(&ThemeName::new("terminal-default"))
            .unwrap();
        assert_eq!(
            theme
                .face(&FaceName::new("syntax.constructor"))
                .unwrap()
                .foreground,
            FaceValue::Value(Color::Ansi(109))
        );
    }

    #[test]
    fn inheritance_cycles_report_the_full_path() {
        let mut registry = ThemeRegistry::default();
        for source in [
            "schema=1\nname=\"a\"\nappearance=\"dark\"\ninherits=\"b\"",
            "schema=1\nname=\"b\"\nappearance=\"dark\"\ninherits=\"a\"",
        ] {
            registry.register_toml(source).unwrap();
        }
        assert_eq!(
            registry.resolve(&ThemeName::new("a")).unwrap_err(),
            ThemeError::ThemeInheritanceCycle(vec![
                "a".to_owned(),
                "b".to_owned(),
                "a".to_owned(),
            ])
        );
    }

    #[test]
    fn theme_schema_parses_extended_text_attributes() {
        let mut registry = ThemeRegistry::default();
        registry
            .register_toml(
                r#"
schema = 1
name = "extended"
appearance = "dark"
selectable = false

[faces]
"plugin.test" = { dim = true, underline-style = "curl", strikethrough = true }
"#,
            )
            .unwrap();
        let theme = registry.resolve(&ThemeName::new("extended")).unwrap();
        let face = theme.face(&FaceName::new("plugin.test")).unwrap();

        assert_eq!(face.dim, FaceValue::Value(true));
        assert_eq!(face.underline, FaceValue::Value(true));
        assert_eq!(
            face.underline_style,
            FaceValue::Value(UnderlineStyle::Curl)
        );
        assert_eq!(face.strikethrough, FaceValue::Value(true));
    }

    #[test]
    fn duplicate_theme_fields_are_rejected() {
        for source in [
            "schema=1\nschema=1\nname=\"a\"\nappearance=\"dark\"",
            "schema=1\nname=\"a\"\nappearance=\"dark\"\n[faces]\n\"x\"={}\n\"x\"={}",
            "schema=1\nname=\"a\"\nappearance=\"dark\"\n[faces]\n\"x\"={bold=true,bold=false}",
        ] {
            assert!(matches!(
                parse_theme(source),
                Err(ThemeError::InvalidSyntax { .. })
            ));
        }
    }
}
