use std::cell::Cell;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use vell_protocol::content_query::{
    DisplayProfile, FaceExpr, FaceName, FaceOverride, FacePatch, FaceRemapScope, FaceRemapToken,
    PaintFace, ThemeName,
};
use vell_protocol::ids::ViewId;
use vell_protocol::revision::Revision;
use vell_theme::{ResolvedTheme, ThemeError, ThemeRegistry};

use crate::mode::{FaceConflict, FaceRegistrationError, FaceRegistry, ModeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FaceRemapOwner {
    User,
    Mode(ModeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ResolvedFaceOperation {
    SetBase {
        scope: FaceRemapScope,
        face: FaceName,
        expressions: Option<Vec<FaceExpr>>,
        owner: FaceRemapOwner,
    },
    AddRelative {
        scope: FaceRemapScope,
        face: FaceName,
        token: FaceRemapToken,
        expressions: Vec<FaceExpr>,
        owner: FaceRemapOwner,
    },
    RemoveRelative {
        token: FaceRemapToken,
        owner: FaceRemapOwner,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FaceRemapError(String);

impl fmt::Display for FaceRemapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FaceRemapError {}

pub(super) struct FaceEnvironment {
    fallback_theme: Arc<ResolvedTheme>,
    active_theme: Arc<ResolvedTheme>,
    global_overrides: HashMap<FaceName, FacePatch>,
    theme_overrides: HashMap<ThemeName, HashMap<FaceName, FacePatch>>,
    display_profile: DisplayProfile,
    revision: Revision,
}

impl FaceEnvironment {
    pub(super) fn new(theme: Option<&ThemeName>) -> Result<Self, ThemeError> {
        Self::with_overrides(theme, Vec::new())
    }

    pub(super) fn with_overrides(
        theme: Option<&ThemeName>,
        overrides: Vec<FaceOverride>,
    ) -> Result<Self, ThemeError> {
        let registry = ThemeRegistry::with_builtins()?;
        for face_override in &overrides {
            if let Some(theme) = &face_override.theme {
                registry.resolve(theme)?;
            }
        }
        let fallback_theme = registry.resolve(&ThemeName::new("terminal-default"))?;
        let active_theme = match theme {
            Some(theme) => registry.resolve(theme)?,
            None => fallback_theme.clone(),
        };
        let mut global_overrides = HashMap::new();
        let mut theme_overrides: HashMap<ThemeName, HashMap<FaceName, FacePatch>> = HashMap::new();
        for face_override in overrides {
            let target = match face_override.theme {
                Some(theme) => theme_overrides.entry(theme).or_default(),
                None => &mut global_overrides,
            };
            target
                .entry(face_override.face)
                .or_default()
                .overlay(&face_override.patch);
        }
        Ok(Self {
            fallback_theme,
            active_theme,
            global_overrides,
            theme_overrides,
            display_profile: DisplayProfile::default(),
            revision: Revision(0),
        })
    }

    #[allow(dead_code, reason = "theme diagnostics expose the active name")]
    pub(super) fn active_theme(&self) -> &ThemeName {
        self.active_theme.name()
    }

    #[allow(dead_code, reason = "theme switching and caches use this revision")]
    pub(super) fn revision(&self) -> Revision {
        self.revision
    }

    fn bump_revision(&mut self) {
        self.revision.next();
    }

    pub(super) fn resolve(&self, name: &FaceName, fallback: &FacePatch) -> FacePatch {
        let mut resolved = fallback.clone();
        if let Some(patch) = self.fallback_theme.face(name) {
            resolved.overlay(patch);
        }
        if self.active_theme.name() != self.fallback_theme.name()
            && let Some(patch) = self.active_theme.face(name)
        {
            resolved.overlay(patch);
        }
        if let Some(patch) = lookup_override(&self.global_overrides, name) {
            resolved.overlay(patch);
        }
        if let Some(overrides) = self.theme_overrides.get(self.active_theme.name())
            && let Some(patch) = lookup_override(overrides, name)
        {
            resolved.overlay(patch);
        }
        self.display_profile.adapt_patch(&mut resolved);
        resolved
    }

    fn set_display_profile(&mut self, profile: DisplayProfile) {
        if self.display_profile != profile {
            self.display_profile = profile;
            self.bump_revision();
        }
    }

    fn adapt_to_display(&self, patch: &mut FacePatch) {
        self.display_profile.adapt_patch(patch);
    }
}

fn lookup_override<'a>(
    overrides: &'a HashMap<FaceName, FacePatch>,
    name: &FaceName,
) -> Option<&'a FacePatch> {
    let mut candidate = name.as_str();
    loop {
        if let Some(patch) = overrides.get(&FaceName::new(candidate)) {
            return Some(patch);
        }
        let (parent, _) = candidate.rsplit_once('.')?;
        candidate = parent;
    }
}

pub(super) struct SessionFaces {
    registry: FaceRegistry,
    environment: FaceEnvironment,
    active_view: Cell<Option<ViewId>>,
    remaps: FaceRemapStore,
}

impl Default for SessionFaces {
    fn default() -> Self {
        Self::new(
            FaceRegistry::default(),
            FaceEnvironment::new(None).expect("built-in themes must be valid"),
        )
    }
}

impl SessionFaces {
    pub(super) fn new(registry: FaceRegistry, environment: FaceEnvironment) -> Self {
        Self {
            registry,
            environment,
            active_view: Cell::new(None),
            remaps: FaceRemapStore::default(),
        }
    }

    pub(super) fn resolve(&self, name: &FaceName) -> FacePatch {
        self.resolve_inner(name, &mut Vec::new())
    }

    fn resolve_inner(&self, name: &FaceName, visiting: &mut Vec<FaceName>) -> FacePatch {
        if visiting.contains(name) {
            return FacePatch::default();
        }
        visiting.push(name.clone());
        let mut fallback = FacePatch::default();
        if let Some(definition) = self.registry.definition(name) {
            for parent in definition.inherits.iter().rev() {
                fallback.overlay(&self.resolve_inner(parent, visiting));
            }
            fallback.overlay(&definition.fallback);
        }
        visiting.pop();
        self.environment.resolve(name, &fallback)
    }

    pub(super) fn resolve_for(
        &self,
        name: &FaceName,
        content: vell_protocol::ids::ContentId,
        view: ViewId,
    ) -> FacePatch {
        let global = self.resolve(name);
        let mut resolved = self
            .remaps
            .resolve(name, content, view, global, |named| self.resolve(named));
        self.environment.adapt_to_display(&mut resolved);
        resolved
    }

    pub(super) fn resolve_root_for(
        &self,
        name: &FaceName,
        content: vell_protocol::ids::ContentId,
        view: ViewId,
    ) -> PaintFace {
        self.resolve_for(name, content, view)
            .resolve(&PaintFace::default())
    }

    pub(super) fn set_active_view(&self, view: Option<ViewId>) {
        self.active_view.set(view);
    }

    pub(super) fn set_display_profile(&mut self, profile: DisplayProfile) {
        self.environment.set_display_profile(profile);
    }

    pub(super) fn resolve_status_bar_root(
        &self,
        target: ViewId,
        content: vell_protocol::ids::ContentId,
        view: ViewId,
    ) -> PaintFace {
        let name = if self.active_view.get() == Some(target) {
            "ui.status-bar"
        } else {
            "ui.status-bar.inactive"
        };
        self.resolve_root_for(&FaceName::new(name), content, view)
    }

    pub(super) fn provider(&self, name: &FaceName) -> Option<&crate::mode_name::ModeName> {
        self.registry.provider(name)
    }

    pub(super) fn conflicts(&self) -> &[FaceConflict] {
        self.registry.conflicts()
    }

    pub(super) fn registration_errors(&self) -> &[FaceRegistrationError] {
        self.registry.registration_errors()
    }

    pub(super) fn registry_mut(&mut self) -> &mut FaceRegistry {
        &mut self.registry
    }

    pub(super) fn validate_operation(
        &self,
        operation: &ResolvedFaceOperation,
    ) -> Result<(), FaceRemapError> {
        self.remaps.validate(operation)
    }

    pub(super) fn apply_operation(
        &mut self,
        operation: ResolvedFaceOperation,
    ) -> Result<(), FaceRemapError> {
        self.remaps.apply(operation)?;
        self.environment.bump_revision();
        Ok(())
    }

    pub(super) fn remove_view_remaps(&mut self, view: ViewId) {
        if self.remaps.remove_scope(FaceRemapScope::View(view)) {
            self.environment.bump_revision();
        }
    }

    pub(super) fn remove_content_remaps(&mut self, content: vell_protocol::ids::ContentId) {
        if self.remaps.remove_scope(FaceRemapScope::Content(content)) {
            self.environment.bump_revision();
        }
    }

    pub(super) fn remove_mode_remaps(&mut self, mode: ModeId) {
        if self.remaps.remove_owner(FaceRemapOwner::Mode(mode)) {
            self.environment.bump_revision();
        }
    }
}

#[derive(Default)]
struct FaceRemapStore {
    entries: HashMap<(FaceRemapScope, FaceName), FaceRemapEntry>,
    tokens: HashMap<FaceRemapToken, (FaceRemapScope, FaceName, FaceRemapOwner)>,
}

#[derive(Default)]
struct FaceRemapEntry {
    base: Option<BaseFaceRemap>,
    relatives: Vec<RelativeFaceRemap>,
}

struct BaseFaceRemap {
    owner: FaceRemapOwner,
    expressions: Vec<FaceExpr>,
}

struct RelativeFaceRemap {
    token: FaceRemapToken,
    owner: FaceRemapOwner,
    expressions: Vec<FaceExpr>,
}

impl FaceRemapStore {
    fn validate(&self, operation: &ResolvedFaceOperation) -> Result<(), FaceRemapError> {
        match operation {
            ResolvedFaceOperation::SetBase {
                scope,
                face,
                expressions: _,
                owner,
            } => {
                if let Some(base) = self
                    .entries
                    .get(&(*scope, face.clone()))
                    .and_then(|entry| entry.base.as_ref())
                    && base.owner != *owner
                {
                    return Err(FaceRemapError(
                        "face remap base is owned by another contributor".to_owned(),
                    ));
                }
            }
            ResolvedFaceOperation::AddRelative {
                token, expressions, ..
            } => {
                if self.tokens.contains_key(token) {
                    return Err(FaceRemapError("face remap token already exists".to_owned()));
                }
                if expressions.is_empty() {
                    return Err(FaceRemapError(
                        "relative face remap requires an expression".to_owned(),
                    ));
                }
            }
            ResolvedFaceOperation::RemoveRelative { token, owner } => {
                let Some((_, _, active_owner)) = self.tokens.get(token) else {
                    return Err(FaceRemapError("unknown face remap token".to_owned()));
                };
                if active_owner != owner {
                    return Err(FaceRemapError(
                        "face remap token is owned by another contributor".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn apply(&mut self, operation: ResolvedFaceOperation) -> Result<(), FaceRemapError> {
        self.validate(&operation)?;
        match operation {
            ResolvedFaceOperation::SetBase {
                scope,
                face,
                expressions,
                owner,
            } => {
                let key = (scope, face);
                let entry = self.entries.entry(key.clone()).or_default();
                entry.base = expressions.map(|expressions| BaseFaceRemap { owner, expressions });
                if entry.base.is_none() && entry.relatives.is_empty() {
                    self.entries.remove(&key);
                }
            }
            ResolvedFaceOperation::AddRelative {
                scope,
                face,
                token,
                expressions,
                owner,
            } => {
                self.tokens.insert(token, (scope, face.clone(), owner));
                self.entries
                    .entry((scope, face))
                    .or_default()
                    .relatives
                    .push(RelativeFaceRemap {
                        token,
                        owner,
                        expressions,
                    });
            }
            ResolvedFaceOperation::RemoveRelative { token, .. } => {
                let (scope, face, _) = self
                    .tokens
                    .remove(&token)
                    .expect("validated face remap token exists");
                let key = (scope, face);
                let entry = self
                    .entries
                    .get_mut(&key)
                    .expect("token index points at a face remap entry");
                entry.relatives.retain(|relative| relative.token != token);
                if entry.base.is_none() && entry.relatives.is_empty() {
                    self.entries.remove(&key);
                }
            }
        }
        Ok(())
    }

    fn resolve(
        &self,
        name: &FaceName,
        content: vell_protocol::ids::ContentId,
        view: ViewId,
        mut resolved: FacePatch,
        global: impl Fn(&FaceName) -> FacePatch,
    ) -> FacePatch {
        for scope in [
            FaceRemapScope::Session,
            FaceRemapScope::Content(content),
            FaceRemapScope::View(view),
        ] {
            let Some(entry) = self.entries.get(&(scope, name.clone())) else {
                continue;
            };
            if let Some(base) = &entry.base {
                resolved = resolve_expressions(&base.expressions, &global);
            }
            for relative in &entry.relatives {
                resolved.overlay(&resolve_expressions(&relative.expressions, &global));
            }
        }
        resolved
    }

    fn remove_scope(&mut self, scope: FaceRemapScope) -> bool {
        let previous_entries = self.entries.len();
        let previous_tokens = self.tokens.len();
        self.entries.retain(|(candidate, _), _| *candidate != scope);
        self.tokens
            .retain(|_, (candidate, _, _)| *candidate != scope);
        self.entries.len() != previous_entries || self.tokens.len() != previous_tokens
    }

    fn remove_owner(&mut self, owner: FaceRemapOwner) -> bool {
        let previous_entries = self.entries.len();
        let previous_tokens = self.tokens.len();
        let previous_relatives = self
            .entries
            .values()
            .map(|entry| entry.relatives.len())
            .sum::<usize>();
        let previous_bases = self
            .entries
            .values()
            .filter(|entry| entry.base.is_some())
            .count();
        for entry in self.entries.values_mut() {
            if entry.base.as_ref().is_some_and(|base| base.owner == owner) {
                entry.base = None;
            }
            entry.relatives.retain(|relative| relative.owner != owner);
        }
        self.entries
            .retain(|_, entry| entry.base.is_some() || !entry.relatives.is_empty());
        self.tokens
            .retain(|_, (_, _, candidate)| *candidate != owner);
        let current_relatives = self
            .entries
            .values()
            .map(|entry| entry.relatives.len())
            .sum::<usize>();
        let current_bases = self
            .entries
            .values()
            .filter(|entry| entry.base.is_some())
            .count();
        self.entries.len() != previous_entries
            || self.tokens.len() != previous_tokens
            || current_relatives != previous_relatives
            || current_bases != previous_bases
    }
}

fn resolve_expressions(
    expressions: &[FaceExpr],
    global: &impl Fn(&FaceName) -> FacePatch,
) -> FacePatch {
    let mut resolved = FacePatch::default();
    for expression in expressions {
        match expression {
            FaceExpr::Named(name) => resolved.overlay(&global(name)),
            FaceExpr::Patch(patch) => resolved.overlay(patch),
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_protocol::content_query::{Color, ColorDepth, FaceValue};

    #[test]
    fn active_theme_overlays_terminal_fallback_by_attribute() {
        let environment = FaceEnvironment::new(Some(&ThemeName::new("catppuccin-mocha"))).unwrap();
        let face = environment.resolve(&FaceName::new("syntax.comment"), &FacePatch::default());
        assert_eq!(face.italic, FaceValue::Value(true));
        assert_eq!(
            face.foreground,
            FaceValue::Value(Color::Rgb {
                red: 0x93,
                green: 0x99,
                blue: 0xb2,
            })
        );
    }

    #[test]
    fn display_profile_is_applied_after_all_visual_layers() {
        let environment = FaceEnvironment::new(Some(&ThemeName::new("catppuccin-mocha"))).unwrap();
        let mut faces = SessionFaces::new(FaceRegistry::default(), environment);
        faces.set_display_profile(DisplayProfile {
            color_depth: ColorDepth::Ansi16,
            appearance: None,
            supports_italic: false,
            supports_underline: true,
            supports_extended_underline: false,
            supports_undercurl: false,
            supports_strikethrough: false,
            supports_dim: true,
        });

        let comment = faces.resolve(&FaceName::new("syntax.comment"));
        let diagnostic = faces.resolve(&FaceName::new("diagnostic.error"));
        faces
            .apply_operation(ResolvedFaceOperation::AddRelative {
                scope: FaceRemapScope::View(ViewId(9)),
                face: FaceName::new("syntax.comment"),
                token: FaceRemapToken(90),
                expressions: vec![FaceExpr::Patch(FacePatch {
                    background: FaceValue::Value(Color::Rgb {
                        red: 255,
                        green: 0,
                        blue: 0,
                    }),
                    ..FacePatch::default()
                })],
                owner: FaceRemapOwner::User,
            })
            .unwrap();
        let local = faces.resolve_for(
            &FaceName::new("syntax.comment"),
            vell_protocol::ids::ContentId(9),
            ViewId(9),
        );

        assert!(matches!(
            comment.foreground,
            FaceValue::Value(Color::Ansi16(0..=15))
        ));
        assert_eq!(comment.italic, FaceValue::Unspecified);
        assert_eq!(
            diagnostic.underline_style,
            FaceValue::Value(vell_protocol::content_query::UnderlineStyle::Line)
        );
        assert!(matches!(
            local.background,
            FaceValue::Value(Color::Ansi16(0..=15))
        ));
    }

    #[test]
    fn status_bar_uses_inactive_face_for_non_focused_target() {
        let environment = FaceEnvironment::new(Some(&ThemeName::new("catppuccin-mocha"))).unwrap();
        let faces = SessionFaces::new(FaceRegistry::default(), environment);
        faces.set_active_view(Some(ViewId(1)));

        let active =
            faces.resolve_status_bar_root(ViewId(1), vell_protocol::ids::ContentId(1), ViewId(10));
        let inactive =
            faces.resolve_status_bar_root(ViewId(2), vell_protocol::ids::ContentId(1), ViewId(10));

        assert_eq!(
            active.background,
            Some(Color::Rgb {
                red: 0x18,
                green: 0x18,
                blue: 0x25,
            })
        );
        assert_eq!(
            inactive.background,
            Some(Color::Rgb {
                red: 0x31,
                green: 0x32,
                blue: 0x44,
            })
        );
    }

    #[test]
    fn global_and_per_theme_overrides_merge_by_attribute() {
        let environment = FaceEnvironment::with_overrides(
            Some(&ThemeName::new("catppuccin-mocha")),
            vec![
                FaceOverride {
                    face: FaceName::new("syntax.comment"),
                    theme: None,
                    patch: FacePatch {
                        italic: FaceValue::Value(false),
                        ..FacePatch::default()
                    },
                },
                FaceOverride {
                    face: FaceName::new("syntax.comment"),
                    theme: Some(ThemeName::new("catppuccin-mocha")),
                    patch: FacePatch {
                        underline: FaceValue::Value(true),
                        ..FacePatch::default()
                    },
                },
            ],
        )
        .unwrap();

        let face = environment.resolve(&FaceName::new("syntax.comment"), &FacePatch::default());

        assert_eq!(face.italic, FaceValue::Value(false));
        assert_eq!(face.underline, FaceValue::Value(true));
        assert_eq!(
            face.foreground,
            FaceValue::Value(Color::Rgb {
                red: 0x93,
                green: 0x99,
                blue: 0xb2,
            })
        );
    }

    #[test]
    fn private_face_inherits_the_resolved_themed_parent() {
        let mut registry = FaceRegistry::default();
        registry.register_definitions(
            crate::mode_name::ModeName::new("todo"),
            vec![vell_protocol::content_query::FaceDefinition {
                name: FaceName::new("plugin.todo.warning"),
                inherits: vec![FaceName::new("diagnostic.warning")],
                fallback: FacePatch {
                    bold: FaceValue::Value(true),
                    ..FacePatch::default()
                },
            }],
        );
        let faces = SessionFaces::new(
            registry,
            FaceEnvironment::new(Some(&ThemeName::new("catppuccin-mocha"))).unwrap(),
        );

        let resolved = faces.resolve(&FaceName::new("plugin.todo.warning"));

        assert_eq!(resolved.bold, FaceValue::Value(true));
        assert_eq!(
            resolved.foreground,
            FaceValue::Value(Color::Rgb {
                red: 0xf9,
                green: 0xe2,
                blue: 0xaf,
            })
        );
    }

    #[test]
    fn remaps_follow_scope_order_and_relative_tokens_are_independent() {
        let mut faces = SessionFaces::default();
        let content = vell_protocol::ids::ContentId(1);
        let view = ViewId(2);
        let face = FaceName::new("syntax.comment");
        let user = FaceRemapOwner::User;
        faces
            .apply_operation(ResolvedFaceOperation::SetBase {
                scope: FaceRemapScope::Content(content),
                face: face.clone(),
                expressions: Some(vec![FaceExpr::Patch(FacePatch {
                    bold: FaceValue::Value(true),
                    ..FacePatch::default()
                })]),
                owner: user,
            })
            .unwrap();
        faces
            .apply_operation(ResolvedFaceOperation::AddRelative {
                scope: FaceRemapScope::View(view),
                face: face.clone(),
                token: FaceRemapToken(10),
                expressions: vec![FaceExpr::Patch(FacePatch {
                    italic: FaceValue::Value(true),
                    ..FacePatch::default()
                })],
                owner: user,
            })
            .unwrap();
        faces
            .apply_operation(ResolvedFaceOperation::AddRelative {
                scope: FaceRemapScope::View(view),
                face: face.clone(),
                token: FaceRemapToken(11),
                expressions: vec![FaceExpr::Patch(FacePatch {
                    underline: FaceValue::Value(true),
                    ..FacePatch::default()
                })],
                owner: user,
            })
            .unwrap();

        let local = faces.resolve_for(&face, content, view);
        let other_view = faces.resolve_for(&face, content, ViewId(3));
        assert_eq!(local.bold, FaceValue::Value(true));
        assert_eq!(local.italic, FaceValue::Value(true));
        assert_eq!(local.underline, FaceValue::Value(true));
        assert_eq!(other_view.bold, FaceValue::Value(true));
        assert_eq!(other_view.underline, FaceValue::Unspecified);

        faces
            .apply_operation(ResolvedFaceOperation::RemoveRelative {
                token: FaceRemapToken(10),
                owner: user,
            })
            .unwrap();
        let after_remove = faces.resolve_for(&face, content, view);
        assert_eq!(after_remove.italic, FaceValue::Unspecified);
        assert_eq!(after_remove.underline, FaceValue::Value(true));
    }

    #[test]
    fn remap_tokens_enforce_ownership_and_scope_cleanup() {
        let mut faces = SessionFaces::default();
        let view = ViewId(5);
        let operation = ResolvedFaceOperation::AddRelative {
            scope: FaceRemapScope::View(view),
            face: FaceName::new("ui.editor"),
            token: FaceRemapToken(42),
            expressions: vec![FaceExpr::Patch(FacePatch::default())],
            owner: FaceRemapOwner::Mode(ModeId::for_test(1)),
        };
        faces.apply_operation(operation).unwrap();

        assert!(
            faces
                .validate_operation(&ResolvedFaceOperation::RemoveRelative {
                    token: FaceRemapToken(42),
                    owner: FaceRemapOwner::Mode(ModeId::for_test(2)),
                })
                .is_err()
        );
        faces.remove_view_remaps(view);
        assert!(
            faces
                .validate_operation(&ResolvedFaceOperation::RemoveRelative {
                    token: FaceRemapToken(42),
                    owner: FaceRemapOwner::Mode(ModeId::for_test(1)),
                })
                .is_err()
        );
    }
}
