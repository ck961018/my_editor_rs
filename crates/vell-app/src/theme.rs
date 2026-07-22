use std::sync::Arc;

use vell_protocol::content_query::{
    Face, FaceName, FacePatch, PaintFace, ThemeName,
};
use vell_protocol::revision::Revision;
use vell_theme::{ResolvedTheme, ThemeError, ThemeRegistry};

use crate::mode::{FaceConflict, FaceRegistry};

pub(super) struct FaceEnvironment {
    fallback_theme: Arc<ResolvedTheme>,
    active_theme: Arc<ResolvedTheme>,
    revision: Revision,
}

impl FaceEnvironment {
    pub(super) fn new(theme: Option<&ThemeName>) -> Result<Self, ThemeError> {
        let registry = ThemeRegistry::with_builtins()?;
        let fallback_theme = registry.resolve(&ThemeName::new("terminal-default"))?;
        let active_theme = match theme {
            Some(theme) => registry.resolve(theme)?,
            None => fallback_theme.clone(),
        };
        Ok(Self {
            fallback_theme,
            active_theme,
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
        resolved
    }

    pub(super) fn resolve_root(&self, name: &FaceName, legacy: &Face) -> PaintFace {
        self.resolve(name, legacy).resolve(&PaintFace::default())
    }
}

pub(super) struct SessionFaces {
    registry: FaceRegistry,
    environment: FaceEnvironment,
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
        }
    }

    pub(super) fn resolve(&self, name: &FaceName) -> FacePatch {
        self.environment.resolve(name, &self.registry.resolve(name))
    }

    pub(super) fn resolve_root(&self, name: &FaceName) -> PaintFace {
        self.environment
            .resolve_root(name, &self.registry.resolve(name))
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
}
