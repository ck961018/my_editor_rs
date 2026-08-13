use std::fmt;

pub const MIN_GUTTER_WIDTH: u8 = 1;
pub const MAX_GUTTER_WIDTH: u8 = 16;
pub const DEFAULT_GUTTER_WIDTH: u8 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditorOptions {
    pub buffer_view: BufferViewOptions,
}

impl EditorOptions {
    pub fn validate(self) -> Result<(), EditorOptionsError> {
        let width = self.buffer_view.gutter.width;
        if !(MIN_GUTTER_WIDTH..=MAX_GUTTER_WIDTH).contains(&width) {
            return Err(EditorOptionsError::InvalidGutterWidth(width));
        }
        Ok(())
    }

    pub fn gutter_width(self) -> i32 {
        if self.buffer_view.gutter.visible {
            i32::from(self.buffer_view.gutter.width)
        } else {
            0
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferViewOptions {
    pub gutter: BufferViewGutterOptions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BufferViewGutterOptions {
    pub visible: bool,
    pub width: u8,
}

impl Default for BufferViewGutterOptions {
    fn default() -> Self {
        Self {
            visible: true,
            width: DEFAULT_GUTTER_WIDTH,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorOptionsError {
    InvalidGutterWidth(u8),
}

impl fmt::Display for EditorOptionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGutterWidth(width) => write!(
                formatter,
                "BufferView gutter width {width} is outside {MIN_GUTTER_WIDTH}..={MAX_GUTTER_WIDTH}"
            ),
        }
    }
}

impl std::error::Error for EditorOptionsError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_options_default_to_a_visible_four_cell_gutter() {
        let options = EditorOptions::default();

        assert!(options.buffer_view.gutter.visible);
        assert_eq!(options.buffer_view.gutter.width, 4);
        assert_eq!(options.gutter_width(), 4);
    }

    #[test]
    fn hidden_gutter_keeps_its_configured_width_but_occupies_no_cells() {
        let options = EditorOptions {
            buffer_view: BufferViewOptions {
                gutter: BufferViewGutterOptions {
                    visible: false,
                    width: 6,
                },
            },
        };

        assert_eq!(options.gutter_width(), 0);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn gutter_width_must_be_between_one_and_sixteen() {
        for width in [0, 17] {
            let options = EditorOptions {
                buffer_view: BufferViewOptions {
                    gutter: BufferViewGutterOptions {
                        visible: true,
                        width,
                    },
                },
            };

            assert_eq!(
                options.validate(),
                Err(EditorOptionsError::InvalidGutterWidth(width))
            );
        }
    }
}
