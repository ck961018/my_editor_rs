use std::cell::RefCell;
use std::rc::Rc;

use vell_mode::{
    ViewExtension, ViewExtensionContext, ViewExtensionContractError, ViewExtensionDefinition,
    ViewExtensionPresentation,
};

use super::{ScriptHost, ScriptViewExtensionDefinition};

pub(super) struct ScriptViewExtension {
    host: Rc<RefCell<ScriptHost>>,
    definition: ScriptViewExtensionDefinition,
}

impl ScriptViewExtension {
    pub(super) fn new(
        host: Rc<RefCell<ScriptHost>>,
        definition: ScriptViewExtensionDefinition,
    ) -> Self {
        Self { host, definition }
    }
}

impl ViewExtension for ScriptViewExtension {
    fn definition(&self) -> &ViewExtensionDefinition {
        &self.definition.definition
    }

    fn present(
        &mut self,
        pane: &str,
        context: &ViewExtensionContext,
    ) -> Result<ViewExtensionPresentation, ViewExtensionContractError> {
        let callback = self.definition.callbacks.get(pane).ok_or_else(|| {
            ViewExtensionContractError::new(format!(
                "view extension Pane '{pane}' has no render callback"
            ))
        })?;
        self.host
            .borrow_mut()
            .present_view_extension(callback, context)
            .map_err(|error| ViewExtensionContractError::new(error.to_string()))
    }

    fn unload(&mut self) {
        self.host
            .borrow_mut()
            .remove_view_extension(self.definition.definition.id());
    }
}
