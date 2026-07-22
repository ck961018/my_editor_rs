use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use vell_protocol::ids::ViewId;
use vell_protocol::content_query::{
    Face, FaceName, FaceOverride, FacePatch, PaintFace, ThemeName,
};
use vell_protocol::revision::Revision;
use vell_theme::{ResolvedTheme, ThemeError, ThemeRegistry};

use crate::mode::{FaceConflict, FaceRegistrationError, FaceRegistry};

pub(super) struct FaceEnvironment {
    fallback_theme: Arc<ResolvedTheme>,
    active_theme: Arc<ResolvedTheme>,
    global_overrides: HashMap<FaceName, FacePatch>,
    theme_overrides: HashMap<ThemeName, HashMap<FaceName, FacePatch>>,
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
        let mut theme_overrides: HashMap<ThemeName, HashMap<FaceName, FacePatch>> =
            HashMap::new();
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

    pub(super) fn resolve(&self, name: &FaceName, legacy: &Face) -> FacePatch {
        let mut resolved = FacePatch::from(legacy);
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
        resolved
    }

    pub(super) fn resolve_root(&self, name: &FaceName, legacy: &Face) -> PaintFace {
        self.resolve(name, legacy).resolve(&PaintFace::default())
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
        }
    }

    pub(super) fn resolve(&self, name: &FaceName) -> FacePatch {
        self.environment.resolve(name, &self.registry.resolve(name))
    }

    pub(super) fn resolve_root(&self, name: &FaceName) -> PaintFace {
        self.environment
            .resolve_root(name, &self.registry.resolve(name))
    }

    pub(super) fn set_active_view(&self, view: Option<ViewId>) {
        self.active_view.set(view);
    }

    pub(super) fn resolve_status_bar_root(&self, target: ViewId) -> PaintFace {
        let name = if self.active_view.get() == Some(target) {
            "ui.status-bar"
        } else {
            "ui.status-bar.inactive"
        };
        self.resolve_root(&FaceName::new(name))
    }

    pub(super) fn provider(
        &self,
        name: &FaceName,
    ) -> Option<&crate::mode_name::ModeName> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use vell_protocol::content_query::{Color, FaceValue};

    #[test]
    fn active_theme_overlays_terminal_fallback_by_attribute() {
        let environment =
            FaceEnvironment::new(Some(&ThemeName::new("catppuccin-mocha"))).unwrap();
        let face = environment.resolve(&FaceName::new("syntax.comment"), &Face::default());
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
    fn status_bar_uses_inactive_face_for_non_focused_target() {
        let environment =
            FaceEnvironment::new(Some(&ThemeName::new("catppuccin-mocha"))).unwrap();
        let faces = SessionFaces::new(FaceRegistry::default(), environment);
        faces.set_active_view(Some(ViewId(1)));

        let active = faces.resolve_status_bar_root(ViewId(1));
        let inactive = faces.resolve_status_bar_root(ViewId(2));

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

        let face = environment.resolve(&FaceName::new("syntax.comment"), &Face::default());

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
}
